//! Capture a raw nettrace stream to a file, for use as a parser test fixture.
//!
//! Development tooling, not a shipped feature — countercow itself is live-view only. Fixtures
//! matter because the nettrace format fails *silently*: a misread field yields plausible numbers
//! rather than an error, so the parser needs real bytes from real runtimes to test against.
//!
//! Capture from as many .NET versions as you can; metadata tag usage differs between them.
//!
//! ```text
//! cargo run --example capture -- <pid> <output-path> [seconds] [counters|runtime]
//! ```
//!
//! `counters` (the default) captures the EventCounter session the dashboard uses; `runtime`
//! captures the manifest-based GC/exception/contention session the investigation screen uses.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use countercow::counters::session::trace_config;
use countercow::ipc::{commands, discovery};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: capture <pid> <output> [seconds]")?.parse()?;
    let output = args.next().ok_or("usage: capture <pid> <output> [seconds]")?;
    let seconds: u64 = args.next().unwrap_or_else(|| "3".into()).parse()?;
    let kind = args.next().unwrap_or_else(|| "counters".into());

    let found = discovery::discover()?;
    let process = found
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    let info = commands::process_info(&process.socket)?;
    println!(
        "capturing {}s of {kind} from {} (pid {}, {})",
        seconds,
        process.name,
        pid,
        info.framework_label().unwrap_or_else(|| "unknown".into())
    );

    let config = match kind.as_str() {
        "runtime" => countercow::runtime::session::trace_config(),
        "profile" => countercow::profile::session::trace_config(),
        _ => trace_config(1.0),
    };
    let session = commands::start_tracing(&process.socket, &config)?;
    let mut stream = session.stream;
    // Short timeout so the deadline is checked even while the runtime is idle between intervals.
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;

    // Stop from another thread, so this one never pauses its reading. If the streaming socket's
    // buffer fills while we are blocked issuing the stop, the runtime blocks writing to it and
    // never processes the stop — a deadlock that only shows up on high-volume sessions.
    let stop_socket = process.socket.clone();
    let session_id = session.session_id;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        let _ = commands::stop_tracing(&stop_socket, session_id);
    });

    let mut captured = Vec::new();
    // Well beyond the flush of a stopped session, but bounded so a wedged runtime cannot hang
    // the capture forever.
    let hard_deadline = Instant::now() + Duration::from_secs(seconds + 30);
    let mut chunk = [0u8; 64 * 1024];

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => captured.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }

        if Instant::now() >= hard_deadline {
            eprintln!("warning: stream did not close; capture may be truncated");
            break;
        }
    }

    std::fs::File::create(&output)?.write_all(&captured)?;
    println!("wrote {} bytes to {output}", captured.len());
    Ok(())
}
