//! Leaving the terminal usable when countercow is killed rather than quit.
//!
//! `ratatui::init` puts the terminal into raw mode and switches to the alternate screen, and
//! `ratatui::restore` undoes both — but only if it is reached. A panic reaches it, because the
//! panic hook is installed alongside. A signal does not: the process dies mid-frame, the alternate
//! screen is never left and raw mode is never cleared, and the shell it was launched from is
//! unusable until `reset`. That is what `kill` and a dropped SSH connection both do.
//!
//! The counter session itself needs no help here. The runtime tears an EventPipe session down when
//! the client's socket closes — measured at under three seconds after a `SIGKILL`, which no
//! handler could have improved on — so what is lost when countercow is killed is the terminal, not
//! the session.

use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Signals worth catching: `kill` without arguments, and the terminal going away.
///
/// Not `SIGINT`. Raw mode turns off the terminal's signal generation, so Ctrl-C arrives as an
/// ordinary key event and is handled with the rest of them.
const CAUGHT: [libc::c_int; 2] = [libc::SIGTERM, libc::SIGHUP];

/// Record the signal and restore the default disposition, so a second one kills outright.
///
/// The handler does nothing else. Anything it touched would have to be async-signal-safe, and the
/// draw loop is already polling on a hundred-millisecond budget — it can do the tidying properly a
/// fraction of a second later. Standing down after the first signal is the safety net: if the loop
/// is wedged and never notices, the next `kill` behaves as it always would rather than finding a
/// process that has quietly made itself unkillable.
extern "C" fn note_signal(signal: libc::c_int) {
    unsafe { libc::signal(signal, libc::SIG_DFL) };
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// Start catching the signals that would otherwise kill us mid-frame.
pub fn listen() {
    for signal in CAUGHT {
        // SAFETY: the handler only writes an atomic and reinstates the default disposition, both
        // of which are safe to do from a signal.
        unsafe { libc::signal(signal, note_signal as *const () as libc::sighandler_t) };
    }
}

/// Whether a signal has asked countercow to stop.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_is_noticed_and_the_default_put_back() {
        assert!(!interrupted(), "nothing should have been raised yet");
        listen();

        // SAFETY: SIGHUP now runs the handler above rather than terminating the test binary, and
        // the handler stands the default back up, so this is raised exactly once.
        unsafe { libc::raise(libc::SIGHUP) };

        assert!(interrupted(), "the draw loop needs to be able to see this");
        assert_eq!(
            unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) },
            libc::SIG_DFL,
            "a second signal must kill outright rather than find us unkillable"
        );
    }
}
