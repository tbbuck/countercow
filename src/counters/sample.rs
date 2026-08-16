//! Turning `EventCounters` events into counter samples.
//!
//! Display names and units come off the wire rather than from a hardcoded table: the runtime
//! sends them in every payload, and the table that used to hold them (`KnownData.cs`) was deleted
//! from dotnet/diagnostics in 2024.

use crate::nettrace::blocks::RawEvent;
use crate::nettrace::metadata::EventMetadata;
use crate::nettrace::payload::decode_flat;
use crate::nettrace::reader::Result;

/// The event name every EventCounter payload arrives under, regardless of provider.
pub const EVENT_COUNTERS: &str = "EventCounters";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterKind {
    /// A gauge: `EventCounter` and `PollingCounter` report an average over the interval.
    Mean,
    /// A rate: `IncrementingEventCounter` and `IncrementingPollingCounter` report an increment
    /// per `rate_time_scale`.
    Rate,
}

#[derive(Debug, Clone)]
pub struct CounterSample {
    pub provider: String,
    pub name: String,
    pub display_name: String,
    pub display_units: String,
    pub value: f64,
    pub kind: CounterKind,
    pub interval_sec: f64,
    /// For rate counters: the window the increment covers, e.g. "00:00:01" or "00:01:00".
    /// The GC collection counters use a minute here while everything else uses a second.
    pub rate_time_scale: Option<String>,
    /// QPC timestamp from the event header.
    pub timestamp: u64,
}

impl CounterSample {
    /// Label for display, falling back to the counter's raw name when the runtime sends none.
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }

    /// Units for display. Rate counters with no declared units are counts.
    pub fn units(&self) -> &str {
        if !self.display_units.is_empty() {
            &self.display_units
        } else if self.kind == CounterKind::Rate {
            "count"
        } else {
            ""
        }
    }

    /// How often this counter's rate is expressed, in seconds, for rate counters.
    ///
    /// Parses the .NET `TimeSpan` "c" format ("00:01:00"). Returns `None` for gauges.
    pub fn rate_seconds(&self) -> Option<f64> {
        if self.kind != CounterKind::Rate {
            return None;
        }
        let scale = self.rate_time_scale.as_deref()?;
        let mut parts = scale.split(':');
        let hours: f64 = parts.next()?.parse().ok()?;
        let minutes: f64 = parts.next()?.parse().ok()?;
        let seconds: f64 = parts.next()?.parse().ok()?;
        let total = hours * 3600.0 + minutes * 60.0 + seconds;
        (total > 0.0).then_some(total)
    }
}

/// Extract a counter sample from an event, or `None` if the event is not an EventCounters payload.
///
/// A provider registers several distinct metadata ids all named `EventCounters` — one per counter
/// wrapper type — with different field lists, so this reads fields by name from the event's own
/// metadata rather than assuming a fixed shape.
pub fn extract(metadata: &EventMetadata, event: &RawEvent) -> Result<Option<CounterSample>> {
    if metadata.event_name != EVENT_COUNTERS {
        return Ok(None);
    }

    let fields = decode_flat(&metadata.fields, &event.payload)?;

    let string_field = |key: &str| -> String {
        fields.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned()
    };

    // "Mean" reads the Mean field; "Sum" reads Increment and is a rate.
    let counter_type = string_field("CounterType");
    let (kind, value) = match counter_type.as_str() {
        "Mean" => (CounterKind::Mean, fields.get("Mean").and_then(|v| v.as_f64())),
        "Sum" => (CounterKind::Rate, fields.get("Increment").and_then(|v| v.as_f64())),
        // An unrecognised counter type means we would be reporting the wrong field.
        _ => return Ok(None),
    };

    let Some(value) = value else {
        return Ok(None);
    };

    let name = string_field("Name");
    if name.is_empty() {
        return Ok(None);
    }

    let rate_time_scale = match kind {
        CounterKind::Rate => Some(string_field("DisplayRateTimeScale")).filter(|s| !s.is_empty()),
        CounterKind::Mean => None,
    };

    Ok(Some(CounterSample {
        provider: metadata.provider_name.clone(),
        name,
        display_name: string_field("DisplayName"),
        display_units: string_field("DisplayUnits"),
        value,
        kind,
        interval_sec: fields.get("IntervalSec").and_then(|v| v.as_f64()).unwrap_or(0.0),
        rate_time_scale,
        timestamp: event.timestamp,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nettrace::metadata::{Field, FieldType, TypeCode};

    fn scalar(name: &str, code: TypeCode) -> Field {
        Field { name: name.into(), ty: FieldType::Scalar(code) }
    }

    fn wrap(inner: Vec<Field>) -> Vec<Field> {
        let outer = vec![Field { name: "Payload".into(), ty: FieldType::Object(inner) }];
        vec![Field { name: String::new(), ty: FieldType::Object(outer) }]
    }

    fn wstr(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn metadata(provider: &str, event_name: &str, fields: Vec<Field>) -> EventMetadata {
        EventMetadata {
            metadata_id: 1,
            provider_name: provider.into(),
            event_id: 0,
            event_name: event_name.into(),
            keywords: 0,
            version: 0,
            level: 0,
            fields,
        }
    }

    /// The 12-field CounterPayload shape used by EventCounter and PollingCounter.
    fn mean_counter(name: &str, display: &str, units: &str, mean: f64) -> (EventMetadata, RawEvent) {
        let fields = wrap(vec![
            scalar("Name", TypeCode::String),
            scalar("DisplayName", TypeCode::String),
            scalar("Mean", TypeCode::Double),
            scalar("StandardDeviation", TypeCode::Double),
            scalar("Count", TypeCode::Int32),
            scalar("Min", TypeCode::Double),
            scalar("Max", TypeCode::Double),
            scalar("IntervalSec", TypeCode::Single),
            scalar("Series", TypeCode::String),
            scalar("CounterType", TypeCode::String),
            scalar("Metadata", TypeCode::String),
            scalar("DisplayUnits", TypeCode::String),
        ]);

        let mut payload = wstr(name);
        payload.extend(wstr(display));
        payload.extend_from_slice(&mean.to_le_bytes());
        payload.extend_from_slice(&0f64.to_le_bytes());
        payload.extend_from_slice(&1i32.to_le_bytes());
        payload.extend_from_slice(&mean.to_le_bytes());
        payload.extend_from_slice(&mean.to_le_bytes());
        payload.extend_from_slice(&1f32.to_le_bytes());
        payload.extend(wstr("Interval=1000"));
        payload.extend(wstr("Mean"));
        payload.extend(wstr(""));
        payload.extend(wstr(units));

        (
            metadata("System.Runtime", EVENT_COUNTERS, fields),
            RawEvent { metadata_id: 1, timestamp: 500, stack_id: 0, payload },
        )
    }

    /// The 9-field IncrementingCounterPayload shape.
    fn rate_counter(name: &str, increment: f64, scale: &str) -> (EventMetadata, RawEvent) {
        let fields = wrap(vec![
            scalar("Name", TypeCode::String),
            scalar("DisplayName", TypeCode::String),
            scalar("DisplayRateTimeScale", TypeCode::String),
            scalar("Increment", TypeCode::Double),
            scalar("IntervalSec", TypeCode::Single),
            scalar("Metadata", TypeCode::String),
            scalar("Series", TypeCode::String),
            scalar("CounterType", TypeCode::String),
            scalar("DisplayUnits", TypeCode::String),
        ]);

        let mut payload = wstr(name);
        payload.extend(wstr(name));
        payload.extend(wstr(scale));
        payload.extend_from_slice(&increment.to_le_bytes());
        payload.extend_from_slice(&1f32.to_le_bytes());
        payload.extend(wstr(""));
        payload.extend(wstr("Interval=1000"));
        payload.extend(wstr("Sum"));
        payload.extend(wstr(""));

        (
            metadata("System.Runtime", EVENT_COUNTERS, fields),
            RawEvent { metadata_id: 1, timestamp: 900, stack_id: 0, payload },
        )
    }

    #[test]
    fn extracts_a_mean_counter() {
        let (md, event) = mean_counter("cpu-usage", "CPU Usage", "%", 12.5);
        let sample = extract(&md, &event).unwrap().unwrap();

        assert_eq!(sample.name, "cpu-usage");
        assert_eq!(sample.label(), "CPU Usage");
        assert_eq!(sample.units(), "%");
        assert_eq!(sample.value, 12.5);
        assert_eq!(sample.kind, CounterKind::Mean);
        assert_eq!(sample.interval_sec, 1.0);
        assert_eq!(sample.timestamp, 500);
        assert_eq!(sample.rate_seconds(), None);
    }

    #[test]
    fn extracts_a_rate_counter_reading_increment_not_mean() {
        let (md, event) = rate_counter("exception-count", 4.0, "00:00:01");
        let sample = extract(&md, &event).unwrap().unwrap();

        assert_eq!(sample.value, 4.0);
        assert_eq!(sample.kind, CounterKind::Rate);
        assert_eq!(sample.rate_seconds(), Some(1.0));
        // A rate with no declared units is a count.
        assert_eq!(sample.units(), "count");
    }

    #[test]
    fn gc_collection_counters_report_a_one_minute_scale() {
        // These are the counters whose rate window differs from every other one.
        let (md, event) = rate_counter("gen-0-gc-count", 3.0, "00:01:00");
        let sample = extract(&md, &event).unwrap().unwrap();
        assert_eq!(sample.rate_seconds(), Some(60.0));
    }

    #[test]
    fn falls_back_to_the_raw_name_when_no_display_name_is_sent() {
        let (md, event) = mean_counter("odd-counter", "", "", 1.0);
        let sample = extract(&md, &event).unwrap().unwrap();
        assert_eq!(sample.label(), "odd-counter");
        assert_eq!(sample.units(), "");
    }

    #[test]
    fn ignores_events_that_are_not_counters() {
        let md = metadata("System.Runtime", "GCStart", vec![]);
        let event = RawEvent { metadata_id: 1, timestamp: 0, stack_id: 0, payload: vec![] };
        assert!(extract(&md, &event).unwrap().is_none());
    }

    #[test]
    fn an_unknown_counter_type_is_skipped_rather_than_misread() {
        let (md, mut event) = mean_counter("cpu-usage", "CPU Usage", "%", 12.5);
        // Rewrite "Mean" to an unrecognised counter type of the same length.
        let needle = wstr("Mean");
        let pos = event
            .payload
            .windows(needle.len())
            .rposition(|w| w == needle.as_slice())
            .unwrap();
        event.payload[pos..pos + needle.len()].copy_from_slice(&wstr("Xxxx"));

        assert!(extract(&md, &event).unwrap().is_none());
    }
}
