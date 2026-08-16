//! Investigation tests, replaying a real captured runtime session.
//!
//! The fixture came from `testapps/aspnet-sample` under load, where the workload is known: the
//! `/alloc` endpoint allocates 1 MiB `byte[]`s (which go to the large object heap and force gen 2
//! collections) and `/throw` raises `InvalidOperationException`. So these tests check not just
//! that events decode, but that they add up to the right explanation.

use std::ops::ControlFlow;

use countercow::app::App;
use countercow::ipc::commands::ProcessInfo;
use countercow::ipc::discovery::DotnetProcess;
use countercow::runtime::events::{AllocationKind, GcReason};
use countercow::runtime::session as runtime_session;
use countercow::runtime::state::RuntimeState;
use countercow::ui::investigate;
use countercow::ui::theme::Theme;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

const RUNTIME: &[u8] = include_bytes!("fixtures/runtime-net10-loaded.nettrace");

fn replay() -> App {
    let process = DotnetProcess {
        pid: 73628,
        socket: "/tmp/socket".into(),
        name: "CounterCowSampleApi".into(),
        command: "cmd".into(),
        start_key_verified: true,
    };
    let info = ProcessInfo {
        os: "macOS".into(),
        arch: "arm64".into(),
        clr_version: Some("10.0.1".into()),
        ..Default::default()
    };

    let mut app = App::new(process, info, 1.0);
    app.toggle_investigate();
    runtime_session::run(std::io::Cursor::new(RUNTIME), |event, qpc| {
        app.record_runtime(event, qpc);
        ControlFlow::Continue(())
    })
    .expect("the captured session parses");
    app
}

/// The accumulated state from replaying the fixture. Goes through `App` rather than duplicating
/// its event dispatch, so these tests exercise the same path the live session takes.
fn state() -> RuntimeState {
    replay().runtime
}

fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let theme = Theme::default();
    terminal.draw(|frame| investigate::render(frame, app, &theme)).unwrap();

    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn a_real_session_decodes_into_events() {
    let state = state();
    assert!(state.events_seen > 1000, "expected a busy capture");
    assert!(state.allocation_ticks > 0);
    assert!(state.total_gcs() > 0);
}

#[test]
fn the_dominant_allocating_type_is_identified() {
    // The workload allocates 1 MiB byte arrays; nothing else comes close.
    let state = state();
    let top = state.top_allocations(5);
    assert_eq!(top[0].type_name, "System.Byte[]");
    assert_eq!(top[0].kind, AllocationKind::Large, "1 MiB arrays go to the LOH");

    let share = state.allocation_share(&top[0]);
    assert!(share > 90.0, "expected byte[] to dominate, got {share:.1}%");
}

#[test]
fn small_object_allocations_are_attributed_too() {
    // The long tail should be recognisable framework types, not garbage from a misread field.
    let state = state();
    let names: Vec<String> =
        state.top_allocations(20).into_iter().map(|s| s.type_name).collect();

    assert!(
        names.iter().any(|n| n.starts_with("System.") || n.starts_with("Microsoft.")),
        "expected framework types in the tail, got {names:?}"
    );
    assert!(
        names.iter().all(|n| !n.is_empty() && n.is_ascii()),
        "type names should be readable, got {names:?}"
    );
}

#[test]
fn collections_are_attributed_to_large_allocation() {
    // Allocating past the LOH threshold is exactly what should be driving these collections.
    let state = state();
    let collections = state.recent_collections(20);
    assert!(!collections.is_empty());

    assert!(
        collections.iter().all(|gc| gc.reason == GcReason::AllocLarge),
        "expected every collection to cite large allocation"
    );
    assert!(
        collections.iter().all(|gc| gc.generation == 2),
        "large object allocation forces gen 2"
    );
}

#[test]
fn pause_times_are_measured_and_plausible() {
    let state = state();
    let collections = state.recent_collections(20);
    let measured: Vec<f64> = collections.iter().filter_map(|gc| gc.pause_ms).collect();

    assert!(!measured.is_empty(), "at least some pauses should be paired");
    for pause in &measured {
        // A blocking gen 2 pause is milliseconds, not microseconds or minutes.
        assert!(*pause > 0.0 && *pause < 1000.0, "implausible pause: {pause} ms");
    }
    assert!(state.total_pause_ms > 0.0);
}

#[test]
fn exceptions_are_captured_with_type_and_message() {
    let state = state();
    let top = state.top_exceptions(5);
    assert!(!top.is_empty(), "the workload throws continuously");
    assert_eq!(top[0].type_name, "System.InvalidOperationException");
    assert_eq!(top[0].message, "sample failure");
    assert!(top[0].count > 10);
}

#[test]
fn allocation_totals_agree_with_the_counter_view() {
    // alloc-rate reported roughly 80 MB/s for this workload, over a ~3 second capture. This is a
    // sampled estimate, so the bar is order-of-magnitude agreement, not equality.
    let state = state();
    let mib = state.total_allocated_bytes as f64 / 1_048_576.0;
    assert!(
        (50.0..1000.0).contains(&mib),
        "expected roughly 100-300 MiB over the capture, got {mib:.1} MiB"
    );
}

#[test]
fn the_screen_renders_the_findings() {
    let output = render(&replay(), 120, 36);
    assert!(output.contains("Investigating"));
    assert!(output.contains("System.Byte[]"), "the dominant type should be visible");
    assert!(output.contains("LOH"));
    assert!(output.contains("large alloc"), "the GC cause should be visible");
    assert!(output.contains("System.InvalidOperationException"));
    assert!(output.contains("Lock contention"));
}

#[test]
fn renders_at_every_size_without_panicking() {
    let app = replay();
    for (width, height) in [(200u16, 60u16), (120, 36), (100, 30), (80, 24), (64, 16), (40, 10)] {
        let output = render(&app, width, height);
        assert!(!output.is_empty(), "{width}x{height} produced nothing");
    }
}

#[test]
fn below_the_minimum_says_so() {
    let output = render(&replay(), 40, 10);
    assert!(output.contains("Terminal too small"));
}

#[test]
fn an_empty_investigation_explains_itself_rather_than_showing_zeroes() {
    let process = DotnetProcess {
        pid: 1,
        socket: "/tmp/s".into(),
        name: "Idle".into(),
        command: "cmd".into(),
        start_key_verified: true,
    };
    let mut app = App::new(process, ProcessInfo::default(), 1.0);
    app.toggle_investigate();

    let output = render(&app, 120, 36);
    assert!(output.contains("Listening for runtime events"));
    assert!(output.contains("idle process may report nothing"));
}

#[test]
fn a_failed_session_reports_the_error() {
    let process = DotnetProcess {
        pid: 1,
        socket: "/tmp/s".into(),
        name: "Gone".into(),
        command: "cmd".into(),
        start_key_verified: true,
    };
    let mut app = App::new(process, ProcessInfo::default(), 1.0);
    app.toggle_investigate();
    app.runtime_error = Some("the process is no longer listening".into());

    let output = render(&app, 120, 36);
    assert!(output.contains("Investigation unavailable"));
    assert!(output.contains("no longer listening"));
    assert!(output.contains("return to the dashboard"));
}

#[test]
fn resetting_clears_the_findings_but_stays_on_the_screen() {
    let mut app = replay();
    assert!(!app.runtime.is_empty());

    app.runtime.reset();
    assert!(app.runtime.is_empty());
    assert!(app.is_investigating(), "reset should not leave the screen");
}

#[test]
fn leaving_and_returning_keeps_what_was_gathered() {
    // Flicking back to the dashboard should not throw away findings that cost the target CPU.
    let mut app = replay();
    let allocated = app.runtime.total_allocated_bytes;

    app.toggle_investigate();
    assert!(!app.is_investigating());
    app.toggle_investigate();

    assert!(app.is_investigating());
    assert_eq!(app.runtime.total_allocated_bytes, allocated);
}
