//! Run a CPU profile and print the hot methods, without the TUI.
//!
//! The quickest way to check that sampling, stack resolution and the rundown all line up.
//!
//! ```text
//! cargo run --example profile_cli -- <pid> [seconds]
//! ```

use std::time::{Duration, Instant};

use countercow::ipc::discovery;
use countercow::profile::{run, session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("usage: profile_cli <pid> [seconds]")?.parse()?;
    let seconds: u64 = args.next().unwrap_or_else(|| "5".into()).parse()?;

    let found = discovery::discover()?;
    let process = found
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    println!("profiling {} (pid {pid}) for {seconds}s…", process.name);

    let session = session::start(&process.socket)?;
    let session_id = session.session_id;

    // Stopping is what makes the rundown arrive, and it must happen on another thread so this one
    // keeps draining the stream.
    let stop_socket = process.socket.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        let _ = session::stop(&stop_socket, session_id);
    });

    let started = Instant::now();
    let result = run::collect(session.stream, |_| {})?;
    let elapsed = started.elapsed().as_secs_f64();

    let state = &result.state;
    println!(
        "\n{} samples in {elapsed:.1}s ({:.0}/s), {} methods known",
        state.samples,
        state.samples as f64 / elapsed,
        result.methods.len()
    );
    // `--all` keeps parked threads in the ranking.
    let include_waiting = std::env::args().any(|a| a == "--all");
    let hot = state.hot_methods(&result.methods, 25, include_waiting);

    println!(
        "  {} working samples, {:.0}% of threads parked in a wait",
        hot.working_samples,
        hot.waiting_percent()
    );
    if state.unresolved_stacks > 0 || state.empty_stacks > 0 {
        println!(
            "  {} unresolved stacks, {} empty",
            state.unresolved_stacks, state.empty_stacks
        );
    }

    // `--stacks` prints a few raw stacks, which is how frame order gets confirmed.
    if std::env::args().any(|a| a == "--stacks") {
        for (index, frames) in result.sample_stacks.iter().enumerate().take(3) {
            println!("\nstack {index} ({} frames, as stored):", frames.len());
            for (depth, address) in frames.iter().enumerate() {
                let name = result
                    .methods
                    .resolve(*address)
                    .map(|m| m.qualified_name())
                    .unwrap_or_else(|| format!("(native 0x{address:x})"));
                println!("  [{depth}] {name}");
            }
        }
    }

    println!("\n{:>7}  {:>7}  METHOD", "SELF", "TOTAL");
    for method in hot.rows {
        println!(
            "{:>6.1}%  {:>6.1}%  {}",
            method.self_percent, method.total_percent, method.name
        );
    }

    Ok(())
}
