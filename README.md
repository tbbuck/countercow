# countercow

A `btop`-style terminal dashboard for .NET runtime counters.

`dotnet-counters` shows you the same data as a flat, scrolling table of name/value pairs — no
history, no shape, no sense of what matters for the kind of app you're looking at. countercow is
the same gap `btop` closed over `top`.

It is a single self-contained Rust binary. **No .NET SDK, no `dotnet-counters`, no managed
dependency at runtime** — it speaks the .NET Diagnostics IPC and EventPipe protocols directly over
a Unix socket. Linux and macOS.

```
┌ Heap size — 25.7 MiB ──────────────────────────────────┐┌ Memory ────────────┐
│29.6 MiB│                                  ⣀⣀⣀⣀⣀⣀⡠⠤⠤⠤││Working set 118.5 MiB│
│        │              ⣀⣀⣀⣀⡠⠤⠤⠤⠤⠒⠒⠒⠊⠉⠉⠉⠉          ││Heap size    25.7 MiB│
│14.8 MiB│⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠤⠔⠒⠒⠒⠉⠉⠉⠉                        ││Alloc rate 81.6 MiB/s│
└────────────────────────────────────────────────────────┘└─────────────────────┘
┌ Requests/sec — 101/s ──────────────────┐┌ Requests ────┐┌ Kestrel ───────────┐
│116/s│              ⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀││Rate     101/s││Total          3,967│
│58/s │⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉                ││Total    1,233││Conn queue         0│
└─────────────────────────────────────────┘└──────────────┘└────────────────────┘
```

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
| `p` | pause history (the session keeps running) |
| `m` | switch braille / octant plotting |
| `?` | help |

Detaching closes the EventPipe session properly and re-discovers processes, so it picks up
anything that has started since — handy when the thing you want to watch is still building.

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

Requires Rust 1.88+ (ratatui's MSRV). No other dependencies.

## How it works

Three layers, bottom-up:

| Module | |
|---|---|
| `src/ipc/` | The Diagnostics IPC protocol: socket discovery, message framing, `ProcessInfo`, `CollectTracing2`. Ported from `dotnet/diagnostics`. |
| `src/nettrace/` | The nettrace V4 stream format with V5 metadata tags — FastSerializer framing, block dispatch, compressed event headers, metadata and payload decoding. |
| `src/counters/` | Turning `EventCounters` events into samples, and knowing how to present them. |

Counter display names and units are read off the wire rather than from a table, because the
runtime sends them in every payload — and because the table that used to hold them
(`KnownData.cs`) was deleted from `dotnet/diagnostics` in 2024.

A few things in this protocol fail *silently* rather than loudly, and are worth knowing if you're
reading the code:

- **The socket directory is `$TMPDIR`, not `/tmp`.** On macOS that's a per-user sandbox path, so
  globbing `/tmp` finds nothing at all even with a dozen .NET processes running.
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
- **After `StopTracing` you must keep draining the original socket.** The runtime writes rundown
  into it before closing.

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
cargo run --example preview -- [aspnet|generic|loaded|console] [width] [height]
cargo run --example capture -- <pid> <path> [seconds]     # regenerate a fixture
```

`testapps/` holds a small ASP.NET Core API and a console app that move counters in recognisable
ways — endpoints for allocating, retaining, throwing and queueing work. They multi-target so the
parser can be checked against several runtimes:

```bash
dotnet run --project testapps/aspnet-sample -f net10.0 -- --urls http://localhost:5199
scripts/drive-load.sh http://localhost:5199 20
countercow --name CounterCowSampleApi
```
