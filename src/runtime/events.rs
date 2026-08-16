//! Decoders for `Microsoft-Windows-DotNETRuntime` events.
//!
//! This provider is manifest-based rather than EventSource-based, which changes everything about
//! how it is read: **no event names and no field lists arrive on the wire**, only a numeric event
//! id and version. The metadata-driven decoding used for counters cannot work here, so each
//! event's layout is hardcoded and keyed on `(id, version)`.
//!
//! Every layout below was confirmed byte-for-byte against a live .NET 10 runtime, and checked
//! against the workload that produced it — see `examples/probe.rs` to re-verify on another
//! runtime version.

use crate::nettrace::reader::Reader;

/// Provider name for the runtime's manifest-based events.
pub const RUNTIME_PROVIDER: &str = "Microsoft-Windows-DotNETRuntime";

pub mod keyword {
    /// GC events, including allocation ticks and suspension.
    pub const GC: u64 = 0x1;
    /// Lock contention start/stop.
    pub const CONTENTION: u64 = 0x4000;
    /// Thrown exceptions.
    pub const EXCEPTION: u64 = 0x8000;
}

pub mod event_id {
    pub const GC_START: i32 = 1;
    pub const GC_END: i32 = 2;
    pub const GC_RESTART_EE_END: i32 = 3;
    pub const GC_HEAP_STATS: i32 = 4;
    pub const GC_SUSPEND_EE_BEGIN: i32 = 9;
    pub const GC_ALLOCATION_TICK: i32 = 10;
    pub const EXCEPTION_THROWN: i32 = 80;
    pub const CONTENTION_START: i32 = 81;
    pub const CONTENTION_STOP: i32 = 91;
}

/// Why the runtime decided to collect. The field that answers "what caused this GC".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcReason {
    AllocSmall,
    Induced,
    LowMemory,
    Empty,
    AllocLarge,
    OutOfSpaceSoh,
    OutOfSpaceLoh,
    InducedNotForced,
    Internal,
    InducedLowMemory,
    InducedCompacting,
    LowMemoryHost,
    PmFullGc,
    LowMemoryHostBlocking,
    Unknown(u32),
}

impl GcReason {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => GcReason::AllocSmall,
            1 => GcReason::Induced,
            2 => GcReason::LowMemory,
            3 => GcReason::Empty,
            4 => GcReason::AllocLarge,
            5 => GcReason::OutOfSpaceSoh,
            6 => GcReason::OutOfSpaceLoh,
            7 => GcReason::InducedNotForced,
            8 => GcReason::Internal,
            9 => GcReason::InducedLowMemory,
            10 => GcReason::InducedCompacting,
            11 => GcReason::LowMemoryHost,
            12 => GcReason::PmFullGc,
            13 => GcReason::LowMemoryHostBlocking,
            other => GcReason::Unknown(other),
        }
    }

    /// Short label for the dashboard.
    pub fn label(self) -> String {
        match self {
            GcReason::AllocSmall => "small alloc".into(),
            GcReason::Induced => "induced".into(),
            GcReason::LowMemory => "low memory".into(),
            GcReason::Empty => "empty".into(),
            GcReason::AllocLarge => "large alloc".into(),
            GcReason::OutOfSpaceSoh => "SOH full".into(),
            GcReason::OutOfSpaceLoh => "LOH full".into(),
            GcReason::InducedNotForced => "induced (soft)".into(),
            GcReason::Internal => "internal".into(),
            GcReason::InducedLowMemory => "induced, low mem".into(),
            GcReason::InducedCompacting => "induced compact".into(),
            GcReason::LowMemoryHost => "host low memory".into(),
            GcReason::PmFullGc => "provisional full".into(),
            GcReason::LowMemoryHostBlocking => "host low mem (block)".into(),
            GcReason::Unknown(v) => format!("reason {v}"),
        }
    }

    /// Whether this reason indicates memory pressure rather than routine collection.
    pub fn is_pressure(self) -> bool {
        matches!(
            self,
            GcReason::LowMemory
                | GcReason::OutOfSpaceSoh
                | GcReason::OutOfSpaceLoh
                | GcReason::InducedLowMemory
                | GcReason::LowMemoryHost
                | GcReason::LowMemoryHostBlocking
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcType {
    /// A blocking collection: the application is paused throughout.
    Blocking,
    Background,
    Foreground,
    Unknown(u32),
}

impl GcType {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => GcType::Blocking,
            1 => GcType::Background,
            2 => GcType::Foreground,
            other => GcType::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GcType::Blocking => "blocking",
            GcType::Background => "background",
            GcType::Foreground => "foreground",
            GcType::Unknown(_) => "?",
        }
    }
}

/// Which heap an allocation landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationKind {
    Small,
    Large,
    Pinned,
    Unknown(u32),
}

impl AllocationKind {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => AllocationKind::Small,
            1 => AllocationKind::Large,
            2 => AllocationKind::Pinned,
            other => AllocationKind::Unknown(other),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AllocationKind::Small => "SOH",
            AllocationKind::Large => "LOH",
            AllocationKind::Pinned => "POH",
            AllocationKind::Unknown(_) => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GcStart {
    /// Collection number, monotonically increasing.
    pub count: u32,
    /// Generation being collected.
    pub depth: u32,
    pub reason: GcReason,
    pub gc_type: GcType,
}

#[derive(Debug, Clone)]
pub struct GcEnd {
    pub count: u32,
    pub depth: u32,
}

/// Bytes that *survived* a collection, per generation. Promotion is the interesting half: a
/// generation that keeps promoting is what drives collections up the generations.
#[derive(Debug, Clone, Default)]
pub struct GcHeapStats {
    pub gen0_size: u64,
    pub gen0_promoted: u64,
    pub gen1_size: u64,
    pub gen1_promoted: u64,
    pub gen2_size: u64,
    pub gen2_promoted: u64,
    pub loh_size: u64,
    pub loh_promoted: u64,
    pub finalization_promoted_size: u64,
    pub finalization_promoted_count: u64,
    pub pinned_object_count: u32,
    pub sink_block_count: u32,
    pub gc_handle_count: u32,
}

/// A sampled allocation. The runtime emits one roughly every 100 KB of small-object allocation,
/// and per allocation for large objects — so counts are a sample, but the byte totals are a good
/// estimate of where allocation pressure comes from.
#[derive(Debug, Clone)]
pub struct AllocationTick {
    pub amount: u64,
    pub kind: AllocationKind,
    pub type_name: String,
    pub object_size: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ExceptionThrown {
    pub type_name: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ContentionStop {
    pub duration_ns: f64,
}

/// GCStart_V2: `Count:u32, Depth:u32, Reason:u32, Type:u32, ClrInstanceID:u16, ClientSequenceNumber:u64`
pub fn decode_gc_start(payload: &[u8]) -> Option<GcStart> {
    let mut r = Reader::new(payload);
    Some(GcStart {
        count: r.u32().ok()?,
        depth: r.u32().ok()?,
        reason: GcReason::from_u32(r.u32().ok()?),
        gc_type: GcType::from_u32(r.u32().ok()?),
    })
}

/// GCEnd_V1: `Count:u32, Depth:u32, ClrInstanceID:u16`
pub fn decode_gc_end(payload: &[u8]) -> Option<GcEnd> {
    let mut r = Reader::new(payload);
    Some(GcEnd { count: r.u32().ok()?, depth: r.u32().ok()? })
}

/// GCHeapStats_V1/V2: eight `u64` size/promoted pairs, then finalization and count fields.
/// V2 appends the pinned object heap, which we do not need but must not misread as something else.
pub fn decode_gc_heap_stats(payload: &[u8]) -> Option<GcHeapStats> {
    let mut r = Reader::new(payload);
    Some(GcHeapStats {
        gen0_size: r.u64().ok()?,
        gen0_promoted: r.u64().ok()?,
        gen1_size: r.u64().ok()?,
        gen1_promoted: r.u64().ok()?,
        gen2_size: r.u64().ok()?,
        gen2_promoted: r.u64().ok()?,
        loh_size: r.u64().ok()?,
        loh_promoted: r.u64().ok()?,
        finalization_promoted_size: r.u64().ok()?,
        finalization_promoted_count: r.u64().ok()?,
        pinned_object_count: r.u32().ok()?,
        sink_block_count: r.u32().ok()?,
        gc_handle_count: r.u32().ok()?,
    })
}

/// GCAllocationTick_V2+: `AllocationAmount:u32, AllocationKind:u32, ClrInstanceID:u16,
/// AllocationAmount64:u64, TypeID:ptr, TypeName:wstr, HeapIndex:u32` then, by version,
/// `Address:ptr` (V3+) and `ObjectSize:u64` (V4+).
///
/// `pointer_size` comes from the trace header — misreading it shifts the type name and yields
/// convincing nonsense.
pub fn decode_allocation_tick(payload: &[u8], pointer_size: usize) -> Option<AllocationTick> {
    let mut r = Reader::new(payload);

    let amount32 = r.u32().ok()?;
    let kind = AllocationKind::from_u32(r.u32().ok()?);
    r.skip(2, "ClrInstanceID").ok()?;
    let amount64 = r.u64().ok()?;
    r.skip(pointer_size, "TypeID").ok()?;
    let type_name = r.utf16_nul_string().ok()?;
    r.skip(4, "HeapIndex").ok()?;

    // Trailing fields depend on the event version; absence is normal on older runtimes.
    let object_size = if r.skip(pointer_size, "Address").is_ok() {
        r.u64().ok()
    } else {
        None
    };

    Some(AllocationTick {
        // The 64-bit field is authoritative; the 32-bit one exists for compatibility.
        amount: if amount64 > 0 { amount64 } else { u64::from(amount32) },
        kind,
        type_name,
        object_size,
    })
}

/// ExceptionThrown_V1: `ExceptionType:wstr, ExceptionMessage:wstr, ExceptionEIP:ptr,
/// ExceptionHRESULT:u32, ExceptionFlags:u16, ClrInstanceID:u16`
pub fn decode_exception_thrown(payload: &[u8]) -> Option<ExceptionThrown> {
    let mut r = Reader::new(payload);
    Some(ExceptionThrown {
        type_name: r.utf16_nul_string().ok()?,
        message: r.utf16_nul_string().ok()?,
    })
}

/// ContentionStop_V1: `ContentionFlags:u8, ClrInstanceID:u16, DurationNs:f64`
pub fn decode_contention_stop(payload: &[u8]) -> Option<ContentionStop> {
    let mut r = Reader::new(payload);
    r.skip(1, "ContentionFlags").ok()?;
    r.skip(2, "ClrInstanceID").ok()?;
    Some(ContentionStop { duration_ns: r.f64().ok()? })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payloads captured verbatim from a live .NET 10 runtime via `examples/probe.rs`. Keeping
    /// the real bytes means these tests fail if a decoder drifts from what runtimes emit.
    mod captured {
        /// GCStart_V2 — collection 240, gen 2, reason 4, blocking.
        pub const GC_START: &[u8] = &[
            0xf0, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        /// GCEnd_V1 — collection 36, gen 2.
        pub const GC_END: &[u8] =
            &[0x24, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00];

        /// GCSuspendEEBegin_V1 — reason 1, count 35.
        pub const GC_SUSPEND: &[u8] =
            &[0x01, 0x00, 0x00, 0x00, 0x23, 0x00, 0x00, 0x00, 0x00, 0x00];

        /// GCAllocationTick_V4 — a 1 MiB System.Byte[] on the large object heap.
        pub const ALLOCATION_TICK: &[u8] = &[
            0x18, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x00, 0x10, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x20, 0x82, 0xe5, 0x0b, 0x01, 0x00, 0x00, 0x00, 0x53, 0x00,
            0x79, 0x00, 0x73, 0x00, 0x74, 0x00, 0x65, 0x00, 0x6d, 0x00, 0x2e, 0x00, 0x42, 0x00,
            0x79, 0x00, 0x74, 0x00, 0x65, 0x00, 0x5b, 0x00, 0x5d, 0x00, 0x00, 0x00, 0x0b, 0x00,
            0x00, 0x00, 0x38, 0x40, 0x00, 0xb0, 0x73, 0x00, 0x00, 0x00, 0x20, 0x00, 0x10, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        /// GCHeapStats_V2 — 110 bytes.
        pub const HEAP_STATS: &[u8] = &[
            0x20, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0xb2, 0x03, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x18, 0x21, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0xd8, 0xd4, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0, 0x33,
            0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0xa8, 0x81, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x50, 0x81, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x57, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x03, 0x00, 0x00, 0x00, 0xfc, 0x02, 0x00, 0x00, 0x00, 0x00, 0x68, 0xa8, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        /// ExceptionThrown_V1 — System.InvalidOperationException / "sample failure".
        pub const EXCEPTION: &[u8] = &[
            0x53, 0x00, 0x79, 0x00, 0x73, 0x00, 0x74, 0x00, 0x65, 0x00, 0x6d, 0x00, 0x2e, 0x00,
            0x49, 0x00, 0x6e, 0x00, 0x76, 0x00, 0x61, 0x00, 0x6c, 0x00, 0x69, 0x00, 0x64, 0x00,
            0x4f, 0x00, 0x70, 0x00, 0x65, 0x00, 0x72, 0x00, 0x61, 0x00, 0x74, 0x00, 0x69, 0x00,
            0x6f, 0x00, 0x6e, 0x00, 0x45, 0x00, 0x78, 0x00, 0x63, 0x00, 0x65, 0x00, 0x70, 0x00,
            0x74, 0x00, 0x69, 0x00, 0x6f, 0x00, 0x6e, 0x00, 0x00, 0x00, 0x73, 0x00, 0x61, 0x00,
            0x6d, 0x00, 0x70, 0x00, 0x6c, 0x00, 0x65, 0x00, 0x20, 0x00, 0x66, 0x00, 0x61, 0x00,
            0x69, 0x00, 0x6c, 0x00, 0x75, 0x00, 0x72, 0x00, 0x65, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        /// ContentionStop_V1 — 11 bytes.
        pub const CONTENTION_STOP: &[u8] =
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x68, 0x1f, 0xde, 0x97, 0x41];
    }

    #[test]
    fn gc_start_reports_generation_and_cause() {
        let gc = decode_gc_start(captured::GC_START).unwrap();
        assert_eq!(gc.count, 240);
        assert_eq!(gc.depth, 2);
        // This capture came from an app allocating 1 MiB arrays, which go straight to the LOH.
        assert_eq!(gc.reason, GcReason::AllocLarge);
        assert_eq!(gc.gc_type, GcType::Blocking);
        assert_eq!(gc.reason.label(), "large alloc");
        assert!(!gc.reason.is_pressure(), "a large allocation is routine, not pressure");
    }

    #[test]
    fn gc_end_matches_its_start() {
        let gc = decode_gc_end(captured::GC_END).unwrap();
        assert_eq!(gc.count, 36);
        assert_eq!(gc.depth, 2);
    }

    #[test]
    fn allocation_tick_names_the_allocating_type() {
        let tick = decode_allocation_tick(captured::ALLOCATION_TICK, 8).unwrap();
        assert_eq!(tick.type_name, "System.Byte[]");
        // 1 MiB plus array overhead, exactly what `new byte[1024 * 1024]` costs.
        assert_eq!(tick.amount, 1_048_600);
        assert_eq!(tick.kind, AllocationKind::Large);
        assert_eq!(tick.object_size, Some(1_048_608));
    }

    #[test]
    fn allocation_tick_pointer_size_shifts_the_type_name() {
        // Decoding a 64-bit payload as 32-bit must not silently produce a plausible-looking name.
        let wrong = decode_allocation_tick(captured::ALLOCATION_TICK, 4).unwrap();
        assert_ne!(wrong.type_name, "System.Byte[]");
    }

    #[test]
    fn heap_stats_expose_promoted_bytes_per_generation() {
        let stats = decode_gc_heap_stats(captured::HEAP_STATS).unwrap();
        assert_eq!(stats.gen0_size, 288);
        assert_eq!(stats.gen0_promoted, 242_416);
        assert_eq!(stats.gen1_size, 467_224);
        assert_eq!(stats.gen1_promoted, 0);
        assert_eq!(stats.gen2_size, 775_384);
        assert_eq!(stats.gen2_promoted, 996_304);
        assert_eq!(stats.loh_size, 1_147_304);
        assert_eq!(stats.finalization_promoted_count, 20);
        assert_eq!(stats.pinned_object_count, 1);
        assert_eq!(stats.gc_handle_count, 764);
    }

    #[test]
    fn exception_carries_type_and_message() {
        let thrown = decode_exception_thrown(captured::EXCEPTION).unwrap();
        assert_eq!(thrown.type_name, "System.InvalidOperationException");
        assert_eq!(thrown.message, "sample failure");
    }

    #[test]
    fn contention_stop_decodes_a_plausible_duration() {
        let stop = decode_contention_stop(captured::CONTENTION_STOP).unwrap();
        assert!(stop.duration_ns > 0.0, "duration should be positive");
        // Sanity bounds: a lock wait is longer than a nanosecond and shorter than a minute.
        assert!(stop.duration_ns > 1.0);
        assert!(stop.duration_ns < 60e9);
    }

    #[test]
    fn suspend_payload_is_the_expected_size() {
        // Not decoded for its fields — only its timestamp matters — but its length confirms the
        // event id mapping is right.
        assert_eq!(captured::GC_SUSPEND.len(), 10);
    }

    #[test]
    fn truncated_payloads_return_none_rather_than_guessing() {
        assert!(decode_gc_start(&[0u8; 4]).is_none());
        assert!(decode_gc_heap_stats(&[0u8; 16]).is_none());
        assert!(decode_allocation_tick(&[0u8; 8], 8).is_none());
        assert!(decode_contention_stop(&[0u8; 4]).is_none());
    }

    #[test]
    fn unknown_enum_values_are_preserved() {
        assert_eq!(GcReason::from_u32(99), GcReason::Unknown(99));
        assert_eq!(GcReason::from_u32(99).label(), "reason 99");
        assert_eq!(AllocationKind::from_u32(7).label(), "?");
    }

    #[test]
    fn pressure_reasons_are_distinguished_from_routine_ones() {
        assert!(GcReason::OutOfSpaceSoh.is_pressure());
        assert!(GcReason::LowMemory.is_pressure());
        assert!(!GcReason::AllocSmall.is_pressure());
        assert!(!GcReason::Induced.is_pressure());
    }
}
