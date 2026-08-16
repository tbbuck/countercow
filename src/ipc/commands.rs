//! The Diagnostics IPC commands countercow needs: process identity, and starting/stopping an
//! EventPipe counter session.

use std::fmt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::decode::{PayloadReader, TruncatedPayload};
use super::encode::PayloadWriter;
use super::frame::{self, command_set, eventpipe, process, IpcError, UNKNOWN_COMMAND};
use super::transport::{connect, ConnectError};

#[derive(Debug)]
pub enum CommandError {
    Connect(ConnectError),
    Ipc(IpcError),
    Payload(TruncatedPayload),
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::Connect(e) => e.fmt(f),
            CommandError::Ipc(e) => e.fmt(f),
            CommandError::Payload(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CommandError::Connect(e) => Some(e),
            CommandError::Ipc(e) => Some(e),
            CommandError::Payload(e) => Some(e),
        }
    }
}

impl From<ConnectError> for CommandError {
    fn from(e: ConnectError) -> Self {
        CommandError::Connect(e)
    }
}
impl From<IpcError> for CommandError {
    fn from(e: IpcError) -> Self {
        CommandError::Ipc(e)
    }
}
impl From<TruncatedPayload> for CommandError {
    fn from(e: TruncatedPayload) -> Self {
        CommandError::Payload(e)
    }
}

type Result<T> = std::result::Result<T, CommandError>;

/// What the runtime reports about itself.
#[derive(Debug, Clone, Default)]
pub struct ProcessInfo {
    pub pid: u64,
    pub command_line: String,
    pub os: String,
    pub arch: String,
    /// Entry assembly name — a far better display name than "dotnet". Absent on ProcessInfo v1.
    pub assembly_name: Option<String>,
    /// e.g. "9.0.7", or "6.0.0-preview.6.12345". Absent on ProcessInfo v1.
    pub clr_version: Option<String>,
    /// e.g. "osx-arm64". ProcessInfo v3 only.
    pub runtime_identifier: Option<String>,
}

impl ProcessInfo {
    /// Major/minor of the CLR version, for gating counters that only exist on newer runtimes.
    pub fn clr_major_minor(&self) -> Option<(u32, u32)> {
        let raw = self.clr_version.as_deref()?;
        // Strip build metadata then prerelease, as the reference client does.
        let core = raw.split('+').next()?.split('-').next()?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        Some((major, minor))
    }

    /// A short label like "net9.0" for the dashboard header.
    pub fn framework_label(&self) -> Option<String> {
        let (major, minor) = self.clr_major_minor()?;
        Some(format!("net{major}.{minor}"))
    }

    /// The name worth showing, given the OS-level process name.
    ///
    /// Framework-dependent apps are launched through the host, so the OS only knows them as
    /// "dotnet"; the runtime knows its own entry assembly, which is what a person recognises.
    /// A self-contained app has a real executable name already, and that wins.
    pub fn display_name<'a>(&'a self, process_name: &'a str) -> &'a str {
        let assembly = self.assembly_name.as_deref().filter(|a| !a.is_empty());
        match assembly {
            Some(assembly) if process_name == "dotnet" => assembly,
            _ => process_name,
        }
    }
}

/// Query process identity, trying ProcessInfo3 then 2 then 1.
///
/// Older runtimes answer `UNKNOWN_COMMAND` for versions they predate, so we walk down. Each
/// attempt needs its own connection: the runtime closes the socket after an error reply.
pub fn process_info(socket: &Path) -> Result<ProcessInfo> {
    let mut last_err = None;

    for command_id in [process::PROCESS_INFO3, process::PROCESS_INFO2, process::PROCESS_INFO] {
        match request_process_info(socket, command_id) {
            Ok(info) => return Ok(info),
            Err(CommandError::Ipc(IpcError::Server(UNKNOWN_COMMAND))) => {
                last_err = Some(CommandError::Ipc(IpcError::Server(UNKNOWN_COMMAND)));
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap_or(CommandError::Ipc(IpcError::Server(UNKNOWN_COMMAND))))
}

fn request_process_info(socket: &Path, command_id: u8) -> Result<ProcessInfo> {
    let mut stream = connect(socket)?;
    frame::write_message(&mut stream, command_set::PROCESS, command_id, &[])?;
    let payload = frame::read_response(&mut stream)?;

    let mut r = PayloadReader::new(&payload);
    let mut info = ProcessInfo::default();

    // v3 leads with a version field; v2 and v1 start straight at the pid.
    if command_id == process::PROCESS_INFO3 {
        r.u32()?;
    }

    info.pid = r.u64()?;
    r.guid()?; // runtime cookie, unused
    info.command_line = r.string()?;
    info.os = r.string()?;
    info.arch = r.string()?;

    if command_id == process::PROCESS_INFO2 || command_id == process::PROCESS_INFO3 {
        info.assembly_name = Some(r.string()?);
        info.clr_version = Some(r.string()?);
    }
    if command_id == process::PROCESS_INFO3 {
        info.runtime_identifier = Some(r.string()?);
    }

    Ok(info)
}

/// One EventSource/EventCounter provider to subscribe to.
#[derive(Debug, Clone)]
pub struct Provider {
    pub name: String,
    pub keywords: u64,
    pub level: u32,
    /// Semicolon-separated `key=value` pairs, e.g. `EventCounterIntervalSec=1`.
    pub filter_data: String,
}

#[derive(Debug, Clone)]
pub struct TraceConfig {
    pub circular_buffer_mb: u32,
    pub providers: Vec<Provider>,
}

/// NetTrace. The alternative (NetPerf V3 = 0) is still accepted by the runtime but is not
/// exercised by any shipping tool.
const FORMAT_NETTRACE: u32 = 1;

impl TraceConfig {
    fn to_payload(&self) -> Vec<u8> {
        let mut w = PayloadWriter::new();
        w.u32(self.circular_buffer_mb)
            .u32(FORMAT_NETTRACE)
            // Rundown emits method/assembly/type maps: pure overhead for counters, and
            // dotnet-counters itself asks for none.
            .bool(false)
            .u32(self.providers.len() as u32);

        for provider in &self.providers {
            w.u64(provider.keywords)
                .u32(provider.level)
                .string(&provider.name)
                .string(&provider.filter_data);
        }
        w.into_bytes()
    }
}

/// A live EventPipe session. The stream carries raw nettrace bytes from the moment this returns.
#[derive(Debug)]
pub struct TraceSession {
    pub session_id: u64,
    pub stream: UnixStream,
}

/// Start a counter session with CollectTracing2, which every runtime from .NET 5 onwards accepts.
///
/// The returned stream is the *same* socket the request went out on: everything after the
/// response header is nettrace.
pub fn start_tracing(socket: &Path, config: &TraceConfig) -> Result<TraceSession> {
    let mut stream = connect(socket)?;
    frame::write_message(
        &mut stream,
        command_set::EVENTPIPE,
        eventpipe::COLLECT_TRACING2,
        &config.to_payload(),
    )?;

    let payload = frame::read_response(&mut stream)?;
    let session_id = PayloadReader::new(&payload).u64()?;
    Ok(TraceSession { session_id, stream })
}

/// Stop a session. This must go out on a *new* connection — the protocol allows one command per
/// connection.
///
/// The runtime then finishes writing into the *original* stream before closing it, so the caller
/// must keep draining that stream to EOF. Crucially, that draining has to continue **while** this
/// call is in flight: if the streaming socket's buffer fills, the runtime blocks writing to it and
/// never gets to process the stop, so a caller that pauses its reader to call this deadlocks.
/// Issue it from another thread, or after arranging for the stream to keep being read.
pub fn stop_tracing(socket: &Path, session_id: u64) -> Result<()> {
    let mut stream = connect(socket)?;
    let mut w = PayloadWriter::new();
    w.u64(session_id);
    frame::write_message(
        &mut stream,
        command_set::EVENTPIPE,
        eventpipe::STOP_TRACING,
        &w.into_bytes(),
    )?;
    frame::read_response(&mut stream)?;
    // Dropping the stream closes this connection. Do not read from it further: a blocking read
    // here is one of the ways to produce the deadlock described above.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clr_version_strips_prerelease_and_metadata() {
        let cases = [
            ("9.0.7", Some((9, 0))),
            ("6.0.0-preview.6.12345", Some((6, 0))),
            ("10.0.1+abc123", Some((10, 0))),
            ("8.0.19-servicing+deadbeef", Some((8, 0))),
            ("not-a-version", None),
        ];
        for (raw, expected) in cases {
            let info = ProcessInfo {
                clr_version: Some(raw.to_owned()),
                ..Default::default()
            };
            assert_eq!(info.clr_major_minor(), expected, "{raw}");
        }
    }

    #[test]
    fn framework_label_reads_naturally() {
        let info = ProcessInfo {
            clr_version: Some("9.0.7".into()),
            ..Default::default()
        };
        assert_eq!(info.framework_label().as_deref(), Some("net9.0"));
    }

    #[test]
    fn missing_clr_version_yields_no_label() {
        assert_eq!(ProcessInfo::default().framework_label(), None);
    }

    #[test]
    fn display_name_prefers_the_assembly_only_for_host_launched_apps() {
        let info = ProcessInfo {
            assembly_name: Some("CrimeRate.Front".into()),
            ..Default::default()
        };
        // Framework-dependent: the OS only knows it as "dotnet".
        assert_eq!(info.display_name("dotnet"), "CrimeRate.Front");
        // Self-contained: the executable name is already meaningful and more specific.
        assert_eq!(info.display_name("Rider.Backend"), "Rider.Backend");
    }

    #[test]
    fn display_name_falls_back_when_no_assembly_is_reported() {
        // ProcessInfo v1 carries no assembly name at all.
        assert_eq!(ProcessInfo::default().display_name("dotnet"), "dotnet");

        let empty = ProcessInfo { assembly_name: Some(String::new()), ..Default::default() };
        assert_eq!(empty.display_name("dotnet"), "dotnet");
    }

    #[test]
    fn trace_payload_matches_the_worked_example() {
        let config = TraceConfig {
            circular_buffer_mb: 256,
            providers: vec![Provider {
                name: "System.Runtime".into(),
                keywords: 0,
                level: 4,
                filter_data: "EventCounterIntervalSec=1".into(),
            }],
        };
        let payload = config.to_payload();
        assert_eq!(payload.len(), 115);

        let message = frame::encode_message(
            command_set::EVENTPIPE,
            eventpipe::COLLECT_TRACING2,
            &payload,
        )
        .unwrap();
        assert_eq!(message.len(), 135);
        assert_eq!(&message[14..16], &[0x87, 0x00]);
        // format = NetTrace, requestRundown = false
        assert_eq!(&message[24..28], &[1, 0, 0, 0]);
        assert_eq!(message[28], 0);
    }
}
