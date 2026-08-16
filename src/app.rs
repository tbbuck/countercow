//! Dashboard state: what we know about the target, and the recent history of every counter.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::counters::catalog;
use crate::counters::sample::CounterSample;
use crate::ipc::commands::ProcessInfo;
use crate::ipc::discovery::DotnetProcess;
use crate::runtime::session::RuntimeEvent;
use crate::runtime::state::RuntimeState;

/// Samples retained per counter. At the default one-second interval this is ten minutes.
pub const HISTORY_CAPACITY: usize = 600;

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
}

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Dashboard,
    /// Runtime events: allocations, collections, exceptions, contention.
    Investigate,
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

#[derive(Debug, Clone, Default)]
pub struct Series {
    pub latest: Option<CounterSample>,
    /// Oldest first.
    pub history: Vec<f64>,
}

impl Series {
    fn push(&mut self, sample: CounterSample) {
        self.history.push(sample.value);
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
    pub interval: f64,
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
            series: BTreeMap::new(),
            providers: BTreeSet::new(),
            status: Status::Connecting,
            paused: false,
            show_help: false,
            exit: None,
            view: View::Dashboard,
            runtime: RuntimeState::new(),
            runtime_error: None,
            investigating_since: None,
            started: Instant::now(),
            last_sample_at: None,
            samples_seen: 0,
        }
    }

    /// Record a sample. Pausing freezes the graphs but keeps the session running, so history is
    /// continuous when you unpause rather than showing a misleading gap.
    pub fn record(&mut self, sample: CounterSample) {
        self.status = Status::Live;
        self.samples_seen += 1;
        self.last_sample_at = Some(Instant::now());
        self.providers.insert(sample.provider.clone());

        if self.paused {
            return;
        }
        let key = (sample.provider.clone(), sample.name.clone());
        self.series.entry(key).or_default().push(sample);
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

    pub fn history(&self, provider: &str, name: &str) -> &[f64] {
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
            View::Dashboard => {
                self.runtime_error = None;
                self.investigating_since = Some(Instant::now());
                View::Investigate
            }
            View::Investigate => View::Dashboard,
        };
    }

    pub fn is_investigating(&self) -> bool {
        self.view == View::Investigate
    }

    pub fn investigating_for(&self) -> Duration {
        self.investigating_since.map(|at| at.elapsed()).unwrap_or_default()
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
        assert_eq!(app.history(catalog::SYSTEM_RUNTIME, "cpu-usage"), &[1.0, 2.0, 3.0]);
        assert_eq!(app.value(catalog::SYSTEM_RUNTIME, "cpu-usage"), Some(3.0));
        assert_eq!(app.status, Status::Live);
    }

    #[test]
    fn history_is_bounded_and_drops_oldest_first() {
        let mut app = app();
        for i in 0..HISTORY_CAPACITY + 50 {
            app.record(sample(catalog::SYSTEM_RUNTIME, "cpu-usage", i as f64));
        }
        let history = app.history(catalog::SYSTEM_RUNTIME, "cpu-usage");
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

        assert_eq!(app.history(catalog::SYSTEM_RUNTIME, "cpu-usage"), &[1.0]);
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
    fn uptime_formats_by_magnitude() {
        assert_eq!(format_uptime(Duration::from_secs(9)), "9s");
        assert_eq!(format_uptime(Duration::from_secs(75)), "1m15s");
        assert_eq!(format_uptime(Duration::from_secs(3_725)), "1h02m");
    }
}
