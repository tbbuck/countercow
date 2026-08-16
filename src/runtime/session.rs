//! The investigation session: a second, deliberately short-lived EventPipe session carrying
//! runtime events rather than counters.
//!
//! This costs the target process real CPU — counters arrive at roughly 40 events/second, whereas
//! the GC keyword alone produced ~1,600/second on a loaded app. It is opened when the user asks
//! for it and closed when they leave.

use std::ops::ControlFlow;
use std::path::Path;

use crate::counters::session::SessionError;
use crate::ipc::commands::{self, Provider, TraceConfig, TraceSession};
use crate::nettrace::blocks::NettraceParser;

use super::events::{self, event_id, keyword, RUNTIME_PROVIDER};

/// Larger than the counter session's buffer: these events are far higher volume, and an
/// undersized buffer means the runtime drops them.
const CIRCULAR_BUFFER_MB: u32 = 64;

/// EventLevel.Verbose — allocation ticks are emitted at verbose.
const LEVEL_VERBOSE: u32 = 5;

/// One decoded runtime event, with the timestamp needed to measure pauses.
#[derive(Debug)]
pub enum RuntimeEvent {
    Allocation(events::AllocationTick),
    GcStart(events::GcStart),
    GcEnd,
    HeapStats(events::GcHeapStats),
    SuspendBegin { timestamp: u64 },
    RestartEnd { timestamp: u64 },
    Exception(events::ExceptionThrown),
    Contention(events::ContentionStop),
}

pub fn trace_config() -> TraceConfig {
    TraceConfig {
        circular_buffer_mb: CIRCULAR_BUFFER_MB,
        providers: vec![Provider {
            name: RUNTIME_PROVIDER.to_owned(),
            keywords: keyword::GC | keyword::CONTENTION | keyword::EXCEPTION,
            level: LEVEL_VERBOSE,
            // Manifest-based providers take no filter arguments.
            filter_data: String::new(),
        }],
    }
}

pub fn start(socket: &Path) -> Result<TraceSession, SessionError> {
    Ok(commands::start_tracing(socket, &trace_config())?)
}

pub fn stop(socket: &Path, session_id: u64) -> Result<(), SessionError> {
    Ok(commands::stop_tracing(socket, session_id)?)
}

/// Parse a runtime session, reporting decoded events until the callback breaks or the stream ends.
///
/// Events we do not model are skipped: this provider emits many more kinds than are useful here,
/// and there is no schema on the wire to decode them generically anyway.
pub fn run<F>(stream: impl std::io::Read, mut on_event: F) -> Result<(), SessionError>
where
    F: FnMut(RuntimeEvent, i64) -> ControlFlow<()>,
{
    let mut parser = NettraceParser::new(stream)?;

    loop {
        let Some(batch) = parser.next_events()? else {
            return Ok(());
        };

        // Pointer width and clock frequency come from the trace header, and both matter:
        // the first shifts field offsets, the second scales pause durations.
        let pointer_size = parser.trace_info().pointer_size.clamp(4, 8) as usize;
        let qpc_frequency = parser.trace_info().qpc_frequency;

        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };
            if metadata.provider_name != RUNTIME_PROVIDER {
                continue;
            }

            let decoded = match metadata.event_id {
                event_id::GC_ALLOCATION_TICK => {
                    events::decode_allocation_tick(&event.payload, pointer_size)
                        .map(RuntimeEvent::Allocation)
                }
                event_id::GC_START => {
                    events::decode_gc_start(&event.payload).map(RuntimeEvent::GcStart)
                }
                event_id::GC_END => Some(RuntimeEvent::GcEnd),
                event_id::GC_HEAP_STATS => {
                    events::decode_gc_heap_stats(&event.payload).map(RuntimeEvent::HeapStats)
                }
                event_id::GC_SUSPEND_EE_BEGIN => {
                    Some(RuntimeEvent::SuspendBegin { timestamp: event.timestamp })
                }
                event_id::GC_RESTART_EE_END => {
                    Some(RuntimeEvent::RestartEnd { timestamp: event.timestamp })
                }
                event_id::EXCEPTION_THROWN => {
                    events::decode_exception_thrown(&event.payload).map(RuntimeEvent::Exception)
                }
                event_id::CONTENTION_STOP => {
                    events::decode_contention_stop(&event.payload).map(RuntimeEvent::Contention)
                }
                _ => None,
            };

            let Some(decoded) = decoded else {
                continue;
            };
            if on_event(decoded, qpc_frequency).is_break() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribes_to_the_manifest_provider_with_all_three_keywords() {
        let config = trace_config();
        assert_eq!(config.providers.len(), 1);

        let provider = &config.providers[0];
        assert_eq!(provider.name, RUNTIME_PROVIDER);
        assert_eq!(provider.keywords, 0x1 | 0x4000 | 0x8000);
        // Allocation ticks are verbose-level; anything lower silently omits them.
        assert_eq!(provider.level, LEVEL_VERBOSE);
        // Manifest-based providers take no EventCounterIntervalSec-style arguments.
        assert!(provider.filter_data.is_empty());
    }

    #[test]
    fn the_buffer_is_larger_than_the_counter_session() {
        // These events arrive orders of magnitude faster than counters; too small a buffer means
        // the runtime drops them rather than blocking.
        let runtime = trace_config();
        let counters = crate::counters::session::trace_config(1.0);
        assert!(
            runtime.circular_buffer_mb > counters.circular_buffer_mb,
            "runtime sessions need more headroom than counter sessions"
        );
    }
}
