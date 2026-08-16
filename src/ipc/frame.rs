//! Diagnostics IPC message framing: a fixed 20-byte header followed by a command payload.
//!
//! All reads are exact-length. This matters more than it looks: after a `CollectTracing`
//! response the *same* socket carries the raw nettrace stream, so a reader that over-reads
//! swallows the `Nettrace` magic and corrupts the parse.

use std::fmt;
use std::io::{self, Read, Write};

/// ASCII "DOTNET_IPC_V1" plus an explicit NUL: 13 characters + terminator = 14 bytes.
pub const MAGIC: &[u8; 14] = b"DOTNET_IPC_V1\0";
pub const HEADER_SIZE: u16 = 20;

pub mod command_set {
    pub const EVENTPIPE: u8 = 0x02;
    pub const PROCESS: u8 = 0x04;
    pub const SERVER: u8 = 0xFF;
}

pub mod eventpipe {
    pub const STOP_TRACING: u8 = 0x01;
    /// Command ids run one ahead of the version number: CollectTracing2 is 0x03.
    pub const COLLECT_TRACING2: u8 = 0x03;
}

pub mod process {
    pub const PROCESS_INFO: u8 = 0x00;
    pub const PROCESS_INFO2: u8 = 0x04;
    pub const PROCESS_INFO3: u8 = 0x08;
}

pub mod server {
    pub const OK: u8 = 0x00;
    pub const ERROR: u8 = 0xFF;
}

#[derive(Debug)]
pub enum IpcError {
    Io(io::Error),
    BadMagic(Vec<u8>),
    /// `size` field claimed fewer bytes than the header itself occupies.
    ShortMessage(u16),
    /// A single IPC message is capped at 65535 bytes by the u16 size field.
    PayloadTooLarge(usize),
    /// The server replied with an error and closed the connection.
    Server(u32),
    /// An unexpected command set/id combination came back.
    UnexpectedResponse { command_set: u8, command_id: u8 },
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::Io(e) => write!(f, "diagnostic socket I/O failed: {e}"),
            IpcError::BadMagic(got) => {
                write!(f, "not a diagnostics endpoint (bad magic: {got:02x?})")
            }
            IpcError::ShortMessage(size) => {
                write!(f, "malformed response: size field is {size}, below the 20-byte header")
            }
            IpcError::PayloadTooLarge(len) => write!(
                f,
                "payload of {len} bytes exceeds the 65515-byte limit imposed by the u16 size field"
            ),
            IpcError::Server(hresult) => match hresult_name(*hresult) {
                Some(name) => write!(f, "runtime rejected the command: {name} (0x{hresult:08X})"),
                None => write!(f, "runtime rejected the command: 0x{hresult:08X}"),
            },
            IpcError::UnexpectedResponse { command_set, command_id } => write!(
                f,
                "unexpected response: command_set 0x{command_set:02X}, command_id 0x{command_id:02X}"
            ),
        }
    }
}

impl std::error::Error for IpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IpcError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(e: io::Error) -> Self {
        IpcError::Io(e)
    }
}

/// The HRESULTs worth naming. `UNKNOWN_COMMAND` means the runtime predates the command version
/// (fall back one); `BAD_ENCODING` means our payload is malformed.
pub fn hresult_name(hresult: u32) -> Option<&'static str> {
    Some(match hresult {
        0x8000_4005 => "E_FAIL",
        0x8007_0057 => "E_INVALIDARG",
        0x8007_007A => "E_INSUFFICIENT_BUFFER",
        0x8000_00CB => "ENVVAR_NOT_FOUND",
        0x8013_1371 => "RUNTIME_UNINITIALIZED",
        0x8013_1384 => "BAD_ENCODING",
        0x8013_1385 => "UNKNOWN_COMMAND",
        0x8013_1386 => "UNKNOWN_MAGIC",
        0x8013_1387 => "UNKNOWN_ERROR",
        0x8013_135B => "NOT_YET_AVAILABLE",
        0x8013_1515 => "E_NOTSUPPORTED",
        0x8013_136A => "PROFILER_ALREADY_ACTIVE",
        _ => return None,
    })
}

pub const UNKNOWN_COMMAND: u32 = 0x8013_1385;

/// Serialise a complete message. `size` covers the header *and* the payload.
pub fn encode_message(command_set: u8, command_id: u8, payload: &[u8]) -> Result<Vec<u8>, IpcError> {
    let total = HEADER_SIZE as usize + payload.len();
    let size = u16::try_from(total).map_err(|_| IpcError::PayloadTooLarge(payload.len()))?;

    let mut msg = Vec::with_capacity(total);
    msg.extend_from_slice(MAGIC);
    msg.extend_from_slice(&size.to_le_bytes());
    msg.push(command_set);
    msg.push(command_id);
    msg.extend_from_slice(&[0, 0]); // reserved
    msg.extend_from_slice(payload);
    Ok(msg)
}

pub fn write_message<W: Write>(
    w: &mut W,
    command_set: u8,
    command_id: u8,
    payload: &[u8],
) -> Result<(), IpcError> {
    let msg = encode_message(command_set, command_id, payload)?;
    w.write_all(&msg)?;
    w.flush()?;
    Ok(())
}

/// Read one message, returning `(command_set, command_id, payload)`.
///
/// Reads exactly `HEADER_SIZE` bytes, then exactly `size - HEADER_SIZE` more — never a byte beyond,
/// so a continuation stream on the same socket stays intact.
pub fn read_message<R: Read>(r: &mut R) -> Result<(u8, u8, Vec<u8>), IpcError> {
    let mut header = [0u8; HEADER_SIZE as usize];
    r.read_exact(&mut header)?;

    if &header[0..14] != MAGIC.as_slice() {
        return Err(IpcError::BadMagic(header[0..14].to_vec()));
    }

    let size = u16::from_le_bytes([header[14], header[15]]);
    if size < HEADER_SIZE {
        return Err(IpcError::ShortMessage(size));
    }
    let command_set = header[16];
    let command_id = header[17];

    let mut payload = vec![0u8; (size - HEADER_SIZE) as usize];
    r.read_exact(&mut payload)?;
    Ok((command_set, command_id, payload))
}

/// Read a message and require it to be a success reply, returning the payload.
///
/// On an error reply the payload starts with a 4-byte HRESULT, and the server has already closed
/// the connection.
pub fn read_response<R: Read>(r: &mut R) -> Result<Vec<u8>, IpcError> {
    let (command_set, command_id, payload) = read_message(r)?;

    if command_set == command_set::SERVER && command_id == server::ERROR {
        let hresult = payload
            .get(0..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0);
        return Err(IpcError::Server(hresult));
    }
    if command_id != server::OK {
        return Err(IpcError::UnexpectedResponse { command_set, command_id });
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_twenty_bytes() {
        let msg = encode_message(command_set::EVENTPIPE, eventpipe::COLLECT_TRACING2, &[]).unwrap();
        assert_eq!(msg.len(), 20);
        assert_eq!(&msg[0..14], b"DOTNET_IPC_V1\0");
        assert_eq!(&msg[14..16], &[20, 0]);
        assert_eq!(msg[16], 0x02);
        assert_eq!(msg[17], 0x03);
        assert_eq!(&msg[18..20], &[0, 0]);
    }

    #[test]
    fn size_covers_header_plus_payload() {
        // The worked CollectTracing2 example is 115 payload bytes => size 135 = 0x0087.
        let msg = encode_message(0x02, 0x03, &vec![0u8; 115]).unwrap();
        assert_eq!(&msg[14..16], &[0x87, 0x00]);
    }

    #[test]
    fn oversized_payload_is_rejected_not_truncated() {
        let err = encode_message(0x02, 0x03, &vec![0u8; 70_000]).unwrap_err();
        assert!(matches!(err, IpcError::PayloadTooLarge(70_000)));
    }

    #[test]
    fn reads_stop_at_the_message_boundary() {
        // A success reply carrying a u64 session id, with trailing stream bytes after it.
        let mut wire = encode_message(command_set::SERVER, server::OK, &7u64.to_le_bytes()).unwrap();
        wire.extend_from_slice(b"Nettrace");

        let mut cursor = std::io::Cursor::new(wire);
        let payload = read_response(&mut cursor).unwrap();
        assert_eq!(payload, 7u64.to_le_bytes());

        // The continuation must be untouched.
        let mut rest = Vec::new();
        cursor.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"Nettrace");
    }

    #[test]
    fn error_reply_surfaces_the_hresult() {
        let wire = encode_message(
            command_set::SERVER,
            server::ERROR,
            &UNKNOWN_COMMAND.to_le_bytes(),
        )
        .unwrap();
        let err = read_response(&mut std::io::Cursor::new(wire)).unwrap_err();
        match err {
            IpcError::Server(hr) => assert_eq!(hr, UNKNOWN_COMMAND),
            other => panic!("expected a server error, got {other:?}"),
        }
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut wire = vec![0u8; 20];
        wire[14] = 20;
        let err = read_message(&mut std::io::Cursor::new(wire)).unwrap_err();
        assert!(matches!(err, IpcError::BadMagic(_)));
    }
}
