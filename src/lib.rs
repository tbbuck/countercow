//! countercow — a btop-style TUI for .NET runtime counters.
//!
//! The crate is layered bottom-up:
//!
//! * [`ipc`] speaks the .NET Diagnostics IPC protocol over a Unix socket, with no managed
//!   dependency — no .NET SDK or `dotnet-counters` needs to be installed.
//! * [`nettrace`] parses the EventPipe stream that a tracing session returns.
//! * [`counters`] turns `EventCounters` events into samples worth displaying.

pub mod app;
pub mod counters;
pub mod ipc;
pub mod nettrace;
pub mod ui;
