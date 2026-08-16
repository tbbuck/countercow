//! Aggregated view of what the runtime has been doing: what allocates, why collections happen,
//! what throws, and what blocks.

use std::collections::HashMap;

use super::events::{AllocationKind, ContentionStop, ExceptionThrown, GcHeapStats, GcReason, GcStart, GcType};

/// Collections retained for the recent-GC list.
const MAX_RECENT_GCS: usize = 64;

/// A type's share of allocation.
#[derive(Debug, Clone)]
pub struct AllocationSite {
    pub type_name: String,
    pub bytes: u64,
    pub ticks: u64,
    pub kind: AllocationKind,
}

/// One observed collection.
#[derive(Debug, Clone)]
pub struct Collection {
    pub count: u32,
    pub generation: u32,
    pub reason: GcReason,
    pub gc_type: GcType,
    /// Wall-clock pause, when both the suspend and restart events were seen.
    pub pause_ms: Option<f64>,
    /// Bytes surviving, summed across generations.
    pub promoted_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ExceptionSite {
    pub type_name: String,
    pub message: String,
    pub count: u64,
}

#[derive(Debug, Default, Clone)]
pub struct ContentionSummary {
    pub events: u64,
    pub total_ns: f64,
    pub max_ns: f64,
}

impl ContentionSummary {
    pub fn mean_ns(&self) -> f64 {
        if self.events == 0 {
            0.0
        } else {
            self.total_ns / self.events as f64
        }
    }
}

/// Everything the investigation session has accumulated.
#[derive(Debug, Default)]
pub struct RuntimeState {
    allocations: HashMap<String, AllocationSite>,
    exceptions: HashMap<String, ExceptionSite>,
    pub contention: ContentionSummary,
    recent_gcs: Vec<Collection>,

    pub total_allocated_bytes: u64,
    pub allocation_ticks: u64,
    pub total_exceptions: u64,

    /// Collections seen, by generation.
    pub gc_counts: [u64; 3],
    pub total_pause_ms: f64,

    /// A collection that has started but not yet reported its heap stats or end.
    pending: Option<Collection>,
    /// QPC timestamp of the last suspend, awaiting its matching restart.
    suspend_started: Option<u64>,
    last_pause_ms: Option<f64>,
    /// Set once anything at all has arrived, so the UI can distinguish "quiet" from "not started".
    pub events_seen: u64,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_allocation(&mut self, type_name: String, bytes: u64, kind: AllocationKind) {
        self.events_seen += 1;
        self.total_allocated_bytes += bytes;
        self.allocation_ticks += 1;

        let site = self.allocations.entry(type_name.clone()).or_insert(AllocationSite {
            type_name,
            bytes: 0,
            ticks: 0,
            kind,
        });
        site.bytes += bytes;
        site.ticks += 1;
        // A type can appear on more than one heap; the larger heap is the more interesting fact.
        if kind == AllocationKind::Large {
            site.kind = kind;
        }
    }

    pub fn record_gc_start(&mut self, start: GcStart) {
        self.events_seen += 1;
        // A collection that never reported stats still counts; flush it before starting another.
        self.flush_pending();

        if let Some(slot) = self.gc_counts.get_mut(start.depth.min(2) as usize) {
            *slot += 1;
        }

        self.pending = Some(Collection {
            count: start.count,
            generation: start.depth,
            reason: start.reason,
            gc_type: start.gc_type,
            // The suspend precedes the start, so the pause is already measurable.
            pause_ms: self.last_pause_ms.take(),
            promoted_bytes: None,
        });
    }

    pub fn record_heap_stats(&mut self, stats: GcHeapStats) {
        self.events_seen += 1;
        if let Some(pending) = &mut self.pending {
            pending.promoted_bytes = Some(
                stats.gen0_promoted + stats.gen1_promoted + stats.gen2_promoted + stats.loh_promoted,
            );
        }
    }

    pub fn record_gc_end(&mut self) {
        self.events_seen += 1;
        self.flush_pending();
    }

    /// Suspension begins; the pause runs until the runtime restarts execution.
    pub fn record_suspend(&mut self, timestamp: u64) {
        self.events_seen += 1;
        self.suspend_started = Some(timestamp);
    }

    /// Execution restarts. `qpc_frequency` converts the tick delta to milliseconds.
    pub fn record_restart(&mut self, timestamp: u64, qpc_frequency: i64) {
        self.events_seen += 1;
        let Some(started) = self.suspend_started.take() else {
            return;
        };
        if qpc_frequency <= 0 || timestamp <= started {
            return;
        }

        let pause_ms = (timestamp - started) as f64 / qpc_frequency as f64 * 1000.0;
        self.total_pause_ms += pause_ms;

        // The suspend/restart pair brackets the collection, so attribute it to the collection in
        // flight if there is one, and hold it for the next start otherwise.
        match &mut self.pending {
            Some(pending) if pending.pause_ms.is_none() => pending.pause_ms = Some(pause_ms),
            _ => self.last_pause_ms = Some(pause_ms),
        }
    }

    pub fn record_exception(&mut self, thrown: ExceptionThrown) {
        self.events_seen += 1;
        self.total_exceptions += 1;

        let entry = self.exceptions.entry(thrown.type_name.clone()).or_insert(ExceptionSite {
            type_name: thrown.type_name,
            message: thrown.message,
            count: 0,
        });
        entry.count += 1;
    }

    pub fn record_contention(&mut self, stop: ContentionStop) {
        self.events_seen += 1;
        self.contention.events += 1;
        self.contention.total_ns += stop.duration_ns;
        self.contention.max_ns = self.contention.max_ns.max(stop.duration_ns);
    }

    fn flush_pending(&mut self) {
        let Some(collection) = self.pending.take() else {
            return;
        };
        self.recent_gcs.push(collection);
        if self.recent_gcs.len() > MAX_RECENT_GCS {
            let overflow = self.recent_gcs.len() - MAX_RECENT_GCS;
            self.recent_gcs.drain(..overflow);
        }
    }

    /// Allocation sites, heaviest first.
    pub fn top_allocations(&self, limit: usize) -> Vec<AllocationSite> {
        let mut sites: Vec<AllocationSite> = self.allocations.values().cloned().collect();
        sites.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.type_name.cmp(&b.type_name)));
        sites.truncate(limit);
        sites
    }

    /// Exception types, most frequent first.
    pub fn top_exceptions(&self, limit: usize) -> Vec<ExceptionSite> {
        let mut sites: Vec<ExceptionSite> = self.exceptions.values().cloned().collect();
        sites.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.type_name.cmp(&b.type_name)));
        sites.truncate(limit);
        sites
    }

    /// Most recent collections first, including one still in flight.
    pub fn recent_collections(&self, limit: usize) -> Vec<Collection> {
        let mut out: Vec<Collection> = Vec::with_capacity(limit);
        if let Some(pending) = &self.pending {
            out.push(pending.clone());
        }
        out.extend(self.recent_gcs.iter().rev().cloned());
        out.truncate(limit);
        out
    }

    /// A type's share of all allocation seen, as a percentage.
    pub fn allocation_share(&self, site: &AllocationSite) -> f64 {
        if self.total_allocated_bytes == 0 {
            0.0
        } else {
            site.bytes as f64 / self.total_allocated_bytes as f64 * 100.0
        }
    }

    pub fn total_gcs(&self) -> u64 {
        self.gc_counts.iter().sum()
    }

    pub fn is_empty(&self) -> bool {
        self.events_seen == 0
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gc_start(count: u32, depth: u32, reason: GcReason) -> GcStart {
        GcStart { count, depth, reason, gc_type: GcType::Blocking }
    }

    #[test]
    fn allocations_accumulate_by_type_and_rank_by_bytes() {
        let mut state = RuntimeState::new();
        state.record_allocation("System.Byte[]".into(), 1_000_000, AllocationKind::Large);
        state.record_allocation("System.Byte[]".into(), 1_000_000, AllocationKind::Large);
        state.record_allocation("System.String".into(), 500_000, AllocationKind::Small);

        let top = state.top_allocations(10);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].type_name, "System.Byte[]");
        assert_eq!(top[0].bytes, 2_000_000);
        assert_eq!(top[0].ticks, 2);
        assert_eq!(top[1].type_name, "System.String");

        assert_eq!(state.total_allocated_bytes, 2_500_000);
        assert_eq!(state.allocation_share(&top[0]).round(), 80.0);
    }

    #[test]
    fn a_type_seen_on_both_heaps_reports_the_large_one() {
        let mut state = RuntimeState::new();
        state.record_allocation("System.Byte[]".into(), 100, AllocationKind::Small);
        state.record_allocation("System.Byte[]".into(), 1_000_000, AllocationKind::Large);
        assert_eq!(state.top_allocations(1)[0].kind, AllocationKind::Large);
    }

    #[test]
    fn collections_record_their_generation_and_reason() {
        let mut state = RuntimeState::new();
        state.record_gc_start(gc_start(1, 0, GcReason::AllocSmall));
        state.record_gc_end();
        state.record_gc_start(gc_start(2, 2, GcReason::AllocLarge));
        state.record_gc_end();

        let recent = state.recent_collections(10);
        assert_eq!(recent.len(), 2);
        // Most recent first.
        assert_eq!(recent[0].count, 2);
        assert_eq!(recent[0].generation, 2);
        assert_eq!(recent[0].reason, GcReason::AllocLarge);

        assert_eq!(state.gc_counts[0], 1);
        assert_eq!(state.gc_counts[2], 1);
        assert_eq!(state.total_gcs(), 2);
    }

    #[test]
    fn a_collection_in_flight_is_shown_before_it_ends() {
        let mut state = RuntimeState::new();
        state.record_gc_start(gc_start(5, 1, GcReason::AllocSmall));
        let recent = state.recent_collections(10);
        assert_eq!(recent.len(), 1, "the pending collection should be visible");
        assert_eq!(recent[0].count, 5);
    }

    #[test]
    fn a_collection_without_an_end_is_not_lost_when_the_next_starts() {
        let mut state = RuntimeState::new();
        state.record_gc_start(gc_start(1, 0, GcReason::AllocSmall));
        state.record_gc_start(gc_start(2, 0, GcReason::AllocSmall));

        let recent = state.recent_collections(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].count, 2, "the new one is pending");
        assert_eq!(recent[1].count, 1, "the old one was flushed, not dropped");
    }

    #[test]
    fn pause_is_measured_from_suspend_to_restart() {
        let mut state = RuntimeState::new();
        // 1e9 ticks per second, so 2.5e6 ticks is 2.5 ms.
        state.record_suspend(1_000_000);
        state.record_restart(3_500_000, 1_000_000_000);
        state.record_gc_start(gc_start(1, 2, GcReason::AllocLarge));
        state.record_gc_end();

        let recent = state.recent_collections(1);
        assert_eq!(recent[0].pause_ms.unwrap(), 2.5);
        assert_eq!(state.total_pause_ms, 2.5);
    }

    #[test]
    fn a_restart_without_a_suspend_is_ignored() {
        let mut state = RuntimeState::new();
        state.record_restart(5_000, 1_000_000_000);
        assert_eq!(state.total_pause_ms, 0.0);
    }

    #[test]
    fn a_backwards_or_zero_frequency_timestamp_does_not_produce_a_bogus_pause() {
        let mut state = RuntimeState::new();
        state.record_suspend(10_000);
        state.record_restart(5_000, 1_000_000_000);
        assert_eq!(state.total_pause_ms, 0.0);

        state.record_suspend(10_000);
        state.record_restart(20_000, 0);
        assert_eq!(state.total_pause_ms, 0.0);
    }

    #[test]
    fn heap_stats_attach_promoted_bytes_to_the_collection_in_flight() {
        let mut state = RuntimeState::new();
        state.record_gc_start(gc_start(1, 2, GcReason::AllocLarge));
        state.record_heap_stats(GcHeapStats {
            gen0_promoted: 100,
            gen1_promoted: 200,
            gen2_promoted: 300,
            loh_promoted: 400,
            ..Default::default()
        });
        state.record_gc_end();

        assert_eq!(state.recent_collections(1)[0].promoted_bytes, Some(1000));
    }

    #[test]
    fn exceptions_group_by_type() {
        let mut state = RuntimeState::new();
        for _ in 0..3 {
            state.record_exception(ExceptionThrown {
                type_name: "System.InvalidOperationException".into(),
                message: "sample failure".into(),
            });
        }
        state.record_exception(ExceptionThrown {
            type_name: "System.ArgumentNullException".into(),
            message: "value".into(),
        });

        let top = state.top_exceptions(10);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].type_name, "System.InvalidOperationException");
        assert_eq!(top[0].count, 3);
        assert_eq!(top[0].message, "sample failure");
        assert_eq!(state.total_exceptions, 4);
    }

    #[test]
    fn contention_tracks_total_mean_and_worst() {
        let mut state = RuntimeState::new();
        state.record_contention(ContentionStop { duration_ns: 1_000.0 });
        state.record_contention(ContentionStop { duration_ns: 3_000.0 });

        assert_eq!(state.contention.events, 2);
        assert_eq!(state.contention.total_ns, 4_000.0);
        assert_eq!(state.contention.mean_ns(), 2_000.0);
        assert_eq!(state.contention.max_ns, 3_000.0);
    }

    #[test]
    fn mean_contention_of_nothing_is_zero_not_a_division_by_zero() {
        assert_eq!(ContentionSummary::default().mean_ns(), 0.0);
    }

    #[test]
    fn the_recent_collection_list_is_bounded() {
        let mut state = RuntimeState::new();
        for i in 0..MAX_RECENT_GCS as u32 + 20 {
            state.record_gc_start(gc_start(i, 0, GcReason::AllocSmall));
            state.record_gc_end();
        }
        assert_eq!(state.recent_collections(1000).len(), MAX_RECENT_GCS);
        // The newest survive, the oldest are dropped.
        assert_eq!(state.recent_collections(1)[0].count, MAX_RECENT_GCS as u32 + 19);
    }

    #[test]
    fn reset_clears_everything() {
        let mut state = RuntimeState::new();
        state.record_allocation("T".into(), 10, AllocationKind::Small);
        state.record_gc_start(gc_start(1, 0, GcReason::AllocSmall));
        assert!(!state.is_empty());

        state.reset();
        assert!(state.is_empty());
        assert_eq!(state.total_allocated_bytes, 0);
        assert!(state.top_allocations(10).is_empty());
        assert!(state.recent_collections(10).is_empty());
    }

    #[test]
    fn share_of_nothing_is_zero() {
        let state = RuntimeState::new();
        let site = AllocationSite {
            type_name: "T".into(),
            bytes: 0,
            ticks: 0,
            kind: AllocationKind::Small,
        };
        assert_eq!(state.allocation_share(&site), 0.0);
    }
}
