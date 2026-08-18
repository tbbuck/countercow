//! Check that a counter session can be closed and reopened at a different rate.
//!
//! This is what `-` and `+` do in the dashboard, and the part of it no test can reach: the layout
//! tests never speak to a runtime, and driving the TUI needs a terminal. Here there is no terminal
//! at all — just the two sessions, back to back, against a live process.
//!
//! The runtime stamps every counter payload with the interval it is using, so the proof is direct
//! rather than inferred from arrival times.
//!
//! ```text
//! cargo run --example rate_probe -- <pid> [first-secs] [second-secs]
//! ```

use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};

use countercow::counters::session;
use countercow::ipc::discovery;

/// The two rates to sample at: the dashboard default, and the fastest rung `-` reaches.
const FIRST: f64 = 1.0;
const SECOND: f64 = 0.25;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: rate_probe <pid> [secs] [secs]")?.parse()?;
    let window = Duration::from_secs_f64(args.next().unwrap_or_else(|| "4".into()).parse()?);

    let found = discovery::discover()?;
    let process = found
        .processes
        .into_iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    let first = sample_at(&process.socket, FIRST, window)?;
    let second = sample_at(&process.socket, SECOND, window)?;

    report("first session", FIRST, &first, window);
    report("second session", SECOND, &second, window);

    if first.samples == 0 || second.samples == 0 {
        return Err("a session yielded nothing; the restart cannot be judged".into());
    }
    // The runtime's own stamp is the assertion: it says what rate it accepted, so a session that
    // silently kept the old cadence fails here rather than looking like a success.
    if (second.reported_interval - SECOND).abs() > SECOND * 0.5 {
        return Err(format!(
            "second session reported a {:.3}s interval, not {SECOND}s — the rate did not change",
            second.reported_interval
        )
        .into());
    }

    println!("\nOK: the session closed and reopened at the new rate.");
    Ok(())
}

struct Run {
    samples: usize,
    /// The interval the runtime says it is using, averaged over the payloads it sent.
    reported_interval: f64,
}

/// Open a counter session, read for `window`, then close it.
fn sample_at(socket: &Path, interval: f64, window: Duration) -> Result<Run, Box<dyn std::error::Error>> {
    let session = session::start(socket, interval)?;
    let session_id = session.session_id;
    let deadline = Instant::now() + window;

    let mut samples = 0usize;
    let mut total_interval = 0.0;
    session::run(session.stream, interval, |sample| {
        if Instant::now() >= deadline {
            return ControlFlow::Break(());
        }
        samples += 1;
        total_interval += sample.interval_sec;
        ControlFlow::Continue(())
    })?;
    session::stop(socket, session_id)?;

    let reported_interval = if samples > 0 { total_interval / samples as f64 } else { 0.0 };
    Ok(Run { samples, reported_interval })
}

fn report(label: &str, asked: f64, run: &Run, window: Duration) {
    println!(
        "{label:>15}: asked {asked}s, runtime reported {:.3}s, {} samples in {:.0}s",
        run.reported_interval,
        run.samples,
        window.as_secs_f64()
    );
}
