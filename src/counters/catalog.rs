//! Presentation rules the wire format does not carry: how to format a value, and which panel a
//! counter belongs to.
//!
//! Display names and units arrive in the payload, so this deliberately holds neither. What it
//! does hold is the knowledge that the runtime is *inconsistent* about units — `gc-heap-size` is
//! decimal MB while `gen-0-size` is raw bytes — and that a GC panel wants them on one scale.

use super::sample::{CounterKind, CounterSample};

pub const SYSTEM_RUNTIME: &str = "System.Runtime";
pub const ASPNET_HOSTING: &str = "Microsoft.AspNetCore.Hosting";
pub const KESTREL: &str = "Microsoft-AspNetCore-Server-Kestrel";
pub const NET_HTTP: &str = "System.Net.Http";

/// Counters the runtime reports in decimal megabytes (bytes ÷ 1,000,000) rather than bytes.
///
/// Everything else carrying a "B" unit is raw. Mixing the two on one axis is wrong by six orders
/// of magnitude, and a heap-size-vs-generation-size chart wants exactly that mix.
const MEGABYTE_COUNTERS: &[&str] =
    &["working-set", "gc-heap-size", "gc-committed", "gen-0-gc-budget"];

/// Collections per interval, youngest generation first.
///
/// Increment counters: each reading is how many collections happened during that interval, which
/// is what a timeline of GC activity wants. The panel that reports them as rates instead reads
/// zero on anything but a busy process.
pub const GC_COUNT_COUNTERS: &[&str] = &["gen-0-gc-count", "gen-1-gc-count", "gen-2-gc-count"];

/// The generation sizes, largest-lived first, as they should appear in the GC panel.
pub const GENERATION_COUNTERS: &[&str] =
    &["gen-0-size", "gen-1-size", "gen-2-size", "loh-size", "poh-size"];

/// Factor converting this counter's reported value into bytes, or `None` if it is not a memory
/// counter. Exposed separately from [`bytes_value`] so a whole history can be scaled consistently.
pub fn byte_scale(sample: &CounterSample) -> Option<f64> {
    if sample.provider != SYSTEM_RUNTIME {
        return None;
    }
    if MEGABYTE_COUNTERS.contains(&sample.name.as_str()) {
        // Decimal MB, matching how the runtime computes it.
        return Some(1_000_000.0);
    }
    if sample.units() == "B" && sample.kind == CounterKind::Mean {
        return Some(1.0);
    }
    None
}

/// Value normalised to bytes, for counters that measure memory. `None` for everything else.
pub fn bytes_value(sample: &CounterSample) -> Option<f64> {
    Some(sample.value * byte_scale(sample)?)
}

/// Human-readable byte size. Uses binary multiples, which is what memory tooling conventionally
/// shows, regardless of how the runtime computed the number.
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.abs();
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let sign = if bytes < 0.0 { "-" } else { "" };
    if unit == 0 {
        format!("{sign}{value:.0} {}", UNITS[unit])
    } else if value < 10.0 {
        format!("{sign}{value:.2} {}", UNITS[unit])
    } else {
        format!("{sign}{value:.1} {}", UNITS[unit])
    }
}

/// A plain count with thousands separators, dropping a meaningless fractional part.
pub fn format_count(value: f64) -> String {
    if value.fract().abs() > f64::EPSILON && value.abs() < 1000.0 {
        return format!("{value:.2}");
    }

    let rounded = value.round().abs() as u64;
    let digits = rounded.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if value < 0.0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// The value as it should read on the dashboard, units included.
pub fn format_sample(sample: &CounterSample) -> String {
    if let Some(bytes) = bytes_value(sample) {
        return format_bytes(bytes);
    }

    match sample.units() {
        "%" => format!("{:.1} %", sample.value),
        "ms" => format!("{:.1} ms", sample.value),
        "B" => format!("{}/s", format_bytes(sample.value)),
        "MB" => format_bytes(sample.value * 1_000_000.0),
        _ => match sample.kind {
            CounterKind::Rate => match sample.rate_seconds() {
                // The GC collection counters are the only ones on a minute window; saying "/s"
                // for them would understate by 60x.
                Some(secs) if secs >= 60.0 => format!("{}/min", format_count(sample.value)),
                _ => format!("{}/s", format_count(sample.value)),
            },
            CounterKind::Mean => format_count(sample.value),
        },
    }
}

/// Percentage counters can drive a gauge; everything else cannot.
pub fn percentage(sample: &CounterSample) -> Option<f64> {
    (sample.units() == "%").then(|| sample.value.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, units: &str, value: f64, kind: CounterKind) -> CounterSample {
        CounterSample {
            provider: SYSTEM_RUNTIME.into(),
            name: name.into(),
            display_name: name.into(),
            display_units: units.into(),
            value,
            kind,
            interval_sec: 1.0,
            rate_time_scale: match kind {
                CounterKind::Rate => Some("00:00:01".into()),
                CounterKind::Mean => None,
            },
            timestamp: 0,
        }
    }

    #[test]
    fn megabyte_counters_are_scaled_to_bytes() {
        // 44.5481 MB as the runtime reports it is ~44.5 million bytes, not 44.5 bytes.
        let ws = sample("working-set", "MB", 44.5481, CounterKind::Mean);
        assert_eq!(bytes_value(&ws), Some(44_548_100.0));
    }

    #[test]
    fn byte_counters_pass_through_unscaled() {
        let gen2 = sample("gen-2-size", "B", 1_313_296.0, CounterKind::Mean);
        assert_eq!(bytes_value(&gen2), Some(1_313_296.0));
    }

    #[test]
    fn heap_size_and_generation_sizes_end_up_comparable() {
        // The whole point: these two must be plottable on one axis.
        let heap = sample("gc-heap-size", "MB", 4.1432, CounterKind::Mean);
        let gen2 = sample("gen-2-size", "B", 1_313_296.0, CounterKind::Mean);

        let heap_bytes = bytes_value(&heap).unwrap();
        let gen2_bytes = bytes_value(&gen2).unwrap();
        assert!(heap_bytes > gen2_bytes, "heap should exceed one generation");
        assert!(heap_bytes / gen2_bytes < 10.0, "and be within an order of magnitude");
    }

    #[test]
    fn rate_counters_are_not_treated_as_memory() {
        // alloc-rate is bytes per second, not a memory level.
        let alloc = sample("alloc-rate", "B", 212_656.0, CounterKind::Rate);
        assert_eq!(bytes_value(&alloc), None);
        assert_eq!(format_sample(&alloc), "207.7 KiB/s");
    }

    #[test]
    fn formats_byte_sizes_readably() {
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1024.0), "1.00 KiB");
        assert_eq!(format_bytes(1_313_296.0), "1.25 MiB");
        assert_eq!(format_bytes(44_548_100.0), "42.5 MiB");
        assert_eq!(format_bytes(5_000_000_000.0), "4.66 GiB");
    }

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(format_count(0.0), "0");
        assert_eq!(format_count(91.0), "91");
        assert_eq!(format_count(2317.0), "2,317");
        assert_eq!(format_count(1_313_296.0), "1,313,296");
        assert_eq!(format_count(0.25), "0.25");
    }

    #[test]
    fn gc_collection_counters_read_per_minute() {
        let mut gc = sample("gen-0-gc-count", "", 3.0, CounterKind::Rate);
        gc.rate_time_scale = Some("00:01:00".into());
        assert_eq!(format_sample(&gc), "3/min");

        let exceptions = sample("exception-count", "", 5.0, CounterKind::Rate);
        assert_eq!(format_sample(&exceptions), "5/s");
    }

    #[test]
    fn formats_percentages_and_durations() {
        assert_eq!(format_sample(&sample("cpu-usage", "%", 12.54, CounterKind::Mean)), "12.5 %");
        assert_eq!(
            format_sample(&sample("total-pause-time-by-gc", "ms", 44.5875, CounterKind::Mean)),
            "44.6 ms"
        );
    }

    #[test]
    fn percentage_extraction_is_limited_to_percent_counters() {
        assert_eq!(percentage(&sample("cpu-usage", "%", 12.5, CounterKind::Mean)), Some(12.5));
        assert_eq!(percentage(&sample("assembly-count", "", 91.0, CounterKind::Mean)), None);
        // Guard against a stray reading driving a gauge past full.
        assert_eq!(percentage(&sample("cpu-usage", "%", 140.0, CounterKind::Mean)), Some(100.0));
    }

    #[test]
    fn non_runtime_providers_have_no_byte_interpretation() {
        let mut requests = sample("total-requests", "", 5.0, CounterKind::Mean);
        requests.provider = ASPNET_HOSTING.into();
        assert_eq!(bytes_value(&requests), None);
        assert_eq!(format_sample(&requests), "5");
    }
}
