//! Driving a live counter session: subscribe, parse, and yield samples.

use std::fmt;
use std::ops::ControlFlow;
use std::path::Path;

use crate::ipc::commands::{self, CommandError, Provider, TraceConfig};
use crate::nettrace::blocks::NettraceParser;
use crate::nettrace::reader::ParseError;

use super::sample::{self, CounterSample};

pub const DEFAULT_INTERVAL_SECS: f64 = 1.0;

/// Buffer the runtime keeps for our session. Counters are low-volume; this is ample.
const CIRCULAR_BUFFER_MB: u32 = 10;

/// EventLevel.Informational.
const LEVEL_INFORMATIONAL: u32 = 4;

/// Providers countercow subscribes to.
///
/// Keywords are 0 throughout, matching what dotnet-counters sends today. EventCounter payload
/// events declare no keywords so any mask would pass, but a broad mask risks switching on
/// unrelated high-volume events from providers that do use keyword bits.
///
/// Note the Kestrel name: the EventCounter provider is hyphenated. The dotted
/// `Microsoft.AspNetCore.Server.Kestrel` is the .NET 8+ *Meter*, a different mechanism —
/// subscribing to it here is not an error, it just silently yields nothing.
pub const PROVIDERS: &[&str] = &[
    "System.Runtime",
    "Microsoft.AspNetCore.Hosting",
    "Microsoft-AspNetCore-Server-Kestrel",
    "System.Net.Http",
];

/// Providers whose presence identifies an ASP.NET Core host.
pub const ASPNET_PROVIDERS: &[&str] =
    &["Microsoft.AspNetCore.Hosting", "Microsoft-AspNetCore-Server-Kestrel"];

pub fn trace_config(interval_secs: f64) -> TraceConfig {
    TraceConfig {
        circular_buffer_mb: CIRCULAR_BUFFER_MB,
        providers: PROVIDERS
            .iter()
            .map(|name| Provider {
                name: (*name).to_owned(),
                keywords: 0,
                level: LEVEL_INFORMATIONAL,
                // InvariantCulture formatting: always a '.' decimal separator.
                filter_data: format!("EventCounterIntervalSec={interval_secs}"),
            })
            .collect(),
    }
}

#[derive(Debug)]
pub enum SessionError {
    Command(CommandError),
    Parse(ParseError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Command(e) => e.fmt(f),
            SessionError::Parse(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Command(e) => Some(e),
            SessionError::Parse(e) => Some(e),
        }
    }
}

impl From<CommandError> for SessionError {
    fn from(e: CommandError) -> Self {
        SessionError::Command(e)
    }
}
impl From<ParseError> for SessionError {
    fn from(e: ParseError) -> Self {
        SessionError::Parse(e)
    }
}

/// Stream counter samples until `on_sample` asks to stop or the process exits.
///
/// On stop, the session is closed over a second connection and the original stream is then
/// drained to EOF — the runtime keeps writing into it after acknowledging the stop, and
/// abandoning it early truncates the trace.
pub fn stream<F>(
    socket: &Path,
    interval_secs: f64,
    mut on_sample: F,
) -> Result<(), SessionError>
where
    F: FnMut(CounterSample) -> ControlFlow<()>,
{
    let session = commands::start_tracing(socket, &trace_config(interval_secs))?;
    let session_id = session.session_id;
    let mut parser = NettraceParser::new(session.stream)?;
    let mut stopping = false;

    loop {
        let batch = match parser.next_events() {
            Ok(Some(batch)) => batch,
            Ok(None) => return Ok(()),
            // Once we have asked to stop, a ragged tail is expected rather than a failure.
            Err(_) if stopping => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        if stopping {
            // Draining after stop: keep reading, but no longer report.
            continue;
        }

        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                // An event whose metadata we have not seen. Skipping is correct: the payload is
                // undecodable without it, and counter metadata always precedes its events.
                continue;
            };

            let Some(sample) = sample::extract(metadata, &event)? else {
                continue;
            };

            // Another tool may be watching the same process at a different cadence; its payloads
            // arrive on our session too. Ignore anything not on our interval.
            if sample.interval_sec > 0.0
                && (sample.interval_sec - interval_secs).abs() > interval_secs * 0.5
            {
                continue;
            }

            if on_sample(sample).is_break() {
                commands::stop_tracing(socket, session_id)?;
                stopping = true;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kestrel_provider_is_hyphenated_not_dotted() {
        // The dotted form is the Meter name and yields no EventCounters at all.
        assert!(PROVIDERS.contains(&"Microsoft-AspNetCore-Server-Kestrel"));
        assert!(!PROVIDERS.contains(&"Microsoft.AspNetCore.Server.Kestrel"));
    }

    #[test]
    fn every_provider_requests_the_counter_interval() {
        let config = trace_config(1.0);
        assert_eq!(config.providers.len(), PROVIDERS.len());
        for provider in &config.providers {
            assert_eq!(provider.filter_data, "EventCounterIntervalSec=1");
            assert_eq!(provider.keywords, 0);
            assert_eq!(provider.level, LEVEL_INFORMATIONAL);
        }
    }

    #[test]
    fn fractional_intervals_use_an_invariant_decimal_point() {
        let config = trace_config(0.5);
        assert_eq!(config.providers[0].filter_data, "EventCounterIntervalSec=0.5");
    }

    #[test]
    fn aspnet_providers_are_a_subset_of_the_subscription() {
        for provider in ASPNET_PROVIDERS {
            assert!(PROVIDERS.contains(provider), "{provider} must be subscribed");
        }
    }
}
