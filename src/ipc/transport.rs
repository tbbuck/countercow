//! Connecting to a diagnostic endpoint.
//!
//! Each connection carries exactly one command. The runtime closes it afterwards — except for
//! `CollectTracing`, where the socket continues as the nettrace stream.

use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ConnectError {
    pub socket: PathBuf,
    pub source: io::Error,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.socket.display();
        match self.source.kind() {
            // The endpoint file outlived the process that created it.
            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound => {
                write!(f, "the process is no longer listening (stale socket {path})")
            }
            io::ErrorKind::PermissionDenied => write!(
                f,
                "permission denied connecting to {path}; the process belongs to another user"
            ),
            _ => write!(f, "could not connect to {path}: {}", self.source),
        }
    }
}

impl std::error::Error for ConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub fn connect(socket: &Path) -> Result<UnixStream, ConnectError> {
    UnixStream::connect(socket).map_err(|source| ConnectError {
        socket: socket.to_path_buf(),
        source,
    })
}
