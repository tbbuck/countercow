# countercow

**A `btop` for .NET.** Live runtime counters, allocation and GC forensics, and a CPU profiler —
one terminal binary, no .NET install required.

[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`dotnet-counters` shows you the same telemetry as a flat, scrolling table of name/value pairs — no
history, no shape, no sense of what matters for the kind of app you're looking at. countercow is
the gap `btop` closed over `top`, and then some:

| | |
|---|---|
| **Watch** | Live counters with gradient-filled history graphs, laid out for the app you attached to. |
| **Investigate** | *What* is filling the heap, *why* each GC ran, what's throwing, what's blocking. |
| **Profile** | A five-second CPU profile ranking methods by self time. |

It is a single self-contained Rust binary that speaks the .NET Diagnostics IPC and EventPipe
protocols directly over a Unix socket — **no .NET SDK, no `dotnet-counters`, no managed dependency
at runtime**. Works against .NET 6 through 10, on Linux and macOS.

```
╭ Heap size ──────────────────────────────────────── 25.3 MiB ╮╭ Memory ────────────────────╮
│27.6 MiB                                   ⣀⣀⣀⣤⣤⣀⣀⣀⣀⣀⣀⣀⣀⣤⣤⣤⣤⣤││Working set        118.5 MiB│
│                  ⢀⣀⣠⣤⣤⣤⣤⣤⣤⣤⣤⣶⣶⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿││Heap size           25.7 MiB│
│       ⣀⣠⣤⣴⣶⣶⣶⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿││Committed           15.2 MiB│
│⣀⣠⣤⣴⣶⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿││Alloc rate        81.6 MiB/s│
│⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿││Fragmented            57.2 %│
│⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿││                            │
╰ -121s ──────────────────────────────────────────────────────╯╰────────────────────────────╯
╭ Heap by generation ─────────────────────────────────────────╮╭ GC activity ───────────────╮
│ ░░░░░░░░░░  ░░░░░░░░░░  ░░░░░░░░░░  ██████████  ░░░░░░░░░░░ ││Gen 0 GCs              3/min│
│ ░░░░░░░░░░  ░░░░░░░░░░  ░░░░░░░░░░  ██████████  ░░░░░░░░░░░ ││Gen 1 GCs              3/min│
│ ░░░░░░░░░░  ░░░░░░░░░░  ░░░░░░░░░░  ██████████  ░░░░░░░░░░░ ││Gen 2 GCs              3/min│
│ ░░░░░░░░░░  ░░░░░░░░░░  ▁▁▁▁▁▁▁▁▁▁  ██████████  ░░░░░░░░░░░ ││Time in GC             0.0 %│
│ ▁▁▁▁▁▁▁▁▁▁  ▁▁▁▁▁▁▁▁▁▁  ██████████  ██████████  ▂▂▂▂▂▂▂▂▂▂▂ ││Pause time            4.2 ms│
│ 131.7 KiB    17.7 KiB   941.1 KiB    4.09 MiB    166.1 KiB  ││Gen 0 budget       119.2 MiB│
│   Gen 0       Gen 1       Gen 2        LOH          POH     ││                            │
╰─────────────────────────────────────────────────────────────╯╰────────────────────────────╯
```

Every graph is filled from the baseline and coloured by height, so a spike changes colour as it
climbs rather than only getting taller. One sample per sub-column, no interpolation: what you see
is what the runtime reported, and the newest reading is always at the right edge.

## Usage

```bash
countercow                 # pick a process from a list
countercow --pid 1234      # attach directly
countercow --name MyApi    # attach by name
countercow ps              # list attachable processes and exit
```

| Key | |
|---|---|
| `q` / `Esc` | quit |
| `d` | detach and pick another process |
| `i` | investigate: allocations, GC causes, exceptions, contention |
| `c` | CPU profile: which methods are burning time |
| `p` | pause history (the session keeps running) |
| `-` / `+` | refresh faster / slower |
| `m` | cycle braille / block / octant plotting |
| `?` | help |

Detaching closes the EventPipe session properly and re-discovers processes, so it picks up
anything that has started since — handy when the thing you want to watch is still building.

`-` and `+` step the refresh rate between 0.25s and 10s (`--interval` sets the starting point).
The runtime is told the rate when the counter session opens and there is no way to change it in
place, so each step closes that session and opens another. History gathered at the old rate is
dropped rather than spliced onto the new — the graphs place samples by index, so keeping it would
silently stretch part of the trace.

## Investigating

Counters tell you *that* the heap is growing. `i` tells you *what* is growing it:

```
╭ Allocations by type ───────────────────────────────────╮╭ Collections ─────────────╮
│System.Byte[]                     LOH   228.0 MiB  97.9%││#251  gen2 large    4.0 ms│
│System.String                     SOH   520.5 KiB   0.2%││#250  gen2 large    4.3 ms│
│System.IO.Pipelines.Pipe          SOH   312.5 KiB   0.1%││#249  gen2 large    4.2 ms│
╰────────────────────────────────────────────────────────╯╰──────────────────────────╯
╭ Exceptions thrown ─────────────────────────────────────╮╭ Lock contention ─────────╮
│System.InvalidOperationExcep sample failure        1,425││Waits                    1│
╰────────────────────────────────────────────────────────╯╰──────────────────────────╯
```

That reads as one causal story: byte arrays are going to the large object heap, which is forcing
gen 2 collections, at ~4 ms of pause each.

This opens a **second** EventPipe session carrying `Microsoft-Windows-DotNETRuntime` events, and
unlike counters it costs the target real CPU — counters arrive at roughly 40 events/second, where
the GC keyword alone produced ~1,600/second on a loaded app. So the session exists only while that
screen is open, and is closed the moment you leave. Findings are kept when you flick back to the
dashboard; `r` clears them.

Allocation counts are *sampled* — the runtime emits a tick roughly every 100 KB of small-object
allocation, and per large-object allocation — so treat the byte totals as a good estimate of where
pressure comes from rather than an exact ledger.

## CPU profiling

`c` runs a fixed five-second profile and ranks methods by self time:

```
   SELF    TOTAL  METHOD
  21.4%    37.1%  Workload.Checksum
  15.7%    15.7%  Workload.Mix
  14.9%    14.9%  (native / runtime code)
```

**Self** is time with the method as the innermost frame — time spent *in* it. **Total** includes
its callees, so `Checksum` at 37.1% is its own 21.4% plus the 15.7% it spends inside `Mix`.
Ranking is by self, because a list sorted by total is topped by whatever sits at the bottom of
every stack.

Unlike the other screens this one is not live, and cannot be. Stack frames arrive as bare
instruction pointers; the table mapping them to method names is only emitted when the session
*stops*. So a profile is necessarily "collect for a window, then resolve".

Two things worth knowing about the numbers:

- **Parked threads are excluded by default.** The sampler measures thread time, not CPU time — it
  samples every thread on every tick, including ones asleep in a wait. Unfiltered, the list is
  topped by whichever thread idles longest. The header reports what share was parked, and `w`
  shows them anyway. The runtime *does* tag each sample with a type that should make this
  distinction, but on macOS/arm64 every sample reports `External`, measured across 27,000 samples
  of a busy process — so parked threads are identified from their stacks instead.
- **`(native / runtime code)` is normal and often large.** The GC, the JIT and syscalls are not
  jitted methods and have no names to resolve to. Naming them anything else would be a guess.

## What it shows

The dashboard adapts to the process. **ASP.NET Core** apps get request and Kestrel connection
panels alongside the GC view; everything else gets a wider runtime and JIT view instead.

There is no "is this ASP.NET" flag anywhere in the diagnostics protocol, and subscribing to a
provider no application implements succeeds silently rather than failing. So countercow subscribes
optimistically and shows a panel once its provider actually reports. Panels are hidden, never shown
empty.

GC and memory get the most room on both layouts, because that is usually what you attached to find
out about.

## Building

```bash
cargo build --release
```

**Requires Rust 1.88+**, verified by building and running the full suite on 1.88 and 1.92, not
just declared. No other dependencies.

The floor is ratatui's. `sysinfo` is pinned to 0.38 rather than 0.39 for the same reason — 0.39
raised its own floor to 1.95 without needing anything this uses — and pinned *exactly* because
sysinfo changes its API across minor versions, so a float there breaks builds rather than merely
moving them.

```bash
scripts/msrv-report.sh            # what the dependency tree actually demands
scripts/crate-msrv-history.sh sysinfo   # how far back a crate stays compatible
```

## How it works

Three layers, bottom-up:

| Module | |
|---|---|
| `src/ipc/` | The Diagnostics IPC protocol: socket discovery, message framing, `ProcessInfo`, `CollectTracing2`. Ported from `dotnet/diagnostics`. |
| `src/nettrace/` | The nettrace V4 stream format with V5 metadata tags — FastSerializer framing, block dispatch, compressed event headers, metadata and payload decoding. |
| `src/counters/` | Turning `EventCounters` events into samples, and knowing how to present them. |
| `src/runtime/` | The investigation session: GC, allocation, exception and contention events. |
| `src/profile/` | CPU profiling: samples, stacks, the method rundown, and hot-method ranking. |

Counter display names and units are read off the wire rather than from a table, because the
runtime sends them in every payload — and because the table that used to hold them
(`KnownData.cs`) was deleted from `dotnet/diagnostics` in 2024.

A few things in this protocol fail *silently* rather than loudly, and are worth knowing if you're
reading the code:

- **The socket directory is `$TMPDIR`, not `/tmp`.** On macOS that's a per-user sandbox path, so
  globbing `/tmp` finds nothing at all even with a dozen .NET processes running. It also means
  `sudo countercow` searches *root's* `$TMPDIR` on macOS and finds less than running unprivileged,
  not more — which is why the empty-list screen names the directory it searched.
- **Most sockets on disk are stale.** A typical developer machine has far more dead socket files
  than live processes. The filename embeds the process start time, which countercow checks against
  the live process to avoid attaching to a recycled PID — a guard the reference client doesn't have.
- **Kestrel's EventCounter provider is hyphenated** (`Microsoft-AspNetCore-Server-Kestrel`). The
  dotted form is the .NET 8+ *Meter*, a different mechanism. Subscribe to the wrong one and you get
  no error and no data.
- **The `Trace` object has no size prefix or alignment padding**, unlike every other nettrace
  block. Treating it like the others desynchronises the whole stream.
- **V1 and V2 metadata field lists are ordered oppositely** — type-then-name versus
  size-then-name-then-type.
- **After `StopTracing` you must keep draining the original socket** — *while the stop is in
  flight*, not just afterwards. If the streaming socket's buffer fills, the runtime blocks writing
  to it and never processes the stop, so a caller that pauses its reader to issue the stop
  deadlocks. Only shows up on high-volume sessions.
- **`Microsoft-Windows-DotNETRuntime` is manifest-based**, so unlike EventSource providers it
  sends no event names and no field lists — only a numeric id. Its payloads can only be decoded
  from schemas hardcoded per `(id, version)`.
- **Stack ids are recycled** after a sequence point, but stack blocks also lag the events that
  reference them. Resolving eagerly loses most samples; resolving at the end reads the wrong
  stacks and silently yields addresses in no method at all. Two generations of stack table,
  rotated at each sequence point, is what gets both right.

v1 reads EventCounters, which work unchanged from .NET 6 through .NET 10. The newer
`System.Diagnostics.Metrics` meters arrived piecemeal (ASP.NET Core in .NET 8, `System.Runtime`
only in .NET 9) and are a natural v2.

## Testing

```bash
cargo test
```

Unit tests cover the byte-level rules that fail silently. Beyond those:

- **`tests/fixtures/*.nettrace`** are real streams captured from real runtimes — .NET 8, 9 and 10,
  ASP.NET and console, idle and under load. Synthetic tests only prove the parser matches my
  reading of the spec; these prove it matches what runtimes actually emit.
- **`tests/dashboard.rs`** renders both layouts from that fixture data at sizes from 200x60 down to
  20x5.

```bash
cargo run --example preview -- [aspnet|generic|loaded|console|investigate] [w] [h]
cargo run --example capture -- <pid> <path> [seconds] [counters|runtime|profile]
cargo run --example profile_cli -- <pid> [seconds] [--all] [--stacks]
cargo run --example probe -- <pid> <provider> <keywords-hex> [seconds] [ids]
```

`probe` is the investigation tool: it subscribes to any provider and reports what actually
arrives, including raw payload bytes. Every hardcoded event layout in `src/runtime/` and
`src/profile/` was derived and verified with it, and it is the way to re-verify on a runtime
version this has not seen.

`testapps/` holds a small ASP.NET Core API and a console app that move counters in recognisable
ways — endpoints for allocating, retaining, throwing and queueing work. They multi-target so the
parser can be checked against several runtimes:

```bash
dotnet run --project testapps/aspnet-sample -f net10.0 -- --urls http://localhost:5199
scripts/drive-load.sh http://localhost:5199 20
countercow --name CounterCowSampleApi
```

## Licence

MIT — see [LICENSE](LICENSE).

countercow is an independent reimplementation of protocols Microsoft documents and implements in
the open. Nothing here is copied, but the wire formats were derived by reading, and verified
against, these MIT-licensed projects — credit where it is due:

- [dotnet/diagnostics](https://github.com/dotnet/diagnostics) — the Diagnostics IPC protocol spec
  and the reference client this one is modelled on.
- [microsoft/perfview](https://github.com/microsoft/perfview) — where the nettrace format is
  actually specified, and the TraceEvent reader that settles what the spec leaves ambiguous.
- [dotnet/runtime](https://github.com/dotnet/runtime) — the writer, which is the final authority
  whenever the two disagree.
