mod ipc;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{bail, Result};

use ipc::discovery::{self, DotnetProcess};

#[derive(Parser)]
#[command(name = "countercow", version, about = "A btop-style TUI for .NET runtime counters")]
struct Cli {
    /// Attach directly to this process id, skipping the picker.
    #[arg(long, global = true)]
    pid: Option<u32>,

    /// Attach to the single process whose name contains this text.
    #[arg(long, global = true)]
    name: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List attachable .NET processes and exit.
    Ps,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Ps) => list_processes(),
        None => {
            // The TUI lands in a later phase; for now resolve the target so the selection
            // logic is exercised end to end.
            let target = resolve_target(cli.pid, cli.name.as_deref())?;
            match target {
                Some(process) => {
                    let info = ipc::commands::process_info(&process.socket)?;
                    println!("{} (pid {})", process.name, process.pid);
                    println!("  runtime  {}", info.framework_label().unwrap_or_else(|| "unknown".into()));
                    println!("  arch     {} {}", info.os, info.arch);
                    if let Some(assembly) = &info.assembly_name {
                        if !assembly.is_empty() {
                            println!("  assembly {assembly}");
                        }
                    }
                    println!("\nDashboard not implemented yet.");
                    Ok(())
                }
                None => {
                    list_processes()?;
                    println!("\nPass --pid or --name to attach.");
                    Ok(())
                }
            }
        }
    }
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
        .into_iter()
        .filter(|p| p.name.to_lowercase().contains(&needle))
        .collect();

    match matches.len() {
        0 => bail!("no attachable .NET process whose name contains {needle:?}"),
        1 => Ok(Some(matches.remove(0))),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|p| format!("{} (pid {})", p.name, p.pid))
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
        println!("{:>7}  {:<24}  {}", "PID", "NAME", "COMMAND");
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
