//! Finding attachable .NET processes.
//!
//! The runtime advertises a diagnostic endpoint as a Unix socket named
//! `{tempdir}dotnet-diagnostic-{pid}-{key}-socket`. Two things make naive enumeration wrong:
//!
//! 1. `{tempdir}` is `$TMPDIR`, not `/tmp`. On macOS that is a per-user sandbox directory, so
//!    globbing `/tmp` finds nothing at all even with many .NET processes running.
//! 2. Sockets outlive their process when it dies uncleanly. On a normal developer machine the
//!    large majority of these files are stale, and a recycled PID can collide with one.
//!
//! `{key}` is the process start time, which lets us reject a stale socket whose PID has been
//! reused — a guard the reference client does not implement.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const SOCKET_PREFIX: &str = "dotnet-diagnostic-";
const SOCKET_SUFFIX: &str = "-socket";

/// `sun_path` is 104 bytes on macOS and 108 on Linux; use the smaller bound everywhere so a
/// path that would fail on macOS is reported the same way on Linux.
const SUN_PATH_MAX: usize = 104;

#[derive(Debug, Clone)]
pub struct DotnetProcess {
    pub pid: u32,
    pub socket: PathBuf,
    /// Best display name: executable stem where available, else the OS process name.
    pub name: String,
    /// Full command line, or the executable path when arguments are unavailable.
    pub command: String,
    /// Whether the socket's key matched the process's real start time.
    pub start_key_verified: bool,
}

/// Everything discovery found, including what it deliberately skipped.
#[derive(Debug, Default)]
pub struct Discovery {
    pub processes: Vec<DotnetProcess>,
    /// Sockets whose PID no longer exists.
    pub stale: usize,
    /// Sockets whose PID is alive but whose start key disagrees — almost certainly PID reuse.
    pub mismatched: usize,
    /// Sockets owned by another user, which we cannot connect to.
    pub foreign: usize,
    /// Sockets whose path is too long for `sun_path`.
    pub too_long: usize,
}

#[derive(Debug)]
pub enum DiscoveryError {
    UnreadableRoot { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::UnreadableRoot { path, source } => write!(
                f,
                "cannot read the diagnostic socket directory {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DiscoveryError::UnreadableRoot { source, .. } => Some(source),
        }
    }
}

/// Where the runtime puts diagnostic sockets: `$TMPDIR` if set, else `/tmp`.
///
/// This mirrors .NET's `Path.GetTempPath()`. Getting it wrong is silent — you simply find no
/// processes.
pub fn ipc_root() -> PathBuf {
    match std::env::var_os("TMPDIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("/tmp"),
    }
}

/// Split `dotnet-diagnostic-{pid}-{key}-socket` into its parts.
///
/// Returns `None` for anything else, which includes the `dotnet-diagnostic-dsrouter-*` sockets
/// used for mobile/WASM bridging — those have a non-numeric first field and we do not support them.
fn parse_socket_name(name: &OsStr) -> Option<(u32, u64)> {
    let name = name.to_str()?;
    let middle = name.strip_prefix(SOCKET_PREFIX)?.strip_suffix(SOCKET_SUFFIX)?;
    let (pid, key) = middle.split_once('-')?;
    Some((pid.parse().ok()?, key.parse().ok()?))
}

/// The value the runtime uses as the socket's disambiguation key, or `None` if we cannot
/// determine it on this platform.
///
/// The units differ per OS, which is why this cannot be derived from `sysinfo` alone.
#[cfg(target_os = "linux")]
fn process_start_key(pid: u32, _system: &System) -> Option<u64> {
    // Field 22 of /proc/{pid}/stat: start time in jiffies since boot. `comm` (field 2) can
    // itself contain spaces and parentheses, so seek past the *last* ')' before tokenising —
    // this is what the runtime's own parser does.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    // Tokens now begin at field 3 (state), so field 22 is index 19.
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn process_start_key(pid: u32, system: &System) -> Option<u64> {
    // macOS and the BSDs use the process start time in Unix epoch seconds, which is exactly
    // what sysinfo reports.
    system.process(Pid::from_u32(pid)).map(sysinfo::Process::start_time)
}

fn display_name(process: &sysinfo::Process) -> String {
    process
        .exe()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        // Not `file_stem`: .NET executables have dotted names like "CrimeRate.VectorTileApi",
        // and file_stem would read ".VectorTileApi" as an extension and drop it. Only strip
        // extensions that really are ones.
        .map(|name| {
            name.strip_suffix(".exe")
                .or_else(|| name.strip_suffix(".dll"))
                .unwrap_or(name)
                .to_owned()
        })
        // Linux truncates the OS process name to 15 characters, so only fall back to it.
        .unwrap_or_else(|| process.name().to_string_lossy().into_owned())
}

fn display_command(process: &sysinfo::Process) -> String {
    let args: Vec<String> = process
        .cmd()
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if args.is_empty() {
        process
            .exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    } else {
        args.join(" ")
    }
}

/// Enumerate attachable .NET processes, skipping stale, reused and unreachable endpoints.
pub fn discover() -> Result<Discovery, DiscoveryError> {
    let root = ipc_root();
    let entries = fs::read_dir(&root).map_err(|source| DiscoveryError::UnreadableRoot {
        path: root.clone(),
        source,
    })?;

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always).with_cmd(sysinfo::UpdateKind::Always).with_user(sysinfo::UpdateKind::Always),
    );

    let own_uid = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid))
        .and_then(|p| p.user_id())
        .cloned();

    let mut found = Discovery::default();
    // A process can have several sockets on disk; keep only the best candidate per PID.
    let mut candidates: Vec<(u32, PathBuf, bool)> = Vec::new();

    for entry in entries.flatten() {
        let Some((pid, key)) = parse_socket_name(&entry.file_name()) else {
            continue;
        };

        let path = entry.path();

        let Some(process) = system.process(Pid::from_u32(pid)) else {
            found.stale += 1;
            continue;
        };

        // Sockets are mode 0600, so another user's endpoint is visible but unusable.
        if let (Some(own), Some(theirs)) = (own_uid.as_ref(), process.user_id()) {
            if own != theirs {
                found.foreign += 1;
                continue;
            }
        }

        if path.as_os_str().len() > SUN_PATH_MAX {
            found.too_long += 1;
            continue;
        }

        // The key is the process start time. A mismatch means this socket belongs to a dead
        // process whose PID has since been reused.
        let verified = match process_start_key(pid, &system) {
            Some(actual) => {
                if actual != key {
                    found.mismatched += 1;
                    continue;
                }
                true
            }
            None => false,
        };

        candidates.push((pid, path, verified));
    }

    // Prefer a verified socket; otherwise fall back to the most recently written one, which is
    // what the reference client does unconditionally.
    candidates.sort_by(|a, b| {
        b.2.cmp(&a.2).then_with(|| {
            mtime(&b.1).cmp(&mtime(&a.1))
        })
    });
    candidates.dedup_by_key(|(pid, _, _)| *pid);

    for (pid, socket, start_key_verified) in candidates {
        let Some(process) = system.process(Pid::from_u32(pid)) else {
            continue;
        };
        found.processes.push(DotnetProcess {
            pid,
            socket,
            name: display_name(process),
            command: display_command(process),
            start_key_verified,
        });
    }

    found
        .processes
        .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)));
    Ok(found)
}

fn mtime(path: &Path) -> std::time::SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn parses_a_well_formed_socket_name() {
        let name = OsString::from("dotnet-diagnostic-77686-1786223894-socket");
        assert_eq!(parse_socket_name(&name), Some((77686, 1786223894)));
    }

    #[test]
    fn rejects_dsrouter_and_malformed_names() {
        for name in [
            "dotnet-diagnostic-dsrouter-77686-1786223894-socket",
            "dotnet-diagnostic-77686-socket",
            "dotnet-diagnostic-77686-1786223894",
            "clr-debug-pipe-1234",
            "dotnet-diagnostic-abc-123-socket",
        ] {
            assert_eq!(parse_socket_name(&OsString::from(name)), None, "{name}");
        }
    }

    #[test]
    fn ipc_root_prefers_tmpdir() {
        // Not asserting a specific value: on macOS this is a per-user sandbox path, on Linux
        // usually /tmp. The contract is only that we never return an empty path.
        assert!(!ipc_root().as_os_str().is_empty());
    }
}
