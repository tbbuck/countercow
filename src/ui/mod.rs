//! Terminal lifecycle and the two interactive screens.
//!
//! Threads and channels rather than an async runtime: one socket and a keyboard is not a
//! concurrency problem. A reader thread blocks on input, a session thread blocks on the nettrace
//! socket, and the main thread coalesces whatever has arrived before drawing a frame.

pub mod chart;
pub mod dashboard;
pub mod gradient;
pub mod graph;
pub mod investigate;
pub mod panels;
pub mod picker;
pub mod profile;
pub mod theme;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::app::{self, App, AppEvent, Exit, Status, View};
use crate::counters::session;
use crate::ipc::commands::{self, ProcessInfo};
use crate::ipc::discovery::{self, DotnetProcess};
use crate::profile::run as profile_run;
use crate::profile::session as profile_session;
use crate::runtime::session as runtime_session;

use picker::{Entry, Picker};
use theme::Theme;

/// Frame budget. Counters arrive once a second, so this only governs how quickly the UI reacts
/// to input and how smoothly the uptime clock ticks.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// Bounded so a pathological burst of samples cannot grow without limit; ample for ~40 counters
/// per interval.
const CHANNEL_CAPACITY: usize = 1024;

pub type Result<T> = color_eyre::Result<T>;

/// Fail with an explanation rather than a panic when there is no terminal to draw on.
///
/// Piping or redirecting countercow is an easy mistake to make, and ratatui's own failure here is
/// a crash report about an unconfigured device.
fn ensure_terminal() -> Result<()> {
    use std::io::IsTerminal;

    if std::io::stdout().is_terminal() {
        return Ok(());
    }
    color_eyre::eyre::bail!(
        "countercow draws an interactive dashboard and needs a terminal, but stdout is not one.\n\
         For non-interactive use try `countercow ps` to list processes, or \
         `countercow --pid <pid> dump` to print samples."
    )
}

/// Run countercow's interactive session: picker, dashboard, and back again on detach.
///
/// One terminal spans the whole session. Tearing it down between screens would flicker the
/// alternate screen on every detach.
pub fn run(mut target: Option<DotnetProcess>, interval: f64) -> Result<()> {
    ensure_terminal()?;
    let mut terminal = ratatui::init();
    let result = session_loop(&mut terminal, &mut target, interval);
    ratatui::restore();
    result
}

fn session_loop(
    terminal: &mut DefaultTerminal,
    target: &mut Option<DotnetProcess>,
    interval: f64,
) -> Result<()> {
    loop {
        // `--pid`/`--name` supply the first target; after a detach we always return to the picker.
        let process = match target.take() {
            Some(process) => process,
            None => match pick_process(terminal)? {
                Some(process) => process,
                None => return Ok(()),
            },
        };

        let info = commands::process_info(&process.socket)?;
        match run_dashboard(terminal, process, info, interval)? {
            Exit::Quit => return Ok(()),
            Exit::Detach => continue,
        }
    }
}

/// Show the picker and return the chosen process, or `None` if the user quit.
fn pick_process(terminal: &mut DefaultTerminal) -> Result<Option<DotnetProcess>> {
    // Re-discover every time: processes come and go while countercow is running, and a detach is
    // usually motivated by wanting something that has just started.
    let found = discovery::discover()?;

    // Ask each runtime who it is. Discovery reports "dotnet" for framework-dependent apps, which
    // is useless for choosing between them; the entry assembly name is what people recognise.
    // Best-effort: a process may exit between discovery and this query.
    let entries: Vec<Entry> = found
        .processes
        .iter()
        .map(|process| Entry {
            process: process.clone(),
            info: commands::process_info(&process.socket).ok(),
        })
        .collect();

    let mut picker = Picker::new(found, entries);
    run_picker(terminal, &mut picker, &Theme::default())?;

    Ok(if picker.cancelled {
        None
    } else {
        picker.selected().map(|e| e.process.clone())
    })
}

fn run_picker(terminal: &mut DefaultTerminal, picker: &mut Picker, theme: &Theme) -> Result<()> {
    loop {
        terminal.draw(|frame| picker.render(frame, theme))?;

        let Event::Key(key) = crossterm::event::read()? else {
            continue;
        };
        // Windows sends both press and release; only act once.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                picker.cancelled = true;
                return Ok(());
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.cancelled = true;
                return Ok(());
            }
            // Enter on an empty list does nothing; there is nothing to attach to.
            KeyCode::Enter if picker.selected().is_some() => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => picker.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => picker.move_by(1),
            KeyCode::PageUp => picker.move_by(-10),
            KeyCode::PageDown => picker.move_by(10),
            KeyCode::Home => picker.select_first(),
            KeyCode::End => picker.select_last(),
            _ => {}
        }
    }
}

/// Attach to a process and run the dashboard until the user leaves or the process exits.
fn run_dashboard(
    terminal: &mut DefaultTerminal,
    process: DotnetProcess,
    info: ProcessInfo,
    interval: f64,
) -> Result<Exit> {
    let socket = process.socket.clone();
    let (tx, rx) = bounded(CHANNEL_CAPACITY);

    let input_stop = Arc::new(AtomicBool::new(false));
    spawn_input_reader(tx.clone(), Arc::clone(&input_stop));

    // Opened before the first frame, so a target that cannot be traced reports why on the terminal
    // instead of showing a dashboard that never fills in.
    let counters = start_counters(&socket, interval, &tx)?;

    let mut app = App::new(process, info, interval);
    let mut theme = Theme::default();
    let result = event_loop(terminal, &mut app, &mut theme, &rx, &tx, &socket, counters);

    input_stop.store(true, Ordering::Relaxed);
    result.map(|()| app.exit.unwrap_or(Exit::Quit))
}

fn spawn_input_reader(tx: Sender<AppEvent>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            // Poll rather than block, so the thread notices the stop flag and exits instead of
            // outliving the terminal restore.
            match crossterm::event::poll(FRAME_INTERVAL) {
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx.send(AppEvent::Input(event)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
}

fn spawn_session_reader(
    stream: std::os::unix::net::UnixStream,
    interval: f64,
    tx: Sender<AppEvent>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let sender = tx.clone();
        let result = session::run(stream, interval, |sample| {
            if stop.load(Ordering::Relaxed) || sender.send(AppEvent::Sample(Box::new(sample))).is_err()
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });

        // A session we asked to stop reaching EOF is not news. It matters on a rate change, where
        // a replacement session is already running and must not be buried under the old one's
        // obituary.
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let ended = match result {
            Ok(()) => AppEvent::SessionEnded(None),
            Err(e) => AppEvent::SessionEnded(Some(e.to_string())),
        };
        let _ = tx.send(ended);
    });
}

/// A live session on the target, tracked so it can be closed the moment it is not wanted.
struct Session {
    session_id: u64,
    stop: Arc<AtomicBool>,
}

impl Session {
    /// Tell the reader thread to stop reporting. Its socket reaching EOF is expected from here on.
    fn silence(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn start_counters(socket: &std::path::Path, interval: f64, tx: &Sender<AppEvent>) -> Result<Session> {
    let session = session::start(socket, interval)?;
    let stop = Arc::new(AtomicBool::new(false));
    spawn_session_reader(session.stream, interval, tx.clone(), Arc::clone(&stop));
    Ok(Session { session_id: session.session_id, stop })
}

/// Tear the counter session down and open another at `interval`.
///
/// The rate is fixed when a session is created — the runtime is told it as provider filter data —
/// so the only way to change it is to ask for a new session. If the old one will not close, stop
/// there rather than stacking a second on top: its reader thread is blocked on a socket that now
/// has no way to reach EOF, and every further rate change would leak another of each.
fn restart_counters(
    counters: &mut Option<Session>,
    socket: &std::path::Path,
    tx: &Sender<AppEvent>,
    interval: f64,
) -> std::result::Result<(), String> {
    if let Some(active) = counters.take() {
        active.silence();
        if let Err(e) = session::stop(socket, active.session_id) {
            return Err(format!("could not close the counter session: {e}"));
        }
    }

    match start_counters(socket, interval, tx) {
        Ok(session) => {
            *counters = Some(session);
            Ok(())
        }
        Err(e) => Err(format!("could not restart counters at {interval}s: {e}")),
    }
}

/// Close the counter session at the end of a dashboard.
///
/// Closing it properly rather than letting the socket drop makes the runtime tear its EventPipe
/// session down promptly, which matters because detaching is possible: a session leaked per attach
/// would accumulate in the target process.
fn stop_counters(counters: &mut Option<Session>, socket: &std::path::Path, app: &App) {
    let Some(active) = counters.take() else {
        return;
    };
    active.silence();
    if let Err(e) = session::stop(socket, active.session_id) {
        // The usual cause is that the process already exited, which is not worth failing over.
        if !matches!(app.status, Status::Ended) {
            eprintln!("note: could not close the counter session cleanly: {e}");
        }
    }
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    theme: &mut Theme,
    rx: &Receiver<AppEvent>,
    tx: &Sender<AppEvent>,
    socket: &std::path::Path,
    counters: Session,
) -> Result<()> {
    let mut counters = Some(counters);
    let mut investigation: Option<Session> = None;
    let mut profiling: Option<Session> = None;

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| match app.view {
                View::Dashboard => dashboard::render(frame, app, theme),
                View::Investigate => investigate::render(frame, app, theme),
                View::Profile => profile::render(frame, app, theme),
            })?;

            // Block for the next event, then absorb everything else already queued. A full
            // interval delivers ~40 samples at once, and an investigation session delivers far
            // more; redrawing per event would be wasted work.
            match rx.recv_timeout(FRAME_INTERVAL) {
                Ok(event) => handle_event(app, theme, event),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                // Both producers are gone; nothing further can arrive.
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
            }
            while let Ok(event) = rx.try_recv() {
                handle_event(app, theme, event);
            }

            // A new refresh rate means a new counter session; the runtime is told the rate when
            // the session opens and there is no way to change it in place.
            if let Some(interval) = app.take_pending_interval() {
                match restart_counters(&mut counters, socket, tx, interval) {
                    Ok(()) => app.apply_interval(interval),
                    Err(error) => app.status = Status::Failed(error),
                }
            }

            // Reconcile the session with the screen. Runtime events cost the target real CPU, so
            // the session exists only while the user is looking at it.
            match (app.is_investigating(), investigation.is_some()) {
                (true, false) => investigation = start_investigation(app, tx, socket),
                (false, true) => stop_investigation(&mut investigation, socket),
                _ => {}
            }

            // A profile is a fixed window: start one when the screen opens, and stop the session
            // when the window is up — stopping is what makes the method names arrive.
            match (
                matches!(app.profile_phase, app::ProfilePhase::Collecting { .. }),
                profiling.is_some(),
            ) {
                (true, false) => profiling = start_profile(app, tx, socket),
                (false, true) if !app.is_profiling() => stop_profile(&mut profiling, socket),
                (true, true) if app.profile_window_elapsed() => {
                    app.profile_phase = app::ProfilePhase::Resolving;
                    stop_profile(&mut profiling, socket);
                }
                _ => {}
            }

            if app.exit.is_some() {
                return Ok(());
            }
        }
    })();

    stop_investigation(&mut investigation, socket);
    stop_profile(&mut profiling, socket);
    stop_counters(&mut counters, socket, app);
    result
}

fn start_profile(
    app: &mut App,
    tx: &Sender<AppEvent>,
    socket: &std::path::Path,
) -> Option<Session> {
    let session = match profile_session::start(socket) {
        Ok(session) => session,
        Err(e) => {
            app.profile_phase = app::ProfilePhase::Failed(e.to_string());
            return None;
        }
    };

    let session_id = session.session_id;
    let stop = Arc::new(AtomicBool::new(false));
    spawn_profile_reader(session.stream, tx.clone());
    Some(Session { session_id, stop })
}

fn stop_profile(profiling: &mut Option<Session>, socket: &std::path::Path) {
    let Some(active) = profiling.take() else {
        return;
    };
    active.stop.store(true, Ordering::Relaxed);
    let _ = profile_session::stop(socket, active.session_id);
}

/// Collect a profile off-thread.
///
/// Parsing tens of thousands of samples and ten thousand method records is fast but not free, and
/// the dashboard should keep drawing throughout.
fn spawn_profile_reader(stream: std::os::unix::net::UnixStream, tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        let progress_tx = tx.clone();
        let result = profile_run::collect(stream, |progress| {
            let _ = progress_tx.send(AppEvent::ProfileProgress(progress.samples));
        });

        let message = match result {
            Ok(profile) => AppEvent::ProfileDone(Box::new(profile)),
            Err(e) => AppEvent::ProfileFailed(e.to_string()),
        };
        let _ = tx.send(message);
    });
}

fn start_investigation(
    app: &mut App,
    tx: &Sender<AppEvent>,
    socket: &std::path::Path,
) -> Option<Session> {
    let session = match runtime_session::start(socket) {
        Ok(session) => session,
        Err(e) => {
            app.runtime_error = Some(e.to_string());
            return None;
        }
    };

    let session_id = session.session_id;
    let stop = Arc::new(AtomicBool::new(false));
    spawn_runtime_reader(session.stream, tx.clone(), Arc::clone(&stop));
    Some(Session { session_id, stop })
}

fn stop_investigation(investigation: &mut Option<Session>, socket: &std::path::Path) {
    let Some(active) = investigation.take() else {
        return;
    };
    active.stop.store(true, Ordering::Relaxed);
    // Best effort: the process may have exited, which is not worth reporting here.
    let _ = runtime_session::stop(socket, active.session_id);
}

fn spawn_runtime_reader(
    stream: std::os::unix::net::UnixStream,
    tx: Sender<AppEvent>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let sender = tx.clone();
        let result = runtime_session::run(stream, |event, qpc_frequency| {
            if stop.load(Ordering::Relaxed)
                || sender.send(AppEvent::Runtime(Box::new(event), qpc_frequency)).is_err()
            {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });

        // A read error after we asked to stop is just the socket closing under us.
        if let Err(e) = result {
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(AppEvent::RuntimeFailed(e.to_string()));
            }
        }
    });
}

fn handle_event(app: &mut App, theme: &mut Theme, event: AppEvent) {
    match event {
        AppEvent::Sample(sample) => app.record(*sample),
        AppEvent::SessionEnded(error) => {
            app.status = match error {
                Some(e) => Status::Failed(e),
                None => Status::Ended,
            };
        }
        AppEvent::Runtime(event, qpc_frequency) => app.record_runtime(*event, qpc_frequency),
        AppEvent::RuntimeFailed(error) => app.runtime_error = Some(error),
        AppEvent::ProfileProgress(samples) => app.profile_samples = samples,
        AppEvent::ProfileDone(result) => app.finish_profile(*result),
        AppEvent::ProfileFailed(error) => {
            app.profile_phase = app::ProfilePhase::Failed(error);
        }
        AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
            handle_key(app, theme, key);
        }
        AppEvent::Input(_) => {}
    }
}

fn handle_key(app: &mut App, theme: &mut Theme, key: KeyEvent) {
    // Help swallows the next keypress, so it can be dismissed with anything.
    if app.show_help {
        app.show_help = false;
        if !matches!(key.code, KeyCode::Char('?')) {
            return;
        }
        return;
    }

    if app.is_profiling() {
        match key.code {
            KeyCode::Char('c') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.cancel_profile();
            }
            KeyCode::Esc => app.cancel_profile(),
            // Re-running is the only way to refresh: a profile is a fixed window, not a stream.
            KeyCode::Char('r') if !app.profile_collecting() => app.start_profile(),
            KeyCode::Char('w') if !app.profile_collecting() => app.toggle_profile_waiting(),
            KeyCode::Char('q') => app.exit = Some(Exit::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.exit = Some(Exit::Quit);
            }
            KeyCode::Char('d') => app.exit = Some(Exit::Detach),
            _ => {}
        }
        return;
    }

    // Escape backs out of the investigation screen rather than quitting outright.
    if app.is_investigating() {
        match key.code {
            KeyCode::Char('i') | KeyCode::Esc => app.toggle_investigate(),
            KeyCode::Char('r') => app.runtime.reset(),
            KeyCode::Char('q') => app.exit = Some(Exit::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.exit = Some(Exit::Quit);
            }
            KeyCode::Char('d') => app.exit = Some(Exit::Detach),
            KeyCode::Char('?') | KeyCode::Char('h') => app.show_help = true,
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.exit = Some(Exit::Quit),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.exit = Some(Exit::Quit);
        }
        KeyCode::Char('d') => app.exit = Some(Exit::Detach),
        KeyCode::Char('i') => app.toggle_investigate(),
        KeyCode::Char('c') => app.start_profile(),
        KeyCode::Char('p') | KeyCode::Char(' ') => app.paused = !app.paused,
        KeyCode::Char('m') => theme.cycle_plot(),
        // btop's convention: `+` adds to the update timer and so slows it down. Both faces of each
        // key are bound, because whether you get `+` or `=` depends on the shift key.
        KeyCode::Char('-') | KeyCode::Char('_') => app.step_interval(true),
        KeyCode::Char('+') | KeyCode::Char('=') => app.step_interval(false),
        KeyCode::Char('?') | KeyCode::Char('h') => app.show_help = true,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::counters::sample::{CounterKind, CounterSample};
    use std::path::PathBuf;

    fn app() -> App {
        let process = DotnetProcess {
            pid: 1,
            socket: PathBuf::from("/tmp/s"),
            name: "Test".into(),
            command: "cmd".into(),
            start_key_verified: true,
        };
        App::new(process, ProcessInfo::default(), 1.0)
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn q_and_escape_quit() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut app = app();
            handle_key(&mut app, &mut Theme::default(), press(code));
            assert_eq!(app.exit, Some(Exit::Quit), "{code:?} should quit");
        }
    }

    #[test]
    fn d_detaches_rather_than_quitting() {
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('d')));
        assert_eq!(app.exit, Some(Exit::Detach));
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app();
        handle_key(
            &mut app,
            &mut Theme::default(),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.exit, Some(Exit::Quit));
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('c')));
        assert_eq!(app.exit, None);
    }

    #[test]
    fn p_toggles_pause() {
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('p')));
        assert!(app.paused);
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('p')));
        assert!(!app.paused);
    }

    #[test]
    fn m_switches_the_plot_glyphs() {
        let mut theme = Theme::default();
        handle_key(&mut app(), &mut theme, press(KeyCode::Char('m')));
        assert_eq!(theme.plot_name(), "block");
    }

    #[test]
    fn minus_asks_for_a_faster_rate_and_plus_a_slower_one() {
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('-')));
        assert_eq!(app.take_pending_interval(), Some(0.5));

        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('+')));
        assert_eq!(app.take_pending_interval(), Some(2.0));
    }

    #[test]
    fn the_unshifted_faces_of_the_rate_keys_work_too() {
        // Whether the key reports `+` or `=` depends on the shift key, and both should step.
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('=')));
        assert_eq!(app.take_pending_interval(), Some(2.0));

        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('_')));
        assert_eq!(app.take_pending_interval(), Some(0.5));
    }

    #[test]
    fn the_rate_keys_do_nothing_on_the_other_screens() {
        // Those sessions carry no interval, and restarting counters underneath them is pointless.
        let mut app = app();
        app.toggle_investigate();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('-')));
        assert_eq!(app.take_pending_interval(), None);
    }

    #[test]
    fn any_key_dismisses_help() {
        let mut app = app();
        app.show_help = true;
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('x')));
        assert!(!app.show_help);
        // Dismissing must not also act on the key.
        assert_eq!(app.exit, None);
    }

    #[test]
    fn quitting_while_help_is_open_takes_two_presses() {
        let mut app = app();
        app.show_help = true;
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('q')));
        assert_eq!(app.exit, None, "first press only closes help");
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('q')));
        assert_eq!(app.exit, Some(Exit::Quit));
    }

    #[test]
    fn detaching_while_help_is_open_also_takes_two_presses() {
        let mut app = app();
        app.show_help = true;
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('d')));
        assert_eq!(app.exit, None);
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('d')));
        assert_eq!(app.exit, Some(Exit::Detach));
    }

    #[test]
    fn samples_reach_the_app_state() {
        let mut app = app();
        let sample = CounterSample {
            provider: "System.Runtime".into(),
            name: "cpu-usage".into(),
            display_name: "CPU Usage".into(),
            display_units: "%".into(),
            value: 12.5,
            kind: CounterKind::Mean,
            interval_sec: 1.0,
            rate_time_scale: None,
            timestamp: 0,
        };
        handle_event(&mut app, &mut Theme::default(), AppEvent::Sample(Box::new(sample)));
        assert_eq!(app.value("System.Runtime", "cpu-usage"), Some(12.5));
    }

    #[test]
    fn session_end_is_reflected_in_status() {
        let mut clean = app();
        handle_event(&mut clean, &mut Theme::default(), AppEvent::SessionEnded(None));
        assert_eq!(clean.status, Status::Ended);

        let mut failed = app();
        handle_event(
            &mut failed,
            &mut Theme::default(),
            AppEvent::SessionEnded(Some("socket closed".into())),
        );
        assert_eq!(failed.status, Status::Failed("socket closed".into()));
    }
}
