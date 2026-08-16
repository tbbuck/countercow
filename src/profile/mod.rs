//! CPU profiling: which methods the process is actually spending time in.
//!
//! Samples arrive continuously, but the method names they resolve against only arrive when the
//! session stops. That shapes everything here — see [`session`].

#![allow(dead_code)]

pub mod methods;
pub mod run;
pub mod session;
pub mod state;
