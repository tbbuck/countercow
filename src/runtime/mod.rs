//! Runtime event investigation: what allocates, why collections happen, what throws, what blocks.
//!
//! Counters tell you *that* the heap is growing; these events tell you *what* is growing it.
//! They come from `Microsoft-Windows-DotNETRuntime`, a manifest-based provider — see
//! [`events`] for why that changes how it must be decoded.

#![allow(dead_code)]

pub mod events;
pub mod session;
pub mod state;
