//! Capture a raw nettrace stream to a file, for use as a parser test fixture.
//!
//! Development tooling, not a shipped feature — countercow itself is live-view only. Fixtures
//! matter because the nettrace format fails *silently*: a misread field yields plausible numbers
//! rather than an error, so the parser needs real bytes from real runtimes to test against.
//!
//! Capture from as many .NET versions as you can; metadata tag usage differs between them.
//!
//! ```text
//! cargo run --example capture -- <pid> <output-path> [seconds]
//! ```

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use countercow::counters::session::trace_config;
use countercow::ipc::{commands, discovery};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: capture <pid> <output> [seconds]")?.parse()?;
    let output = args.next().ok_or("usage: capture <pid> <output> [seconds]")?;
    let seconds: u64 = args.next().unwrap_or_else(|| "3".into()).parse()?;

    let found = discovery::discover()?;
    let process = found
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    let info = commands::process_info(&process.socket)?;
    println!(
        "capturing {}s from {} (pid {}, {})",
        seconds,
        process.name,
        pid,
        info.framework_label().unwrap_or_else(|| "unknown".into())
    );

    let session = commands::start_tracing(&process.socket, &trace_config(1.0))?;
    let mut stream = session.stream;
    // Short timeout so the deadline is checked even while the runtime is idle between intervals.
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;

    let mut captured = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut chunk = [0u8; 64 * 1024];
    let mut stopped = false;

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

        if !stopped && Instant::now() >= deadline {
            // Stop goes out on a second connection; the runtime then finishes writing into this
            // one, so we keep reading until it closes.
            commands::stop_tracing(&process.socket, session.session_id)?;
            stopped = true;
        }
    }

    std::fs::File::create(&output)?.write_all(&captured)?;
    println!("wrote {} bytes to {output}", captured.len());
    Ok(())
}
