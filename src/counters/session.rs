//! Driving a live counter session: subscribe, parse, and yield samples.

use std::fmt;
use std::ops::ControlFlow;
use std::path::Path;

use crate::ipc::commands::{self, CommandError, Provider, TraceConfig, TraceSession};
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
        // Rundown emits method/assembly/type maps: pure overhead for counters, and
        // dotnet-counters itself asks for none.
        request_rundown: false,
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

/// Open a counter session. The returned stream carries nettrace from this point on.
pub fn start(socket: &Path, interval_secs: f64) -> Result<TraceSession, SessionError> {
    Ok(commands::start_tracing(socket, &trace_config(interval_secs))?)
}

/// Ask the runtime to end a session.
///
/// This goes out on a fresh connection — the protocol permits one command per connection — and
/// causes the streaming socket to reach EOF, which is how [`run`] learns to finish.
pub fn stop(socket: &Path, session_id: u64) -> Result<(), SessionError> {
    Ok(commands::stop_tracing(socket, session_id)?)
}

/// Parse a session's stream, reporting counter samples until the callback breaks, the process
/// exits, or the session is stopped from elsewhere.
pub fn run<F>(
    stream: impl std::io::Read,
    interval_secs: f64,
    mut on_sample: F,
) -> Result<(), SessionError>
where
    F: FnMut(CounterSample) -> ControlFlow<()>,
{
    let mut parser = NettraceParser::new(stream)?;

    loop {
        let Some(batch) = parser.next_events()? else {
            return Ok(());
        };

        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                // An event whose metadata we have not seen. Skipping is correct: the payload is
                // undecodable without it, and counter metadata always precedes its events.
                continue;
            };

            let Some(sample) = sample::extract(metadata, &event)? else {
                continue;
            };

            if !ours(sample.interval_sec, interval_secs) {
                continue;
            }

            if on_sample(sample).is_break() {
                return Ok(());
            }
        }
    }
}

/// Whether a counter payload belongs to the session we asked for.
///
/// Another tool may be watching the same process at a different cadence; its payloads arrive on
/// our session too, and at a faster cadence they would swamp ours. Anything clearly faster than we
/// asked for is therefore rejected.
///
/// Nothing slower is, though a symmetric window would reject that too. The runtime stamps the
/// interval it measured rather than the one it was asked for, so a payload arriving late is our
/// own data from a process that was busy, not somebody else's arriving early — and discarding it
/// leaves a panel reading as though the counter does not exist, which is a far more confusing
/// failure than a duplicate point from a slower session.
fn ours(reported_interval: f64, requested: f64) -> bool {
    reported_interval <= 0.0 || reported_interval >= requested * 0.6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payload_at_the_rate_we_asked_for_is_ours() {
        assert!(ours(1.0, 1.0));
        assert!(ours(0.25, 0.25));
    }

    #[test]
    fn a_late_payload_is_still_ours() {
        // The runtime reports what it measured, and a busy process reports late. Dropping these
        // is what leaves a counter looking as though it does not exist.
        assert!(ours(1.4, 1.0));
        assert!(ours(3.0, 1.0), "even a badly delayed one");
    }

    #[test]
    fn a_faster_cadence_belongs_to_someone_else() {
        // A second tool watching at 0.25s while we asked for 1s would otherwise deliver four
        // times our sample rate into our history.
        assert!(!ours(0.25, 1.0));
        assert!(!ours(0.5, 1.0));
    }

    #[test]
    fn an_unstamped_payload_is_kept() {
        // The first payload of a session carries no interval; rejecting it would drop a counter's
        // opening reading for no reason.
        assert!(ours(0.0, 1.0));
    }

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
