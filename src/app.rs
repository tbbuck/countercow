//! Dashboard state: what we know about the target, and the recent history of every counter.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::counters::catalog;
use crate::counters::sample::CounterSample;
use crate::ipc::commands::ProcessInfo;
use crate::ipc::discovery::DotnetProcess;
use crate::profile::run::ProfileResult;
use crate::profile::state::HotProfile;
use crate::runtime::session::RuntimeEvent;
use crate::runtime::state::RuntimeState;

/// Samples retained per counter. At the default one-second interval this is ten minutes.
pub const HISTORY_CAPACITY: usize = 600;

/// How much one press of `-` or `+` moves the refresh rate, and the range it moves within.
///
/// Faster than 100 ms and the counters are mostly measuring the runtime's own timer; slower than
/// ten seconds and the graphs stop reading as live. Same range and step as btop's update timer.
pub const INTERVAL_STEP: f64 = 0.1;
pub const MIN_INTERVAL: f64 = 0.1;
pub const MAX_INTERVAL: f64 = 10.0;

/// How long a rate must stand still before the session is reopened at it.
///
/// A step is 100 ms, so getting from one second to two is ten presses. Restarting the EventPipe
/// session on each of them would open and close ten sessions against the target in about as many
/// frames; waiting for the keypresses to stop collapses that into one.
const INTERVAL_SETTLE: Duration = Duration::from_millis(400);

/// Default CPU profile window. Long enough for a stable picture at ~13,000 samples/second,
/// short enough not to feel like a wait.
pub const DEFAULT_PROFILE_SECONDS: u64 = 5;

/// Hot methods retained from a profile — more than fits on screen, so the filter can be toggled
/// without losing the tail.
const PROFILE_ROWS: usize = 200;

/// Identifies a counter across providers, since names are only unique within one.
pub type CounterKey = (String, String);

/// Messages the UI thread reacts to.
#[derive(Debug)]
pub enum AppEvent {
    Input(crossterm::event::Event),
    Sample(Box<CounterSample>),
    /// The counter session finished; `Some` carries the failure that ended it.
    SessionEnded(Option<String>),
    /// A decoded runtime event, with the clock frequency needed to scale pause durations.
    Runtime(Box<RuntimeEvent>, i64),
    /// The investigation session could not start, or ended unexpectedly.
    RuntimeFailed(String),
    /// Samples collected so far in the current profile.
    ProfileProgress(u64),
    /// A finished profile: raw samples plus the method table to rank them against.
    ProfileDone(Box<ProfileResult>),
    ProfileFailed(String),
}

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    /// Runtime events: allocations, collections, exceptions, contention.
    Investigate,
    /// A fixed-window CPU profile.
    Profile,
}

/// Where a CPU profile has got to. It is not a live view: names only exist once the session ends.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfilePhase {
    Collecting { until: Instant },
    /// The window is up and the rundown is being read.
    Resolving,
    Done,
    Failed(String),
}

/// How a dashboard session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Leave countercow entirely.
    Quit,
    /// Go back to the process picker to attach to something else.
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Attached, but no counters have arrived yet.
    Connecting,
    Live,
    /// The session ended cleanly — usually the target process exited.
    Ended,
    Failed(String),
}

/// One counter reading, and when it arrived.
///
/// The arrival time is kept because the refresh rate can change underneath a series: without it
/// the only way to say how far back a graph reaches is to multiply the sample count by the current
/// interval, which is wrong for every sample gathered at the previous one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    pub at: Instant,
    pub value: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Series {
    pub latest: Option<CounterSample>,
    /// Oldest first.
    pub history: Vec<Reading>,
}

impl Series {
    fn push(&mut self, sample: CounterSample, at: Instant) {
        self.history.push(Reading { at, value: sample.value });
        if self.history.len() > HISTORY_CAPACITY {
            let overflow = self.history.len() - HISTORY_CAPACITY;
            self.history.drain(..overflow);
        }
        self.latest = Some(sample);
    }
}

pub struct App {
    pub process: DotnetProcess,
    pub info: ProcessInfo,
    /// The rate the live session is actually running at.
    pub interval: f64,
    /// A rate the user has asked for and when they last asked, until the session has been
    /// restarted at it.
    pending_interval: Option<(f64, Instant)>,
    series: BTreeMap<CounterKey, Series>,
    providers: BTreeSet<String>,
    pub status: Status,
    pub paused: bool,
    pub show_help: bool,
    /// Set once the user asks to leave this dashboard, and how.
    pub exit: Option<Exit>,
    pub view: View,
    /// Accumulated runtime events. Only populated while investigating.
    pub runtime: RuntimeState,
    pub runtime_error: Option<String>,
    pub profile_phase: ProfilePhase,
    /// The ranked result currently on screen.
    pub profile: Option<HotProfile>,
    /// The raw samples and method table behind it, kept so the ranking can be recomputed when
    /// the parked-thread filter is toggled — no need to profile again.
    profile_result: Option<ProfileResult>,
    /// Samples seen so far, for the progress display.
    pub profile_samples: u64,
    /// Whether parked threads are included in the ranking.
    pub profile_show_waiting: bool,
    /// How long each profile runs. Fixed rather than open-ended, because the result only exists
    /// once the window closes.
    pub profile_seconds: u64,
    /// When the current investigation session began.
    investigating_since: Option<Instant>,
    started: Instant,
    pub last_sample_at: Option<Instant>,
    /// Counts every sample received, so the footer can show the session is alive.
    pub samples_seen: u64,
}

impl App {
    pub fn new(process: DotnetProcess, info: ProcessInfo, interval: f64) -> Self {
        Self {
            process,
            info,
            interval,
            pending_interval: None,
            series: BTreeMap::new(),
            providers: BTreeSet::new(),
            status: Status::Connecting,
            paused: false,
            show_help: false,
            exit: None,
            view: View::Dashboard,
            runtime: RuntimeState::new(),
            runtime_error: None,
            profile_phase: ProfilePhase::Resolving,
            profile: None,
            profile_result: None,
            profile_samples: 0,
            profile_show_waiting: false,
            profile_seconds: DEFAULT_PROFILE_SECONDS,
            investigating_since: None,
            started: Instant::now(),
            last_sample_at: None,
            samples_seen: 0,
        }
    }

    /// Record a sample. Pausing freezes the graphs but keeps the session running, so history is
    /// continuous when you unpause rather than showing a misleading gap.
    pub fn record(&mut self, sample: CounterSample) {
        self.record_at(sample, Instant::now());
    }

    /// [`Self::record`] with the arrival time supplied, for replaying a capture at the cadence it
    /// was gathered at rather than as fast as it parses.
    pub fn record_at(&mut self, sample: CounterSample, now: Instant) {
        self.status = Status::Live;
        self.samples_seen += 1;
        self.last_sample_at = Some(now);
        self.providers.insert(sample.provider.clone());

        if self.paused {
            return;
        }
        let key = (sample.provider.clone(), sample.name.clone());
        self.series.entry(key).or_default().push(sample, now);
    }

    /// Move the refresh rate one step. `faster` shortens the interval.
    ///
    /// This only records the request. The rate is fixed when the counter session is created — the
    /// runtime is told it as provider filter data — so applying it means opening a new session,
    /// which is the event loop's job once the rate has stopped moving.
    pub fn step_interval(&mut self, faster: bool) {
        let from = self.wanted_interval();
        let delta = if faster { -INTERVAL_STEP } else { INTERVAL_STEP };

        // Rounded back onto the step. A tenth has no exact binary form, so ten unrounded presses
        // land on 0.9999999999999999, which reaches the runtime as a filter string to match and
        // the corner of the screen as a rate nobody asked for.
        let next = (((from + delta) * 10.0).round() / 10.0).clamp(MIN_INTERVAL, MAX_INTERVAL);

        // Clamping can land the wrong side of where we started — stepping down from an interval
        // above the range would otherwise answer a request to slow down by speeding up.
        if next != from && faster == (next < from) {
            self.pending_interval = Some((next, Instant::now()));
        }
    }

    /// The rate the user has asked for, whether or not the session is running at it yet.
    pub fn wanted_interval(&self) -> f64 {
        self.pending_interval.map_or(self.interval, |(interval, _)| interval)
    }

    /// The rate the session should be restarted at, once the keypresses have stopped.
    pub fn take_pending_interval(&mut self) -> Option<f64> {
        self.take_settled_interval(Instant::now())
    }

    /// [`Self::take_pending_interval`] against a given clock, so the settle window can be tested
    /// without sleeping through it.
    fn take_settled_interval(&mut self, now: Instant) -> Option<f64> {
        let (interval, asked_at) = self.pending_interval?;
        if now.saturating_duration_since(asked_at) < INTERVAL_SETTLE {
            return None;
        }
        self.pending_interval = None;
        Some(interval)
    }

    /// Adopt a rate the session is now running at.
    ///
    /// History is deliberately kept. Every reading carries the moment it arrived, so a graph can
    /// say exactly how far back it reaches whatever cadence produced it; throwing the history away
    /// instead would blank every chart on each press, and at a slow rate it then refills one
    /// sub-column per interval — which is indistinguishable from the display having frozen.
    pub fn apply_interval(&mut self, interval: f64) {
        self.interval = interval;
    }

    pub fn series(&self, provider: &str, name: &str) -> Option<&Series> {
        self.series.get(&(provider.to_owned(), name.to_owned()))
    }

    pub fn latest(&self, provider: &str, name: &str) -> Option<&CounterSample> {
        self.series(provider, name)?.latest.as_ref()
    }

    pub fn value(&self, provider: &str, name: &str) -> Option<f64> {
        Some(self.latest(provider, name)?.value)
    }

    pub fn history(&self, provider: &str, name: &str) -> &[Reading] {
        self.series(provider, name).map_or(&[], |s| &s.history)
    }

    /// Formatted value, or a placeholder when the counter has not reported.
    pub fn display(&self, provider: &str, name: &str) -> String {
        self.latest(provider, name)
            .map(catalog::format_sample)
            .unwrap_or_else(|| "—".into())
    }

    pub fn has_provider(&self, provider: &str) -> bool {
        self.providers.contains(provider)
    }

    /// Whether to show the ASP.NET panels.
    ///
    /// There is no "is ASP.NET" flag anywhere in the protocol. Subscribing to a provider that no
    /// EventSource implements succeeds and silently yields nothing, so the arrival of events is
    /// the signal. The hosting counters are polling counters, so they report even on an idle app.
    pub fn is_aspnet(&self) -> bool {
        self.has_provider(catalog::ASPNET_HOSTING) || self.has_provider(catalog::KESTREL)
    }

    pub fn providers(&self) -> &BTreeSet<String> {
        &self.providers
    }

    pub fn counter_count(&self) -> usize {
        self.series.len()
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// Enter or leave the investigation screen.
    ///
    /// Leaving keeps what was gathered, so flicking back and forth does not throw the findings
    /// away; the session itself is stopped by the event loop, which is what actually costs the
    /// target process.
    pub fn toggle_investigate(&mut self) {
        self.view = match self.view {
            View::Investigate => View::Dashboard,
            // From anywhere else, including a finished profile, `i` opens the investigation.
            View::Dashboard | View::Profile => {
                self.runtime_error = None;
                self.investigating_since = Some(Instant::now());
                View::Investigate
            }
        };
    }

    pub fn is_investigating(&self) -> bool {
        self.view == View::Investigate
    }

    pub fn investigating_for(&self) -> Duration {
        self.investigating_since.map(|at| at.elapsed()).unwrap_or_default()
    }

    /// Enter the profile screen and begin a collection window.
    pub fn start_profile(&mut self) {
        self.view = View::Profile;
        self.profile = None;
        self.profile_samples = 0;
        self.profile_phase = ProfilePhase::Collecting {
            until: Instant::now() + Duration::from_secs(self.profile_seconds),
        };
    }

    pub fn is_profiling(&self) -> bool {
        self.view == View::Profile
    }

    pub fn profile_collecting(&self) -> bool {
        matches!(self.profile_phase, ProfilePhase::Collecting { .. })
    }

    /// Show or hide parked threads, re-ranking what is already collected.
    ///
    /// No new session is needed: the samples are kept, only the filter changes.
    pub fn toggle_profile_waiting(&mut self) {
        self.profile_show_waiting = !self.profile_show_waiting;
        self.rank_profile();
    }

    /// Store a finished profile and rank it.
    pub fn finish_profile(&mut self, result: ProfileResult) {
        self.profile_result = Some(result);
        self.profile_phase = ProfilePhase::Done;
        self.rank_profile();
    }

    fn rank_profile(&mut self) {
        self.profile = self.profile_result.as_ref().map(|result| {
            result.state.hot_methods(&result.methods, PROFILE_ROWS, self.profile_show_waiting)
        });
    }

    /// Leave the profile screen, abandoning any collection in flight.
    pub fn cancel_profile(&mut self) {
        if self.profile_collecting() {
            self.profile_phase = ProfilePhase::Failed("cancelled".into());
        }
        self.view = View::Dashboard;
    }

    /// Whether the collection window has elapsed and the session should be stopped.
    pub fn profile_window_elapsed(&self) -> bool {
        match self.profile_phase {
            ProfilePhase::Collecting { until } => Instant::now() >= until,
            _ => false,
        }
    }

    /// Fraction of the collection window elapsed, for the progress bar.
    pub fn profile_progress(&self) -> f64 {
        match self.profile_phase {
            ProfilePhase::Collecting { until } => {
                let remaining = until.saturating_duration_since(Instant::now()).as_secs_f64();
                let total = self.profile_seconds as f64;
                if total <= 0.0 {
                    1.0
                } else {
                    (total - remaining) / total
                }
            }
            _ => 1.0,
        }
    }

    pub fn profile_remaining_secs(&self) -> f64 {
        match self.profile_phase {
            ProfilePhase::Collecting { until } => {
                until.saturating_duration_since(Instant::now()).as_secs_f64()
            }
            _ => 0.0,
        }
    }

    /// Fold a decoded runtime event into the investigation state.
    pub fn record_runtime(&mut self, event: RuntimeEvent, qpc_frequency: i64) {
        match event {
            RuntimeEvent::Allocation(tick) => {
                self.runtime.record_allocation(tick.type_name, tick.amount, tick.kind)
            }
            RuntimeEvent::GcStart(start) => self.runtime.record_gc_start(start),
            RuntimeEvent::GcEnd => self.runtime.record_gc_end(),
            RuntimeEvent::HeapStats(stats) => self.runtime.record_heap_stats(stats),
            RuntimeEvent::SuspendBegin { timestamp } => self.runtime.record_suspend(timestamp),
            RuntimeEvent::RestartEnd { timestamp } => {
                self.runtime.record_restart(timestamp, qpc_frequency)
            }
            RuntimeEvent::Exception(thrown) => self.runtime.record_exception(thrown),
            RuntimeEvent::Contention(stop) => self.runtime.record_contention(stop),
        }
    }

    /// True when samples have stopped arriving for noticeably longer than the interval.
    pub fn is_stalled(&self) -> bool {
        match self.last_sample_at {
            Some(at) => at.elapsed().as_secs_f64() > self.interval * 3.0,
            None => self.uptime().as_secs_f64() > self.interval * 3.0,
        }
    }
}

/// Format a duration as the header shows it.
pub fn format_uptime(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counters::sample::CounterKind;
    use std::path::PathBuf;

    fn app() -> App {
        let process = DotnetProcess {
            pid: 1,
            socket: PathBuf::from("/tmp/socket"),
            name: "Test".into(),
            command: "test".into(),
            start_key_verified: true,
        };
        App::new(process, ProcessInfo::default(), 1.0)
    }

    /// An app started at an interval that need not sit on the step, as `--interval` allows.
    fn app_at(interval: f64) -> App {
        let process = DotnetProcess {
            pid: 1,
            socket: PathBuf::from("/tmp/socket"),
            name: "Test".into(),
            command: "test".into(),
            start_key_verified: true,
        };
        App::new(process, ProcessInfo::default(), interval)
    }

    /// A counter's history as plain numbers.
    fn values(app: &App, name: &str) -> Vec<f64> {
        app.history(catalog::SYSTEM_RUNTIME, name).iter().map(|r| r.value).collect()
    }

    fn sample(provider: &str, name: &str, value: f64) -> CounterSample {
        CounterSample {
            provider: provider.into(),
            name: name.into(),
            display_name: name.into(),
            display_units: String::new(),
            value,
            kind: CounterKind::Mean,
            interval_sec: 1.0,
            rate_time_scale: None,
            timestamp: 0,
        }
    }

    #[test]
    fn records_history_in_arrival_order() {
        let mut app = app();
        for v in [1.0, 2.0, 3.0] {
            app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", v));
        }
        assert_eq!(values(&app, "cpu-usage"), vec![1.0, 2.0, 3.0]);
        assert_eq!(app.value(catalog::SYSTEM_RUNTIME, "cpu-usage"), Some(3.0));
        assert_eq!(app.status, Status::Live);
    }

    #[test]
    fn history_is_bounded_and_drops_oldest_first() {
        let mut app = app();
        for i in 0..HISTORY_CAPACITY + 50 {
            app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", i as f64));
        }
        let history = values(&app, "cpu-usage");
        assert_eq!(history.len(), HISTORY_CAPACITY);
        assert_eq!(history[0], 50.0, "oldest samples are discarded");
        assert_eq!(history[history.len() - 1], (HISTORY_CAPACITY + 49) as f64);
    }

    #[test]
    fn counters_from_different_providers_do_not_collide() {
        // "current-requests" exists on both Hosting and System.Net.Http.
        let mut app = app();
        app.record(sample(catalog::ASPNET_HOSTING, "current-requests", 5.0));
        app.record(sample(catalog::NET_HTTP, "current-requests", 9.0));

        assert_eq!(app.value(catalog::ASPNET_HOSTING, "current-requests"), Some(5.0));
        assert_eq!(app.value(catalog::NET_HTTP, "current-requests"), Some(9.0));
        assert_eq!(app.counter_count(), 2);
    }

    #[test]
    fn pausing_freezes_history_but_keeps_the_session_live() {
        let mut app = app();
        app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", 1.0));
        app.paused = true;
        app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", 2.0));

        assert_eq!(values(&app, "cpu-usage"), vec![1.0]);
        assert_eq!(app.samples_seen, 2, "still counting arrivals");
        assert!(app.last_sample_at.is_some());
    }

    #[test]
    fn aspnet_is_detected_from_arriving_events_only() {
        let mut app = app();
        app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", 1.0));
        assert!(!app.is_aspnet(), "runtime counters alone prove nothing");

        app.record(sample(catalog::ASPNET_HOSTING, "total-requests", 0.0));
        assert!(app.is_aspnet());
    }

    #[test]
    fn kestrel_alone_also_identifies_an_aspnet_host() {
        let mut app = app();
        app.record(sample(catalog::KESTREL, "total-connections", 0.0));
        assert!(app.is_aspnet());
    }

    #[test]
    fn missing_counters_display_a_placeholder() {
        let app = app();
        assert_eq!(app.display(catalog::SYSTEM_RUNTIME, "cpu-usage"), "—");
        assert!(app.history(catalog::SYSTEM_RUNTIME, "cpu-usage").is_empty());
    }

    #[test]
    fn the_rate_steps_by_a_tenth_of_a_second_each_way() {
        let mut faster = app();
        assert_eq!(faster.interval, 1.0);

        faster.step_interval(true);
        assert_eq!(faster.wanted_interval(), 0.9, "- is faster");

        let mut slower = app();
        slower.step_interval(false);
        assert_eq!(slower.wanted_interval(), 1.1, "+ is slower");
    }

    #[test]
    fn repeated_presses_step_from_the_wanted_rate_not_the_live_one() {
        let mut app = app();
        for _ in 0..5 {
            app.step_interval(true);
        }
        assert_eq!(app.wanted_interval(), 0.5, "five steps below one second");
        assert_eq!(app.interval, 1.0, "the session has not been restarted yet");
    }

    #[test]
    fn stepping_never_drifts_off_the_tenth() {
        // A tenth is not exact in binary, so ten unrounded presses would land just short of one.
        let mut app = app();
        for _ in 0..9 {
            app.step_interval(true);
        }
        assert_eq!(app.wanted_interval(), MIN_INTERVAL);
        for _ in 0..9 {
            app.step_interval(false);
        }
        assert_eq!(app.wanted_interval(), 1.0, "back exactly where it started");
    }

    #[test]
    fn the_range_stops_at_both_ends_without_asking_for_a_restart() {
        let mut fastest = app_at(MIN_INTERVAL);
        fastest.step_interval(true);
        assert_eq!(fastest.take_pending_interval(), None, "already at the fastest rate");

        let mut slowest = app_at(MAX_INTERVAL);
        slowest.step_interval(false);
        assert_eq!(slowest.take_pending_interval(), None, "already at the slowest rate");
    }

    #[test]
    fn stepping_always_moves_in_the_direction_asked_for() {
        // From outside the range, clamping alone would answer "slower" by speeding up.
        let mut above = app_at(30.0);
        above.step_interval(false);
        assert_eq!(above.wanted_interval(), 30.0, "nothing slower to reach");
        above.step_interval(true);
        assert_eq!(above.wanted_interval(), MAX_INTERVAL, "and back into range");
    }

    #[test]
    fn an_off_step_interval_snaps_onto_the_step() {
        let mut app = app_at(0.73);
        app.step_interval(false);
        assert_eq!(app.wanted_interval(), 0.8);
    }

    #[test]
    fn a_rate_is_not_applied_until_the_keypresses_stop() {
        // Ten presses to get from one second to two would otherwise open ten sessions.
        let mut app = app();
        app.step_interval(true);
        assert_eq!(app.take_pending_interval(), None, "still being pressed");

        let settled = Instant::now() + INTERVAL_SETTLE;
        assert_eq!(app.take_settled_interval(settled), Some(0.9));
        assert_eq!(app.take_settled_interval(settled), None, "taken only once");
    }

    #[test]
    fn a_further_press_restarts_the_settle_window() {
        let mut app = app();
        app.step_interval(true);
        let nearly = Instant::now() + INTERVAL_SETTLE - Duration::from_millis(1);
        app.step_interval(true);

        assert_eq!(app.take_settled_interval(nearly), None, "the second press reset the wait");
        assert_eq!(app.wanted_interval(), 0.8);
    }

    #[test]
    fn applying_a_rate_keeps_the_history_gathered_at_the_old_one() {
        // Clearing it would blank every chart on each press, and at a slow rate the refill is
        // slow enough to be indistinguishable from the display having stopped.
        let mut app = app();
        for v in [1.0, 2.0, 3.0] {
            app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", v));
        }

        app.apply_interval(0.5);
        assert_eq!(app.interval, 0.5);
        assert_eq!(values(&app, "cpu-usage"), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn readings_are_stamped_with_their_arrival() {
        let mut app = app();
        app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", 1.0));
        app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", 2.0));

        let history = app.history(catalog::SYSTEM_RUNTIME, "cpu-usage");
        assert!(history[1].at >= history[0].at, "arrival order is recorded, not assumed");
    }

    #[test]
    fn uptime_formats_by_magnitude() {
        assert_eq!(format_uptime(Duration::from_secs(9)), "9s");
        assert_eq!(format_uptime(Duration::from_secs(75)), "1m15s");
        assert_eq!(format_uptime(Duration::from_secs(3_725)), "1h02m");
    }
}
