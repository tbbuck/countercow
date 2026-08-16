//! The CPU profiling session.
//!
//! Unlike counters and runtime events, this one cannot report as it goes. Stack frames arrive as
//! bare instruction pointers, and the table that maps them to method names is emitted by the
//! runtime's *rundown* — which is delivered when the session stops. So a profile is necessarily
//! "collect for a fixed window, then resolve", not a live view.

use std::path::Path;

use crate::counters::session::SessionError;
use crate::ipc::commands::{self, Provider, TraceConfig, TraceSession};

/// Emits one sample per thread per millisecond, each carrying a stack.
pub const SAMPLE_PROFILER: &str = "Microsoft-DotNETCore-SampleProfiler";
/// Method load events for anything jitted while the session runs.
pub const RUNTIME_PROVIDER: &str = "Microsoft-Windows-DotNETRuntime";
/// Where the bulk of the method table arrives, at session stop.
pub const RUNDOWN_PROVIDER: &str = "Microsoft-Windows-DotNETRuntimeRundown";

/// Jit keyword: method load/unload events.
const KEYWORD_JIT: u64 = 0x10;
/// Loader keyword: module and assembly loads.
const KEYWORD_LOADER: u64 = 0x8;

/// Samples arrive at ~13,000/second on a busy app, so the buffer needs real headroom.
const CIRCULAR_BUFFER_MB: u32 = 256;

/// What a thread was doing when it was sampled.
///
/// The profiler samples every thread, including ones parked in a wait. Without this distinction a
/// hot-method list is dominated by whatever thread sits blocked the longest — technically the most
/// sampled code, and completely useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleType {
    /// The runtime could not walk the stack.
    Error,
    /// In native code: a syscall, a wait, the GC, or a P/Invoke.
    External,
    /// Executing managed code — the samples that answer "what is burning CPU".
    Managed,
    Unknown(u32),
}

impl SampleType {
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => SampleType::Error,
            1 => SampleType::External,
            2 => SampleType::Managed,
            other => SampleType::Unknown(other),
        }
    }

    /// The `ThreadSample` payload is a single `u32` sample type.
    pub fn from_payload(payload: &[u8]) -> Self {
        match payload.get(0..4) {
            Some(bytes) => {
                Self::from_u32(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            None => SampleType::Unknown(u32::MAX),
        }
    }
}

pub mod event_id {
    /// The sampling profiler's only event: a thread sample, carrying a stack id.
    pub const THREAD_SAMPLE: i32 = 0;

    /// Event ids carrying a method's address range and name.
    ///
    /// The runtime and rundown providers number these differently and the numbering has shifted
    /// between versions, so this is a range rather than a fixed pair: `MethodLoadVerbose`,
    /// `MethodUnloadVerbose` and the `MethodDCStart/EndVerbose` rundown records all share one
    /// payload shape. Records that do not decode to a plausible method are discarded, so casting
    /// the net a little wide is safe.
    pub const METHOD_VERBOSE_IDS: std::ops::RangeInclusive<i32> = 141..=144;
}

pub fn trace_config() -> TraceConfig {
    TraceConfig {
        circular_buffer_mb: CIRCULAR_BUFFER_MB,
        // The whole point: without rundown, every stack frame is an unresolvable address.
        request_rundown: true,
        providers: vec![
            Provider {
                name: SAMPLE_PROFILER.to_owned(),
                keywords: 0,
                level: 4,
                filter_data: String::new(),
            },
            Provider {
                name: RUNTIME_PROVIDER.to_owned(),
                keywords: KEYWORD_JIT | KEYWORD_LOADER,
                level: 5,
                filter_data: String::new(),
            },
        ],
    }
}

pub fn start(socket: &Path) -> Result<TraceSession, SessionError> {
    Ok(commands::start_tracing(socket, &trace_config())?)
}

pub fn stop(socket: &Path, session_id: u64) -> Result<(), SessionError> {
    Ok(commands::stop_tracing(socket, session_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rundown_is_requested_because_names_depend_on_it() {
        let config = trace_config();
        assert!(config.request_rundown, "without this, frames are bare addresses");
    }

    #[test]
    fn sample_types_decode_from_the_four_byte_payload() {
        // Captured from a live runtime: a thread parked in a wait.
        assert_eq!(SampleType::from_payload(&[0x01, 0, 0, 0]), SampleType::External);
        assert_eq!(SampleType::from_payload(&[0x02, 0, 0, 0]), SampleType::Managed);
        assert_eq!(SampleType::from_payload(&[0x00, 0, 0, 0]), SampleType::Error);
    }

    #[test]
    fn a_truncated_sample_payload_is_not_mistaken_for_managed() {
        assert!(!matches!(SampleType::from_payload(&[]), SampleType::Managed));
        assert!(!matches!(SampleType::from_payload(&[2, 0]), SampleType::Managed));
    }

    #[test]
    fn subscribes_to_the_profiler_and_the_jit() {
        let config = trace_config();
        let names: Vec<&str> = config.providers.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&SAMPLE_PROFILER));
        assert!(names.contains(&RUNTIME_PROVIDER));
    }
}
