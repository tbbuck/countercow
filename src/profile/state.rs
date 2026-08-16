//! Sample aggregation.
//!
//! Samples are counted per instruction pointer as they stream in, never stored as stacks: at
//! ~13,000 samples/second, keeping the stacks would cost far more memory than keeping the answer.
//! Names are attached at the end, once the rundown provides them.

use std::collections::{HashMap, HashSet};

use super::methods::MethodTable;

/// Label for samples whose address belongs to no jitted method — the runtime itself, the GC, the
/// OS. A large share of any real profile, and honest to name.
pub const NATIVE_LABEL: &str = "(native / runtime code)";

/// Leaf frames that mean a thread was parked, not working.
///
/// The sampling profiler measures *thread* time, not CPU time: it samples every thread on every
/// tick, including ones asleep in a wait. Left unfiltered, a hot-method list is topped by whatever
/// thread idles the longest, which is true and useless.
///
/// The runtime does tag each sample with a type that should distinguish this, but on macOS/arm64
/// every sample reports `External` — measured across 27,000 samples of a process under real load —
/// so the distinction has to be recovered from the stack instead. These are the runtime's blocking
/// primitives, matched on the innermost frame only. `w` in the UI shows them anyway.
const WAIT_FRAMES: &[&str] = &[
    "System.Threading.Monitor.Wait",
    "System.Threading.ManualResetEventSlim.Wait",
    "System.Threading.WaitHandle.WaitOneNoCheck",
    "System.Threading.LowLevelLifoSemaphore.Wait",
    "System.Threading.LowLevelLifoSemaphore.WaitForSignal",
    "System.Threading.LowLevelMonitor.Wait",
    "System.Threading.SemaphoreSlim.WaitUntilCountOrTimeout",
    "System.Threading.Thread.Sleep",
    "System.Threading.Thread.SleepInternal",
    "System.Threading.PortableThreadPool+WorkerThread.WorkerThreadStart",
    "System.Threading.PortableThreadPool+GateThread.GateThreadStart",
    "System.IO.FileSystemWatcher+RunningInstance+StaticWatcherRunLoopManager.WatchForFileSystemEventsThreadStart",
    "Microsoft.Extensions.Logging.Console.ConsoleLoggerProcessor.TryDequeue",
    "System.Net.Sockets.SocketAsyncEngine.EventLoop",
];

/// Whether a leaf frame means the thread was parked rather than working.
///
/// Note that spin waits are deliberately absent: a spinning thread really is burning CPU, and
/// seeing that is the point.
pub fn is_wait_frame(method_name: &str) -> bool {
    WAIT_FRAMES.contains(&method_name)
}

/// One row of the result: a method and how much time was spent in it.
#[derive(Debug, Clone)]
pub struct HotMethod {
    pub name: String,
    /// Samples where this method was the innermost frame — time spent *in* it.
    pub self_samples: u64,
    /// Samples where it appeared anywhere on the stack — time spent in it or its callees.
    pub total_samples: u64,
    pub self_percent: f64,
    pub total_percent: f64,
}

/// A resolved profile: the ranked methods plus how the samples were split.
#[derive(Debug, Default, Clone)]
pub struct HotProfile {
    pub rows: Vec<HotMethod>,
    /// Samples attributed to methods doing work — the denominator for the percentages.
    pub working_samples: u64,
    /// Samples where the innermost frame was a blocking primitive.
    pub waiting_samples: u64,
}

impl HotProfile {
    /// Share of attributable samples that found a thread parked.
    pub fn waiting_percent(&self) -> f64 {
        let total = self.working_samples + self.waiting_samples;
        if total == 0 {
            0.0
        } else {
            self.waiting_samples as f64 / total as f64 * 100.0
        }
    }
}

#[derive(Debug, Default)]
pub struct ProfileState {
    /// Innermost-frame counts, keyed by address.
    self_counts: HashMap<u64, u64>,
    /// Counts of stacks containing an address anywhere, keyed by address.
    total_counts: HashMap<u64, u64>,
    pub samples: u64,
    /// Samples whose stack could not be found — a stack id defined outside the window we kept.
    pub unresolved_stacks: u64,
    /// Samples whose stack was empty.
    pub empty_stacks: u64,
}

impl ProfileState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one sample. `frames` is innermost-first.
    pub fn record_stack(&mut self, frames: &[u64]) {
        self.samples += 1;

        let Some(leaf) = frames.first() else {
            self.empty_stacks += 1;
            return;
        };
        *self.self_counts.entry(*leaf).or_default() += 1;

        // Count each address once per stack, so recursion does not inflate the total.
        let mut seen = HashSet::with_capacity(frames.len());
        for frame in frames {
            if seen.insert(*frame) {
                *self.total_counts.entry(*frame).or_default() += 1;
            }
        }
    }

    /// A sample whose stack we could not resolve.
    pub fn record_missing_stack(&mut self) {
        self.samples += 1;
        self.unresolved_stacks += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.samples == 0
    }

    /// Samples that contributed usable frames.
    pub fn usable_samples(&self) -> u64 {
        self.samples
            .saturating_sub(self.unresolved_stacks)
            .saturating_sub(self.empty_stacks)
    }

    /// Resolve addresses to methods and rank them by self time.
    ///
    /// Ranked by self rather than total, because a flat list sorted by total is dominated by
    /// whatever sits at the bottom of every stack — true but useless.
    ///
    /// Classification happens here rather than at record time because it needs method names,
    /// which only exist once the rundown has landed.
    pub fn hot_methods(
        &self,
        methods: &MethodTable,
        limit: usize,
        include_waiting: bool,
    ) -> HotProfile {
        let mut self_by_name: HashMap<String, u64> = HashMap::new();
        let mut total_by_name: HashMap<String, u64> = HashMap::new();
        let mut waiting_samples = 0;

        for (address, count) in &self.self_counts {
            let name = Self::label_for(*address, methods);
            if !include_waiting && is_wait_frame(&name) {
                waiting_samples += count;
                continue;
            }
            *self_by_name.entry(name).or_default() += count;
        }
        for (address, count) in &self.total_counts {
            let name = Self::label_for(*address, methods);
            if !include_waiting && is_wait_frame(&name) {
                continue;
            }
            *total_by_name.entry(name).or_default() += count;
        }

        // Both percentages are shares of every usable sample, not of the filtered subset. Using
        // the working count for `total` would let a method that sits on every thread's stack
        // report several hundred percent, which is arithmetically defensible and useless.
        let working: u64 = self_by_name.values().sum();
        let denominator = self.usable_samples().max(1) as f64;

        let mut rows: Vec<HotMethod> = self_by_name
            .keys()
            .chain(total_by_name.keys())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .map(|name| {
                let self_samples = self_by_name.get(name).copied().unwrap_or(0);
                let total_samples = total_by_name.get(name).copied().unwrap_or(0);
                HotMethod {
                    name: name.clone(),
                    self_samples,
                    total_samples,
                    self_percent: self_samples as f64 / denominator * 100.0,
                    total_percent: total_samples as f64 / denominator * 100.0,
                }
            })
            .collect();

        rows.sort_by(|a, b| {
            b.self_samples
                .cmp(&a.self_samples)
                .then_with(|| b.total_samples.cmp(&a.total_samples))
                .then_with(|| a.name.cmp(&b.name))
        });
        rows.truncate(limit);

        HotProfile { rows, working_samples: working, waiting_samples }
    }

    fn label_for(address: u64, methods: &MethodTable) -> String {
        methods
            .resolve(address)
            .map(|m| m.qualified_name())
            .unwrap_or_else(|| NATIVE_LABEL.to_owned())
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::methods::Method;

    fn table() -> MethodTable {
        let mut table = MethodTable::new();
        for (start, name) in [(1000u64, "Work"), (2000, "Helper"), (3000, "Main")] {
            table.insert(Method {
                start_address: start,
                size: 100,
                namespace: "App".into(),
                name: name.into(),
                signature: "()".into(),
            });
        }
        table.finish();
        table
    }

    #[test]
    fn the_innermost_frame_gets_the_self_time() {
        let mut state = ProfileState::new();
        // Main -> Helper -> Work, innermost first.
        for _ in 0..10 {
            state.record_stack(&[1050, 2050, 3050]);
        }

        let hot = state.hot_methods(&table(), 10, false).rows;
        assert_eq!(hot[0].name, "App.Work");
        assert_eq!(hot[0].self_samples, 10);
        assert_eq!(hot[0].self_percent, 100.0);

        // The callers get total time but no self time.
        let main = hot.iter().find(|m| m.name == "App.Main").unwrap();
        assert_eq!(main.self_samples, 0);
        assert_eq!(main.total_samples, 10);
        assert_eq!(main.total_percent, 100.0);
    }

    #[test]
    fn ranking_is_by_self_time_not_total() {
        // Main is on every stack but does no work itself; it must not top the list.
        let mut state = ProfileState::new();
        for _ in 0..5 {
            state.record_stack(&[1050, 3050]);
        }
        for _ in 0..3 {
            state.record_stack(&[2050, 3050]);
        }

        let hot = state.hot_methods(&table(), 10, false).rows;
        assert_eq!(hot[0].name, "App.Work", "5 self samples");
        assert_eq!(hot[1].name, "App.Helper", "3 self samples");
        assert_eq!(hot[2].name, "App.Main", "8 total, 0 self");
    }

    #[test]
    fn recursion_counts_once_per_stack() {
        // Work calling itself should not report 300% total.
        let mut state = ProfileState::new();
        state.record_stack(&[1050, 1050, 1050, 3050]);

        let hot = state.hot_methods(&table(), 10, false).rows;
        let work = hot.iter().find(|m| m.name == "App.Work").unwrap();
        assert_eq!(work.total_samples, 1);
        assert_eq!(work.total_percent, 100.0);
    }

    #[test]
    fn frames_in_the_same_method_aggregate() {
        // Two different addresses inside one method are one method.
        let mut state = ProfileState::new();
        state.record_stack(&[1010]);
        state.record_stack(&[1090]);

        let hot = state.hot_methods(&table(), 10, false).rows;
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].name, "App.Work");
        assert_eq!(hot[0].self_samples, 2);
    }

    #[test]
    fn unresolvable_addresses_are_labelled_not_dropped() {
        let mut state = ProfileState::new();
        for _ in 0..4 {
            state.record_stack(&[999_999]);
        }
        state.record_stack(&[1050]);

        let hot = state.hot_methods(&table(), 10, false).rows;
        let native = hot.iter().find(|m| m.name == NATIVE_LABEL).expect("native bucket");
        assert_eq!(native.self_samples, 4);
        assert_eq!(native.self_percent, 80.0);
    }

    #[test]
    fn missing_and_empty_stacks_are_counted_but_not_charged_to_anyone() {
        let mut state = ProfileState::new();
        state.record_stack(&[1050]);
        state.record_missing_stack();
        state.record_stack(&[]);

        assert_eq!(state.samples, 3);
        assert_eq!(state.unresolved_stacks, 1);
        assert_eq!(state.empty_stacks, 1);
        assert_eq!(state.usable_samples(), 1);

        // The one usable sample is 100% of what could be attributed.
        let hot = state.hot_methods(&table(), 10, false).rows;
        assert_eq!(hot[0].self_percent, 100.0);
    }

    #[test]
    fn a_profile_with_no_usable_samples_does_not_divide_by_zero() {
        let mut state = ProfileState::new();
        state.record_missing_stack();
        assert_eq!(state.usable_samples(), 0);
        assert!(state.hot_methods(&table(), 10, false).rows.is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        let mut state = ProfileState::new();
        state.record_stack(&[1050]);
        state.record_stack(&[2050]);
        state.record_stack(&[3050]);
        assert_eq!(state.hot_methods(&table(), 2, false).rows.len(), 2);
    }

    /// A table containing one real method and one blocking primitive.
    fn table_with_wait() -> MethodTable {
        let mut table = MethodTable::new();
        table.insert(Method {
            start_address: 1000,
            size: 100,
            namespace: "App".into(),
            name: "Work".into(),
            signature: "()".into(),
        });
        table.insert(Method {
            start_address: 4000,
            size: 100,
            namespace: "System.Threading".into(),
            name: "Monitor.Wait".into(),
            signature: "()".into(),
        });
        table.finish();
        table
    }

    #[test]
    fn parked_threads_are_excluded_from_the_ranking_by_default() {
        // Nine samples of a thread asleep, one of real work. Unfiltered, the sleeping thread
        // would top the list at 90%.
        let mut state = ProfileState::new();
        for _ in 0..9 {
            state.record_stack(&[4050]);
        }
        state.record_stack(&[1050]);

        let hot = state.hot_methods(&table_with_wait(), 10, false);
        assert_eq!(hot.rows.len(), 1);
        assert_eq!(hot.rows[0].name, "App.Work", "only the working method survives");
        assert_eq!(hot.working_samples, 1);
        assert_eq!(hot.waiting_samples, 9);
        assert_eq!(hot.waiting_percent(), 90.0);
        // Percentages are shares of all sampled time, so the single working sample reads as 10%
        // of the process, not 100% of the work. That keeps self and total on one scale.
        assert_eq!(hot.rows[0].self_percent, 10.0);
    }

    #[test]
    fn a_method_on_every_stack_cannot_exceed_one_hundred_percent() {
        // Thread entry points sit under everything. With a filtered denominator they would
        // report several hundred percent.
        let mut state = ProfileState::new();
        for _ in 0..9 {
            state.record_stack(&[4050, 1050]);
        }
        state.record_stack(&[1050]);

        let hot = state.hot_methods(&table_with_wait(), 10, false);
        let work = hot.rows.iter().find(|m| m.name == "App.Work").unwrap();
        assert!(work.total_percent <= 100.0, "got {}%", work.total_percent);
        assert_eq!(work.total_percent, 100.0, "Work is on every stack");
    }

    #[test]
    fn parked_threads_can_be_shown_on_request() {
        let mut state = ProfileState::new();
        for _ in 0..9 {
            state.record_stack(&[4050]);
        }
        state.record_stack(&[1050]);

        let hot = state.hot_methods(&table_with_wait(), 10, true);
        assert_eq!(hot.rows.len(), 2);
        assert_eq!(hot.rows[0].name, "System.Threading.Monitor.Wait");
        assert_eq!(hot.rows[0].self_percent, 90.0);
        assert_eq!(hot.waiting_samples, 0, "nothing was filtered out");
    }

    #[test]
    fn spin_waits_count_as_work_because_they_burn_cpu() {
        assert!(!is_wait_frame("System.Threading.Thread.LongSpinWait"));
        assert!(!is_wait_frame("System.Threading.SpinWait.SpinOnce"));
        assert!(is_wait_frame("System.Threading.Monitor.Wait"));
    }

    #[test]
    fn a_method_merely_named_wait_is_not_treated_as_blocking() {
        // Matching is exact, so application code is never silently hidden.
        assert!(!is_wait_frame("MyApp.Orders.WaitForCustomer"));
        assert!(!is_wait_frame("System.Threading.Monitor.WaitHelper"));
    }

    #[test]
    fn waiting_percent_of_nothing_is_zero() {
        assert_eq!(HotProfile::default().waiting_percent(), 0.0);
    }

    #[test]
    fn reset_clears_everything() {
        let mut state = ProfileState::new();
        state.record_stack(&[1050]);
        assert!(!state.is_empty());
        state.reset();
        assert!(state.is_empty());
        assert!(state.hot_methods(&table(), 10, false).rows.is_empty());
    }
}
