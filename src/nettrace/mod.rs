//! A parser for the nettrace stream format emitted by .NET EventPipe.
//!
//! Targets nettrace V4 framing with V5 metadata tags, which covers .NET Core 3.0 through .NET 10.
//! The authoritative spec is `NetTraceFormat_v5.md` in microsoft/perfview — not dotnet/runtime,
//! and note that the published document contradicts the runtime in a couple of places (see
//! [`event::EventHeader::read_uncompressed`]).
//!
//! Only what a counter view needs is decoded: stack and sequence-point blocks are skipped.

#![allow(dead_code)]

pub mod blocks;
pub mod event;
pub mod fastserial;
pub mod metadata;
pub mod payload;
pub mod reader;
