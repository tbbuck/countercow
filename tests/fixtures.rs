//! Parser regression tests against real captured nettrace streams.
//!
//! Synthetic tests confirm the parser matches my *understanding* of the format; these confirm it
//! matches what runtimes actually emit. That distinction matters here because a misread field
//! produces plausible numbers rather than an error.
//!
//! Regenerate with `cargo run --example capture -- <pid> <path> <seconds>`.

use std::collections::{BTreeSet, HashMap};

use countercow::counters::sample::{self, CounterKind, CounterSample};
use countercow::nettrace::blocks::NettraceParser;

/// Captured from a net9.0 ASP.NET Core app under light load.
const ASPNET_NET9: &[u8] = include_bytes!("fixtures/aspnet-net9.nettrace");
/// Captured from a net10.0 non-ASP.NET process.
const GENERIC_NET10: &[u8] = include_bytes!("fixtures/generic-net10.nettrace");

struct Parsed {
    samples: Vec<CounterSample>,
    process_id: i32,
    qpc_frequency: i64,
    metadata_count: usize,
}

fn parse(fixture: &[u8]) -> Parsed {
    let mut parser = NettraceParser::new(std::io::Cursor::new(fixture)).expect("valid preamble");
    let mut samples = Vec::new();

    while let Some(batch) = parser.next_events().expect("stream parses cleanly") {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };
            if let Some(s) = sample::extract(metadata, &event).expect("payload decodes") {
                samples.push(s);
            }
        }
    }

    let trace = parser.trace_info();
    Parsed {
        process_id: trace.process_id,
        qpc_frequency: trace.qpc_frequency,
        metadata_count: parser.metadata().len(),
        samples,
    }
}

fn names_by_provider(parsed: &Parsed) -> HashMap<String, BTreeSet<String>> {
    let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
    for sample in &parsed.samples {
        out.entry(sample.provider.clone()).or_default().insert(sample.name.clone());
    }
    out
}

fn latest(parsed: &Parsed, name: &str) -> CounterSample {
    parsed
        .samples
        .iter()
        .filter(|s| s.name == name)
        .next_back()
        .unwrap_or_else(|| panic!("no sample named {name}"))
        .clone()
}

#[test]
fn trace_header_is_read_from_a_real_stream() {
    let parsed = parse(ASPNET_NET9);
    assert!(parsed.process_id > 0, "process id should be populated");
    // Unix runtimes report nanosecond QPC.
    assert_eq!(parsed.qpc_frequency, 1_000_000_000);
    assert!(parsed.metadata_count > 0, "metadata should be registered");
}

#[test]
fn every_documented_system_runtime_counter_is_decoded() {
    let parsed = parse(ASPNET_NET9);
    let by_provider = names_by_provider(&parsed);
    let runtime = by_provider.get("System.Runtime").expect("System.Runtime counters");

    // The full .NET 9 set. If the runtime adds counters this test still passes; if the parser
    // starts dropping any, it fails.
    for expected in [
        "cpu-usage",
        "working-set",
        "gc-heap-size",
        "gen-0-gc-count",
        "gen-1-gc-count",
        "gen-2-gc-count",
        "gen-0-gc-budget",
        "threadpool-thread-count",
        "monitor-lock-contention-count",
        "threadpool-queue-length",
        "threadpool-completed-items-count",
        "alloc-rate",
        "active-timer-count",
        "gc-fragmentation",
        "gc-committed",
        "exception-count",
        "time-in-gc",
        "total-pause-time-by-gc",
        "gen-0-size",
        "gen-1-size",
        "gen-2-size",
        "loh-size",
        "poh-size",
        "assembly-count",
        "il-bytes-jitted",
        "methods-jitted-count",
        "time-in-jit",
    ] {
        assert!(runtime.contains(expected), "missing counter {expected}");
    }
    assert!(runtime.len() >= 27, "expected at least 27 counters, got {}", runtime.len());
}

#[test]
fn aspnet_providers_appear_only_for_an_aspnet_process() {
    // This is the signal the dashboard uses to decide which panels to show. Subscribing to a
    // provider no EventSource implements succeeds silently and yields nothing, so "did events
    // arrive" is the only reliable test.
    let aspnet = names_by_provider(&parse(ASPNET_NET9));
    assert!(aspnet.contains_key("Microsoft.AspNetCore.Hosting"));
    assert!(aspnet.contains_key("Microsoft-AspNetCore-Server-Kestrel"));

    let generic = names_by_provider(&parse(GENERIC_NET10));
    assert!(generic.contains_key("System.Runtime"), "runtime counters always present");
    assert!(
        !generic.contains_key("Microsoft.AspNetCore.Hosting"),
        "a non-ASP.NET process must not report hosting counters"
    );
}

#[test]
fn hosting_and_kestrel_counters_are_named_as_expected() {
    let by_provider = names_by_provider(&parse(ASPNET_NET9));

    let hosting = &by_provider["Microsoft.AspNetCore.Hosting"];
    for expected in ["requests-per-second", "total-requests", "current-requests", "failed-requests"]
    {
        assert!(hosting.contains(expected), "missing hosting counter {expected}");
    }

    let kestrel = &by_provider["Microsoft-AspNetCore-Server-Kestrel"];
    for expected in ["total-connections", "current-connections", "connection-queue-length"] {
        assert!(kestrel.contains(expected), "missing kestrel counter {expected}");
    }
}

#[test]
fn counter_kinds_and_units_come_off_the_wire() {
    let parsed = parse(ASPNET_NET9);

    let working_set = latest(&parsed, "working-set");
    assert_eq!(working_set.kind, CounterKind::Mean);
    assert_eq!(working_set.label(), "Working Set");
    assert_eq!(working_set.units(), "MB");
    assert!(working_set.value > 0.0);

    let alloc_rate = latest(&parsed, "alloc-rate");
    assert_eq!(alloc_rate.kind, CounterKind::Rate);
    assert_eq!(alloc_rate.units(), "B");
    assert_eq!(alloc_rate.rate_seconds(), Some(1.0));

    // The counter whose display name the published docs get wrong.
    assert_eq!(latest(&parsed, "methods-jitted-count").label(), "Number of Methods Jitted");
}

#[test]
fn gc_collection_counters_use_a_one_minute_rate_window() {
    // Every other incrementing counter is per-second. Rendering these as "/sec" would understate
    // them by 60x.
    let parsed = parse(ASPNET_NET9);
    for name in ["gen-0-gc-count", "gen-1-gc-count", "gen-2-gc-count"] {
        let sample = latest(&parsed, name);
        assert_eq!(sample.kind, CounterKind::Rate);
        assert_eq!(sample.rate_seconds(), Some(60.0), "{name} should use a minute window");
    }

    assert_eq!(latest(&parsed, "exception-count").rate_seconds(), Some(1.0));
}

#[test]
fn byte_and_megabyte_counters_are_distinguishable() {
    // The runtime mixes units within one provider: heap size is decimal MB while generation
    // sizes are raw bytes. Plotting them on a shared axis without normalising is wrong by
    // six orders of magnitude.
    let parsed = parse(ASPNET_NET9);
    assert_eq!(latest(&parsed, "gc-heap-size").units(), "MB");
    assert_eq!(latest(&parsed, "gc-committed").units(), "MB");
    assert_eq!(latest(&parsed, "gen-0-size").units(), "B");
    assert_eq!(latest(&parsed, "gen-2-size").units(), "B");
    assert_eq!(latest(&parsed, "loh-size").units(), "B");
}

#[test]
fn samples_advance_over_time() {
    let parsed = parse(ASPNET_NET9);
    let timestamps: Vec<u64> = parsed
        .samples
        .iter()
        .filter(|s| s.name == "cpu-usage")
        .map(|s| s.timestamp)
        .collect();

    assert!(timestamps.len() >= 2, "fixture should span several intervals");
    assert!(
        timestamps.windows(2).all(|w| w[1] > w[0]),
        "timestamps must increase monotonically"
    );
}

#[test]
fn net10_fixture_parses_with_the_same_code_path() {
    // Metadata tag usage differs between runtime versions; this guards the V5 tag handling.
    let parsed = parse(GENERIC_NET10);
    assert!(parsed.metadata_count > 0);
    assert!(!parsed.samples.is_empty());
    assert!(parsed.samples.iter().any(|s| s.name == "cpu-usage"));
}

#[test]
fn a_truncated_capture_errors_rather_than_returning_partial_nonsense() {
    let truncated = &ASPNET_NET9[..ASPNET_NET9.len() / 2];
    let mut parser = NettraceParser::new(std::io::Cursor::new(truncated)).unwrap();

    let mut hit_error = false;
    loop {
        match parser.next_events() {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => {
                hit_error = true;
                break;
            }
        }
    }
    assert!(hit_error, "a mid-block truncation should surface as an error");
}
