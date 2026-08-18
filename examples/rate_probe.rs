//! Check what a counter session actually delivers at a given refresh rate.
//!
//! This is what `-` and `+` do in the dashboard, and the part of it no test can reach: the layout
//! tests never speak to a runtime, and driving the TUI needs a terminal. Here there is no terminal
//! at all — just a session per rate, back to back, against a live process.
//!
//! The runtime stamps every counter payload with the interval it measured, so the probe reads the
//! stream directly rather than through [`session::run`], whose interval filter is one of the
//! things worth measuring.
//!
//! ```text
//! cargo run --example rate_probe -- <pid> [window-secs] [interval...]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use countercow::counters::{sample, session};
use countercow::ipc::discovery;
use countercow::nettrace::blocks::NettraceParser;

/// Rates to sweep when none are given.
const DEFAULT_SWEEP: [f64; 6] = [0.25, 0.5, 1.0, 2.0, 3.0, 5.0];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 =
        args.next().ok_or("usage: rate_probe <pid> [window-secs] [interval...]")?.parse()?;
    let window = Duration::from_secs_f64(args.next().unwrap_or_else(|| "6".into()).parse()?);

    let mut intervals: Vec<f64> = Vec::new();
    for arg in args {
        intervals.push(arg.parse()?);
    }
    if intervals.is_empty() {
        intervals.extend(DEFAULT_SWEEP);
    }

    let found = discovery::discover()?;
    let process = found
        .processes
        .into_iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    let mut failures = Vec::new();
    for &interval in &intervals {
        let run = sample_at(&process.socket, interval, window)?;
        run.report(interval);

        if run.kept == 0 {
            failures.push(format!("{interval}s delivered nothing the dashboard would accept"));
        } else if run.dropped > 0 {
            failures.push(format!(
                "{interval}s had {} of {} payloads filtered out",
                run.dropped,
                run.kept + run.dropped
            ));
        }
    }

    if !failures.is_empty() {
        return Err(failures.join("; ").into());
    }
    println!("\nOK: every rate opened, delivered, and closed.");
    Ok(())
}

#[derive(Default)]
struct Run {
    /// Payloads the dashboard's interval filter would accept.
    kept: usize,
    /// Payloads it would throw away.
    dropped: usize,
    /// Distinct counters seen, however they were stamped.
    counters: BTreeSet<String>,
    /// How many payloads carried each stamped interval, to the nearest 10 ms.
    stamps: BTreeMap<u64, usize>,
    /// Per provider: how many payloads were kept, dropped, and the worst interval stamped.
    /// Providers do not share a counter timer, so one can drift while the others do not.
    providers: BTreeMap<String, (usize, usize, f64)>,
    /// Events whose metadata never arrived, so nothing could be decoded from them. The session
    /// skips these silently, which is exactly how a provider can go missing without a word.
    unknown_metadata: BTreeMap<u32, usize>,
    /// Events that decoded but carried no counter payload.
    not_a_counter: usize,
}

impl Run {
    fn report(&self, asked: f64) {
        let spread: Vec<String> = self
            .stamps
            .iter()
            .map(|(ms, count)| format!("{:.2}s x{count}", *ms as f64 / 1000.0))
            .collect();
        println!(
            "asked {asked:>5}s | kept {:>5} | dropped {:>5} | {:>3} counters | stamped {}",
            self.kept,
            self.dropped,
            self.counters.len(),
            spread.join(", ")
        );
        for (provider, (kept, dropped, worst)) in &self.providers {
            println!("    {provider:<38} kept {kept:>5}  dropped {dropped:>5}  worst {worst:.2}s");
        }
        if !self.unknown_metadata.is_empty() {
            let total: usize = self.unknown_metadata.values().sum();
            println!(
                "    {:<38} {total} events across {} metadata ids: {:?}",
                "NO METADATA (silently skipped)",
                self.unknown_metadata.len(),
                self.unknown_metadata.keys().collect::<Vec<_>>()
            );
        }
        if self.not_a_counter > 0 {
            println!("    {:<38} {}", "decoded but not a counter payload", self.not_a_counter);
        }
    }
}

/// Open a counter session, read for `window`, then close it.
fn sample_at(
    socket: &Path,
    interval: f64,
    window: Duration,
) -> Result<Run, Box<dyn std::error::Error>> {
    let session = session::start(socket, interval)?;
    let session_id = session.session_id;
    let deadline = Instant::now() + window;

    let mut run = Run::default();
    let mut parser = NettraceParser::new(session.stream)?;

    'reading: while let Some(batch) = parser.next_events()? {
        for event in batch {
            if Instant::now() >= deadline {
                break 'reading;
            }
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                *run.unknown_metadata.entry(event.metadata_id).or_default() += 1;
                continue;
            };
            let Some(sample) = sample::extract(metadata, &event)? else {
                run.not_a_counter += 1;
                continue;
            };

            run.counters.insert(format!("{}/{}", sample.provider, sample.name));
            *run.stamps.entry((sample.interval_sec * 100.0).round() as u64 * 10).or_default() += 1;

            let tally = run.providers.entry(sample.provider.clone()).or_default();
            tally.2 = tally.2.max(sample.interval_sec);

            // The rule the dashboard applies, evaluated here so its cost shows up in the output
            // rather than silently shrinking the sample count.
            if sample.interval_sec > 0.0 && (sample.interval_sec - interval).abs() > interval * 0.5
            {
                run.dropped += 1;
                tally.1 += 1;
            } else {
                run.kept += 1;
                tally.0 += 1;
            }
        }
    }
    session::stop(socket, session_id)?;

    Ok(run)
}
