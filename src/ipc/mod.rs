//! A from-scratch client for the .NET Diagnostics IPC protocol.
//!
//! Ported from `dotnet/diagnostics/src/Microsoft.Diagnostics.NETCore.Client`. No managed
//! dependency: countercow talks to the runtime's diagnostic server directly over a Unix socket.

// This module is a protocol client: it implements the commands and wire primitives the protocol
// defines, not only the subset any one caller happens to reach for today.
#![allow(dead_code)]

pub mod commands;
pub mod decode;
pub mod discovery;
pub mod encode;
pub mod frame;
pub mod transport;
