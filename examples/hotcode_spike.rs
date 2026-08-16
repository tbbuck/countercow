//! Feasibility spike for a hot-code (sampling profiler) view.
//!
//! Turning CPU samples into method names needs three things that counters do not: stack blocks in
//! the stream, a rundown naming every loaded method, and an address-to-method map to join them.
//! This measures whether each is actually available, and at what cost, before committing to build
//! it.
//!
//! ```text
//! cargo run --example hotcode_spike -- <pid> [seconds]
//! ```

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use countercow::ipc::commands::{self, Provider, TraceConfig};
use countercow::ipc::discovery;
use countercow::nettrace::blocks::NettraceParser;

/// The sampling profiler: one event per thread per millisecond, each referencing a stack.
const SAMPLE_PROFILER: &str = "Microsoft-DotNETCore-SampleProfiler";
const RUNTIME: &str = "Microsoft-Windows-DotNETRuntime";

/// Jit keyword: method load events for anything jitted during the session.
const KEYWORD_JIT: u64 = 0x10;
/// Loader keyword: module and assembly loads, needed to attribute addresses to modules.
const KEYWORD_LOADER: u64 = 0x8;

/// MethodLoadVerbose / MethodDCStartVerbose and friends, which carry method names.
const METHOD_LOAD_VERBOSE: i32 = 143;
const METHOD_DC_START_VERBOSE: i32 = 141;
const METHOD_DC_END_VERBOSE: i32 = 142;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: hotcode_spike <pid> [seconds]")?.parse()?;
    let seconds: u64 = args.next().unwrap_or_else(|| "5".into()).parse()?;

    let found = discovery::discover()?;
    let process = found
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    println!("spiking hot-code feasibility on {} (pid {pid}) for {seconds}s\n", process.name);

    // Rundown is the crux: without it, stack frames are bare addresses. It is requested as part
    // of the session and delivered when the session *stops*.
    let config = TraceConfig {
        circular_buffer_mb: 256,
        request_rundown: true,
        providers: vec![
            Provider {
                name: SAMPLE_PROFILER.to_owned(),
                keywords: 0,
                level: 4,
                filter_data: String::new(),
            },
            Provider {
                name: RUNTIME.to_owned(),
                keywords: KEYWORD_JIT | KEYWORD_LOADER,
                level: 5,
                filter_data: String::new(),
            },
        ],
    };

    let session = commands::start_tracing(&process.socket, &config)?;
    let session_id = session.session_id;

    let stop_socket = process.socket.clone();
    let stop_at = Instant::now() + Duration::from_secs(seconds);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        let _ = commands::stop_tracing(&stop_socket, session_id);
    });

    let started = Instant::now();
    let mut parser = NettraceParser::new(session.stream)?;
    let mut event_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples = 0usize;
    let mut with_stacks = 0usize;
    let mut method_events = 0usize;
    let mut method_events_after_stop = 0usize;
    let mut first_method_name: Option<String> = None;

    while let Ok(Some(batch)) = parser.next_events() {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };

            let key = format!("{} id {}", metadata.provider_name, metadata.event_id);
            *event_counts.entry(key).or_default() += 1;

            if metadata.provider_name == SAMPLE_PROFILER {
                samples += 1;
                if event.stack_id != 0 {
                    with_stacks += 1;
                }
            }

            if matches!(
                metadata.event_id,
                METHOD_LOAD_VERBOSE | METHOD_DC_START_VERBOSE | METHOD_DC_END_VERBOSE
            ) && metadata.provider_name == RUNTIME
            {
                method_events += 1;
                if Instant::now() > stop_at {
                    method_events_after_stop += 1;
                }
                if first_method_name.is_none() {
                    first_method_name = extract_method_name(&event.payload);
                }
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    println!("--- feasibility ---");
    println!("elapsed:              {elapsed:.1}s");
    println!("CPU samples:          {samples} ({:.0}/s)", samples as f64 / elapsed);
    println!("  with a stack id:    {with_stacks}");
    println!("method name events:   {method_events}");
    println!("  arriving after stop:{method_events_after_stop}");
    if let Some(name) = &first_method_name {
        println!("  example name:       {name}");
    }

    println!("\n--- blocks in the stream ---");
    for (kind, count) in parser.block_counts() {
        println!("{kind:<16} {count}");
    }

    println!("\n--- busiest events ---");
    let mut by_count: Vec<_> = event_counts.iter().collect();
    by_count.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
    for (name, count) in by_count.iter().take(8) {
        println!("{name:<45} {count}");
    }

    Ok(())
}

/// MethodLoadVerbose payloads end with namespace, name and signature strings; pull the first
/// readable one out to prove names are really present.
fn extract_method_name(payload: &[u8]) -> Option<String> {
    let mut offset = 0;
    while offset + 8 <= payload.len() {
        let mut units = Vec::new();
        let mut cursor = offset;
        while cursor + 2 <= payload.len() {
            let unit = u16::from_le_bytes([payload[cursor], payload[cursor + 1]]);
            if unit == 0 {
                break;
            }
            if !(0x20..0x7f).contains(&unit) {
                units.clear();
                break;
            }
            units.push(unit);
            cursor += 2;
        }
        if units.len() >= 6 {
            return Some(String::from_utf16_lossy(&units));
        }
        offset += 2;
    }
    None
}
