use std::ops::ControlFlow;
use std::time::Instant;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{bail, Result};

use countercow::counters;
use countercow::ipc;
use countercow::ipc::discovery::{self, DotnetProcess};
use countercow::ui;

#[derive(Parser)]
#[command(name = "countercow", version, about = "A btop-style TUI for .NET runtime counters")]
struct Cli {
    /// Attach directly to this process id, skipping the picker.
    #[arg(long, global = true)]
    pid: Option<u32>,

    /// Attach to the single process whose name contains this text.
    #[arg(long, global = true)]
    name: Option<String>,

    /// Counter refresh interval in seconds. Adjustable in the dashboard with - and +.
    #[arg(
        long,
        global = true,
        default_value_t = counters::session::DEFAULT_INTERVAL_SECS,
        value_parser = parse_interval
    )]
    interval: f64,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List attachable .NET processes and exit.
    Ps,
    /// Print raw counter samples as they arrive. Diagnostic aid for the parser.
    Dump {
        /// Stop after this many seconds.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
}

/// Smallest and largest refresh intervals worth asking the runtime for.
///
/// Zero is the one that matters: it is what a typo produces, and the runtime takes it literally —
/// the session opens, reports nothing, and the dashboard sits there looking attached to a dead
/// process. The upper bound is judgement rather than a limit.
const MIN_INTERVAL_SECS: f64 = 0.05;
const MAX_INTERVAL_SECS: f64 = 60.0;

fn parse_interval(text: &str) -> std::result::Result<f64, String> {
    let seconds: f64 = text.parse().map_err(|_| format!("{text:?} is not a number"))?;
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&seconds) {
        return Err(format!(
            "{seconds} seconds is outside the {MIN_INTERVAL_SECS}–{MAX_INTERVAL_SECS} range \
             countercow can sample at"
        ));
    }
    Ok(seconds)
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Ps) => list_processes(),
        Some(Command::Dump { seconds }) => {
            let Some(process) = resolve_target(cli.pid, cli.name.as_deref())? else {
                bail!("dump needs a target: pass --pid or --name");
            };
            dump_counters(&process, seconds, cli.interval)
        }
        None => {
            // --pid/--name supply the first target; without them, and after any detach, the
            // picker chooses.
            let target = resolve_target(cli.pid, cli.name.as_deref())?;
            ui::run(target, cli.interval)
        }
    }
}

/// Stream counters to stdout for a fixed window, grouped by provider.
fn dump_counters(process: &DotnetProcess, seconds: u64, interval: f64) -> Result<()> {
    let info = ipc::commands::process_info(&process.socket)?;
    println!(
        "{} (pid {}, {})  —  {interval}s interval, {seconds}s window",
        info.display_name(&process.name),
        process.pid,
        info.framework_label().unwrap_or_else(|| "unknown runtime".into())
    );

    let deadline = Instant::now() + std::time::Duration::from_secs(seconds);
    let mut count = 0usize;
    let mut providers: std::collections::BTreeSet<String> = Default::default();

    let session = counters::session::start(&process.socket, interval)?;
    let session_id = session.session_id;

    counters::session::run(session.stream, interval, |sample| {
        if Instant::now() >= deadline {
            return ControlFlow::Break(());
        }
        count += 1;
        providers.insert(sample.provider.clone());

        let units = sample.units();
        let suffix = match sample.rate_seconds() {
            Some(secs) if secs > 1.0 => format!(" {units} / {:.0} min", secs / 60.0),
            Some(_) => format!(" {units} / sec"),
            None if units.is_empty() => String::new(),
            None => format!(" {units}"),
        };
        println!(
            "  {:<38} {:>16.4}{}",
            format!("{}/{}", sample.provider, sample.name),
            sample.value,
            suffix
        );
        ControlFlow::Continue(())
    })?;
    counters::session::stop(&process.socket, session_id)?;

    println!("\n{count} samples from {} providers:", providers.len());
    for provider in &providers {
        println!("  {provider}");
    }
    Ok(())
}

/// Resolve `--pid` / `--name` to exactly one process, or `None` if neither was given.
fn resolve_target(pid: Option<u32>, name: Option<&str>) -> Result<Option<DotnetProcess>> {
    if pid.is_none() && name.is_none() {
        return Ok(None);
    }

    let found = discovery::discover()?;

    if let Some(pid) = pid {
        return match found.processes.into_iter().find(|p| p.pid == pid) {
            Some(process) => Ok(Some(process)),
            None => bail!(
                "no attachable .NET process with pid {pid}. It may not be a .NET process, may \
                 have exited, or may belong to another user."
            ),
        };
    }

    let needle = name.unwrap().to_lowercase();
    let mut matches: Vec<DotnetProcess> = found
        .processes
        .iter()
        .filter(|p| p.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();

    // Framework-dependent apps are launched through the host, so their process name is just
    // "dotnet" and matching on it is useless. Ask the runtimes for their entry assembly instead.
    // Only worth the round trips once the cheap match has come up empty.
    if matches.is_empty() {
        matches = found
            .processes
            .iter()
            .filter(|p| {
                ipc::commands::process_info(&p.socket)
                    .ok()
                    .and_then(|info| info.assembly_name)
                    .is_some_and(|assembly| assembly.to_lowercase().contains(&needle))
            })
            .cloned()
            .collect();
    }

    match matches.len() {
        0 => bail!(
            "no attachable .NET process whose name or entry assembly contains {needle:?}. \
             Run `countercow ps` to see what is available."
        ),
        1 => Ok(Some(matches.remove(0))),
        _ => {
            // Identical process names are the common case here, so lead with something that
            // actually distinguishes them.
            let names: Vec<String> = matches
                .iter()
                .map(|p| {
                    let assembly = ipc::commands::process_info(&p.socket)
                        .ok()
                        .and_then(|info| info.assembly_name)
                        .filter(|a| !a.is_empty() && *a != p.name);
                    match assembly {
                        Some(assembly) => format!("{assembly} (pid {})", p.pid),
                        None => format!("{} (pid {})", p.name, p.pid),
                    }
                })
                .collect();
            bail!(
                "{:?} matches {} processes: {}. Use --pid to pick one.",
                needle,
                names.len(),
                names.join(", ")
            )
        }
    }
}

fn list_processes() -> Result<()> {
    let found = discovery::discover()?;

    if found.processes.is_empty() {
        println!("No attachable .NET processes found in {}.", discovery::ipc_root().display());
    } else {
        println!("{:>7}  {:<24}  COMMAND", "PID", "NAME");
        for process in &found.processes {
            println!(
                "{:>7}  {:<24}  {}",
                process.pid,
                truncate(&process.name, 24),
                process.command
            );
        }
    }

    // Say what was skipped rather than silently showing a short list.
    let mut skipped = Vec::new();
    if found.stale > 0 {
        skipped.push(format!("{} stale", found.stale));
    }
    if found.mismatched > 0 {
        skipped.push(format!("{} reusing a dead process's pid", found.mismatched));
    }
    if found.foreign > 0 {
        skipped.push(format!("{} owned by another user", found.foreign));
    }
    if found.too_long > 0 {
        skipped.push(format!("{} with an over-long socket path", found.too_long));
    }
    let total_skipped = found.stale + found.mismatched + found.foreign + found.too_long;
    if total_skipped > 0 {
        println!("\nSkipped {total_skipped} sockets: {}.", skipped.join(", "));
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}
