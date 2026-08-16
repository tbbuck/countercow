//! Terminal lifecycle and the two interactive screens.
//!
//! Threads and channels rather than an async runtime: one socket and a keyboard is not a
//! concurrency problem. A reader thread blocks on input, a session thread blocks on the nettrace
//! socket, and the main thread coalesces whatever has arrived before drawing a frame.

pub mod chart;
pub mod dashboard;
pub mod panels;
pub mod picker;
pub mod theme;

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::app::{App, AppEvent, Status};
use crate::counters::session;
use crate::ipc::commands::{self, ProcessInfo};
use crate::ipc::discovery::{self, DotnetProcess};

use picker::{Entry, Picker};
use theme::Theme;

/// Frame budget. Counters arrive once a second, so this only governs how quickly the UI reacts
/// to input and how smoothly the uptime clock ticks.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// Bounded so a pathological burst of samples cannot grow without limit; ample for ~40 counters
/// per interval.
const CHANNEL_CAPACITY: usize = 1024;

pub type Result<T> = color_eyre::Result<T>;

/// Show the picker and return the chosen process, or `None` if the user quit.
pub fn pick_process() -> Result<Option<DotnetProcess>> {
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
    let theme = Theme::default();
    let mut terminal = ratatui::init();
    let result = run_picker(&mut terminal, &mut picker, &theme);
    ratatui::restore();
    result?;

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
            KeyCode::Enter => {
                if picker.selected().is_some() {
                    return Ok(());
                }
            }
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

/// Attach to a process and run the dashboard until the user quits or the process exits.
pub fn run_dashboard(process: DotnetProcess, info: ProcessInfo, interval: f64) -> Result<()> {
    let socket = process.socket.clone();
    let session = session::start(&socket, interval)?;
    let session_id = session.session_id;

    let (tx, rx) = bounded(CHANNEL_CAPACITY);
    let stop = Arc::new(AtomicBool::new(false));

    spawn_input_reader(tx.clone(), Arc::clone(&stop));
    spawn_session_reader(session.stream, interval, tx, Arc::clone(&stop));

    let mut app = App::new(process, info, interval);
    let mut theme = Theme::default();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &mut theme, &rx);
    ratatui::restore();

    // Close the session properly rather than letting the socket drop, so the runtime tears down
    // its EventPipe session promptly.
    stop.store(true, Ordering::Relaxed);
    if let Err(e) = session::stop(&socket, session_id) {
        // The usual cause is that the process already exited, which is not worth failing over.
        if !matches!(app.status, Status::Ended) {
            eprintln!("note: could not close the counter session cleanly: {e}");
        }
    }

    result
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

        let ended = match result {
            Ok(()) => AppEvent::SessionEnded(None),
            // A read error after we asked to stop is just the socket closing under us.
            Err(_) if stop.load(Ordering::Relaxed) => AppEvent::SessionEnded(None),
            Err(e) => AppEvent::SessionEnded(Some(e.to_string())),
        };
        let _ = tx.send(ended);
    });
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    theme: &mut Theme,
    rx: &Receiver<AppEvent>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| dashboard::render(frame, app, theme))?;

        // Block for the next event, then absorb everything else already queued. A full interval
        // delivers ~40 samples at once; redrawing per sample would be wasted work.
        match rx.recv_timeout(FRAME_INTERVAL) {
            Ok(event) => handle_event(app, theme, event),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            // Both producers are gone; nothing further can arrive.
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        while let Ok(event) = rx.try_recv() {
            handle_event(app, theme, event);
        }

        if app.should_quit {
            return Ok(());
        }
    }
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

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Char('p') | KeyCode::Char(' ') => app.paused = !app.paused,
        KeyCode::Char('m') => theme.toggle_marker(),
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
            assert!(app.should_quit, "{code:?} should quit");
        }
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = app();
        handle_key(
            &mut app,
            &mut Theme::default(),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut app = app();
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('c')));
        assert!(!app.should_quit);
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
    fn m_switches_the_plot_marker() {
        let mut theme = Theme::default();
        handle_key(&mut app(), &mut theme, press(KeyCode::Char('m')));
        assert_eq!(theme.marker_name(), "octant");
    }

    #[test]
    fn any_key_dismisses_help() {
        let mut app = app();
        app.show_help = true;
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('x')));
        assert!(!app.show_help);
        // Dismissing must not also act on the key.
        assert!(!app.should_quit);
    }

    #[test]
    fn quitting_while_help_is_open_takes_two_presses() {
        let mut app = app();
        app.show_help = true;
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('q')));
        assert!(!app.should_quit, "first press only closes help");
        handle_key(&mut app, &mut Theme::default(), press(KeyCode::Char('q')));
        assert!(app.should_quit);
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
