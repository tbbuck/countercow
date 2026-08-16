//! Driving a profile from start to resolved result.

use std::io::Read;

use crate::counters::session::SessionError;
use crate::nettrace::blocks::NettraceParser;

use super::methods::{decode_method, MethodTable};
use super::session::{event_id, SampleType, RUNDOWN_PROVIDER, RUNTIME_PROVIDER, SAMPLE_PROFILER};
use super::state::ProfileState;

/// Progress reported while collecting, so the UI can show something during the window.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub samples: u64,
    pub methods: usize,
}

/// How many stacks to keep verbatim for diagnostics.
const SAMPLE_STACKS_KEPT: usize = 8;

#[derive(Debug, Default)]
pub struct ProfileResult {
    pub state: ProfileState,
    pub methods: MethodTable,
    /// A few stacks kept as-is, for checking frame order and resolution by eye.
    pub sample_stacks: Vec<Vec<u64>>,
}

/// Event ids that carry a method's address range and name.
///
/// Both the runtime provider (methods jitted during the session) and the rundown provider
/// (everything already loaded) use this shape.
fn is_method_event(provider: &str, id: i32) -> bool {
    event_id::METHOD_VERBOSE_IDS.contains(&id)
        && (provider == RUNTIME_PROVIDER || provider == RUNDOWN_PROVIDER)
}

/// Collect a profile from a session stream, resolving it once the stream ends.
///
/// `on_progress` is called as batches arrive; the caller stops the session when its window is up,
/// which is what makes the stream end and the rundown arrive.
pub fn collect<F>(stream: impl Read, mut on_progress: F) -> Result<ProfileResult, SessionError>
where
    F: FnMut(Progress),
{
    let mut parser = NettraceParser::new(stream)?;
    parser.collect_stacks();

    let mut state = ProfileState::new();
    let mut methods = MethodTable::new();

    // Samples wait for the next sequence point before being resolved.
    //
    // Two facts force this. Stack blocks lag the events referencing them, so resolving as samples
    // arrive loses most of them. But stack ids are also *reused* across sequence points, so
    // resolving everything at the very end reads later stacks for earlier samples — which silently
    // produces addresses that belong to no method at all. A sequence point is exactly the boundary
    // where every preceding event has been emitted and no id has yet been recycled.
    let mut pending: Vec<(u32, SampleType)> = Vec::new();
    let mut sample_stacks: Vec<Vec<u64>> = Vec::new();

    while let Some(batch) = parser.next_events()? {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };

            if metadata.provider_name == SAMPLE_PROFILER
                && metadata.event_id == event_id::THREAD_SAMPLE
            {
                pending.push((event.stack_id, SampleType::from_payload(&event.payload)));
            } else if is_method_event(&metadata.provider_name, metadata.event_id) {
                if let Some(method) = decode_method(&event.payload) {
                    // Guard against decoding a same-numbered event of a different shape.
                    if !method.name.is_empty() && method.start_address > 0 {
                        methods.insert(method);
                    }
                }
            }
        }

        if parser.take_sequence_point() {
            keep_examples(&pending, &parser, &mut sample_stacks);
            drain(&mut pending, &mut state, &parser);
            // Discard the stacks now they have been used. Ids are recycled after a sequence
            // point, so keeping them means a later stack silently answers for an earlier id —
            // which resolves to addresses in no method at all rather than failing visibly.
            parser.rotate_stacks();
        }

        on_progress(Progress {
            samples: state.samples + pending.len() as u64,
            methods: methods.len(),
        });
    }

    // Samples after the final sequence point; their stacks are still current.
    drain(&mut pending, &mut state, &parser);
    methods.finish();

    Ok(ProfileResult { state, methods, sample_stacks })
}

/// Keep a few of the deepest stacks verbatim. Deepest, because a one-frame stack says nothing
/// about frame order.
fn keep_examples<R: Read>(
    pending: &[(u32, SampleType)],
    parser: &NettraceParser<R>,
    out: &mut Vec<Vec<u64>>,
) {
    for (stack_id, _) in pending {
        if out.len() >= SAMPLE_STACKS_KEPT {
            return;
        }
        if let Some(frames) = parser.stacks().get(*stack_id) {
            if frames.len() > 4 {
                out.push(frames.to_vec());
            }
        }
    }
}

fn drain<R: Read>(
    pending: &mut Vec<(u32, SampleType)>,
    state: &mut ProfileState,
    parser: &NettraceParser<R>,
) {
    for (stack_id, sample_type) in pending.drain(..) {
        // Only `Error` is genuinely unusable. The type is *supposed* to separate managed
        // execution from native waits, but on macOS/arm64 every sample reports `External`, so
        // filtering on it would discard the entire profile. Parked threads are identified from
        // their stacks instead, once names exist.
        if sample_type == SampleType::Error {
            state.record_missing_stack();
            continue;
        }
        match parser.stacks().get(stack_id) {
            Some(frames) => state.record_stack(frames),
            None => state.record_missing_stack(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_events_are_recognised_from_both_providers() {
        // Methods jitted during the session come from the runtime provider; everything already
        // loaded arrives in the rundown.
        assert!(is_method_event(RUNTIME_PROVIDER, 143));
        assert!(is_method_event(RUNDOWN_PROVIDER, 144));
        assert!(is_method_event(RUNDOWN_PROVIDER, 141));
    }

    #[test]
    fn other_providers_and_ids_are_not_mistaken_for_methods() {
        assert!(!is_method_event(SAMPLE_PROFILER, event_id::THREAD_SAMPLE));
        assert!(!is_method_event("System.Runtime", 143));
        assert!(!is_method_event(RUNTIME_PROVIDER, 1));
        assert!(!is_method_event(RUNTIME_PROVIDER, 150));
    }
}
