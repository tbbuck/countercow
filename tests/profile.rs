//! CPU profile tests.
//!
//! Two layers. Synthetic samples pin down the ranking arithmetic exactly, where knowing the right
//! answer matters more than realism. A captured session from a live .NET 10 process then checks
//! the whole chain — stack blocks, the method rundown, address resolution — against a workload
//! whose hot methods are known by construction.

use countercow::app::{App, ProfilePhase, View};
use countercow::ipc::commands::ProcessInfo;
use countercow::ipc::discovery::DotnetProcess;
use countercow::profile::methods::{Method, MethodTable};
use countercow::profile::run::{self, ProfileResult};
use countercow::profile::state::ProfileState;
use countercow::ui::profile;
use countercow::ui::theme::Theme;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Captured from `testapps/aspnet-sample` on net10.0 while `/compute` was being hammered. That
/// endpoint runs `Workload.Checksum`, which calls `Workload.Mix`, and does nothing else — so the
/// profile has a known right answer.
const CPU_PROFILE: &[u8] = include_bytes!("fixtures/profile-net10-cpu.nettrace");

fn collect_fixture() -> ProfileResult {
    run::collect(std::io::Cursor::new(CPU_PROFILE), |_| {}).expect("the capture parses")
}

/// Addresses chosen so each method occupies an obvious range.
const MIX: u64 = 1_000;
const CHECKSUM: u64 = 2_000;
const ENDPOINT: u64 = 3_000;
const MONITOR_WAIT: u64 = 4_000;

fn methods() -> MethodTable {
    let mut table = MethodTable::new();
    for (start, namespace, name) in [
        (MIX, "Workload", "Mix"),
        (CHECKSUM, "Workload", "Checksum"),
        (ENDPOINT, "Microsoft.AspNetCore.Routing", "EndpointMiddleware.Invoke"),
        (MONITOR_WAIT, "System.Threading", "Monitor.Wait"),
    ] {
        table.insert(Method {
            start_address: start,
            size: 100,
            namespace: namespace.into(),
            name: name.into(),
            signature: "()".into(),
        });
    }
    table.finish();
    table
}

/// A profile shaped like the real one: work under an endpoint, plus idle threads.
fn result() -> ProfileResult {
    let mut state = ProfileState::new();
    // 30 samples deep in Mix, 20 in Checksum itself, both under the endpoint.
    for _ in 0..30 {
        state.record_stack(&[MIX + 10, CHECKSUM + 10, ENDPOINT + 10]);
    }
    for _ in 0..20 {
        state.record_stack(&[CHECKSUM + 20, ENDPOINT + 10]);
    }
    // 50 samples of threads parked in a wait.
    for _ in 0..50 {
        state.record_stack(&[MONITOR_WAIT + 5]);
    }
    ProfileResult { state, methods: methods(), sample_stacks: Vec::new() }
}

fn app() -> App {
    let process = DotnetProcess {
        pid: 4242,
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
    App::new(process, info, 1.0)
}

fn finished() -> App {
    let mut app = app();
    app.start_profile();
    app.finish_profile(result());
    app
}

fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let theme = Theme::default();
    terminal.draw(|frame| profile::render(frame, app, &theme)).unwrap();

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
fn the_hottest_method_leads_the_list() {
    let app = finished();
    let hot = app.profile.as_ref().unwrap();

    assert_eq!(hot.rows[0].name, "Workload.Mix", "30 self samples");
    assert_eq!(hot.rows[0].self_samples, 30);
    assert_eq!(hot.rows[1].name, "Workload.Checksum", "20 self samples");
}

#[test]
fn total_time_includes_callees() {
    let app = finished();
    let hot = app.profile.as_ref().unwrap();

    let checksum = hot.rows.iter().find(|m| m.name == "Workload.Checksum").unwrap();
    // Checksum is on 50 stacks: its own 20 plus the 30 where it called Mix.
    assert_eq!(checksum.self_samples, 20);
    assert_eq!(checksum.total_samples, 50);

    // The middleware did no work itself but is under everything.
    let endpoint = hot.rows.iter().find(|m| m.name.contains("EndpointMiddleware")).unwrap();
    assert_eq!(endpoint.self_samples, 0);
    assert_eq!(endpoint.total_samples, 50);
}

#[test]
fn parked_threads_are_hidden_but_counted() {
    let app = finished();
    let hot = app.profile.as_ref().unwrap();

    assert!(
        !hot.rows.iter().any(|m| m.name.contains("Monitor.Wait")),
        "a sleeping thread is not hot code"
    );
    assert_eq!(hot.waiting_samples, 50);
    assert_eq!(hot.working_samples, 50);
    assert_eq!(hot.waiting_percent(), 50.0);
}

#[test]
fn showing_parked_threads_reranks_without_reprofiling() {
    let mut app = finished();
    let before = app.profile.as_ref().unwrap().rows.len();

    app.toggle_profile_waiting();

    let hot = app.profile.as_ref().unwrap();
    assert!(hot.rows.len() > before, "the wait frame reappears");
    assert_eq!(hot.rows[0].name, "System.Threading.Monitor.Wait", "50 samples, the most of any");
    assert_eq!(hot.waiting_samples, 0);
    // Still finished — toggling must not restart collection.
    assert_eq!(app.profile_phase, ProfilePhase::Done);
}

#[test]
fn the_screen_shows_the_ranking() {
    let output = render(&finished(), 100, 24);
    assert!(output.contains("CPU profile"));
    assert!(output.contains("Hot methods"));
    assert!(output.contains("Workload.Mix"));
    assert!(output.contains("Workload.Checksum"));
    assert!(output.contains("SELF"));
    assert!(output.contains("TOTAL"));
    // 30 of 100 usable samples.
    assert!(output.contains("30.0%"));
}

#[test]
fn collecting_shows_progress_rather_than_an_empty_table() {
    let mut app = app();
    app.start_profile();

    let output = render(&app, 100, 24);
    assert!(output.contains("Collecting"));
    assert!(output.contains("remaining"));
    // The screen should explain why nothing is showing yet.
    assert!(output.contains("results appear at the end"));
}

#[test]
fn resolving_is_distinguished_from_collecting() {
    let mut app = app();
    app.start_profile();
    app.profile_phase = ProfilePhase::Resolving;

    let output = render(&app, 100, 24);
    assert!(output.contains("Resolving"));
    assert!(output.contains("method table"));
}

#[test]
fn a_failure_is_reported_with_a_way_forward() {
    let mut app = app();
    app.profile_phase = ProfilePhase::Failed("the process is no longer listening".into());

    let output = render(&app, 100, 24);
    assert!(output.contains("Profile failed"));
    assert!(output.contains("no longer listening"));
    assert!(output.contains("try again"));
}

#[test]
fn a_process_doing_nothing_says_so() {
    let mut app = app();
    app.start_profile();

    let mut state = ProfileState::new();
    for _ in 0..40 {
        state.record_stack(&[MONITOR_WAIT + 5]);
    }
    app.finish_profile(ProfileResult {
        state,
        methods: methods(),
        sample_stacks: Vec::new(),
    });

    let output = render(&app, 100, 24);
    assert!(output.contains("No managed code was executing"));
    assert!(output.contains("parked"));
}

#[test]
fn starting_a_profile_enters_the_screen_and_the_collecting_phase() {
    let mut app = app();
    assert_eq!(app.view, View::Dashboard);

    app.start_profile();
    assert_eq!(app.view, View::Profile);
    assert!(app.profile_collecting());
    assert!(!app.profile_window_elapsed(), "the window has just opened");
    assert!(app.profile_remaining_secs() > 0.0);
}

#[test]
fn leaving_mid_collection_cancels_rather_than_leaving_a_session_running() {
    let mut app = app();
    app.start_profile();

    app.cancel_profile();
    assert_eq!(app.view, View::Dashboard);
    assert!(!app.profile_collecting(), "the event loop stops the session on this");
}

#[test]
fn re_running_discards_the_previous_result() {
    let mut app = finished();
    assert!(app.profile.is_some());

    app.start_profile();
    assert!(app.profile.is_none(), "stale results must not linger under a new run");
    assert!(app.profile_collecting());
}

#[test]
fn renders_at_every_size_without_panicking() {
    let app = finished();
    for (width, height) in [(200u16, 60u16), (120, 36), (100, 24), (80, 20), (64, 14), (40, 8)] {
        assert!(!render(&app, width, height).is_empty(), "{width}x{height}");
    }
}

#[test]
fn below_the_minimum_says_so() {
    assert!(render(&finished(), 40, 8).contains("Terminal too small"));
}

// --- against a real captured session ---

#[test]
fn a_real_capture_yields_samples_and_a_method_table() {
    let result = collect_fixture();
    assert!(result.state.samples > 10_000, "got {} samples", result.state.samples);
    assert!(result.methods.len() > 1_000, "got {} methods", result.methods.len());
}

#[test]
fn nearly_every_stack_resolves() {
    // Stack ids are recycled, and the two-generation table exists to stop that losing samples.
    // A regression there shows up here as a jump in unresolved stacks.
    let result = collect_fixture();
    let unresolved = result.state.unresolved_stacks as f64 / result.state.samples as f64;
    assert!(unresolved < 0.05, "{:.1}% of stacks unresolved", unresolved * 100.0);
}

#[test]
fn the_known_hot_methods_come_out_on_top() {
    // The workload does nothing but Checksum -> Mix, so those two must lead the ranking.
    let result = collect_fixture();
    let hot = result.state.hot_methods(&result.methods, 20, false);

    let leaders: Vec<&str> =
        hot.rows.iter().take(4).map(|m| m.name.as_str()).collect();
    assert!(
        leaders.contains(&"Workload.Checksum"),
        "expected Checksum near the top, got {leaders:?}"
    );
    assert!(
        leaders.contains(&"Workload.Mix"),
        "expected Mix near the top, got {leaders:?}"
    );
}

#[test]
fn a_callers_total_time_covers_its_callee() {
    // Checksum's only callee is Mix, so Checksum's total must account for both.
    let result = collect_fixture();
    let hot = result.state.hot_methods(&result.methods, 50, false);

    let checksum = hot.rows.iter().find(|m| m.name == "Workload.Checksum").unwrap();
    let mix = hot.rows.iter().find(|m| m.name == "Workload.Mix").unwrap();

    assert!(
        checksum.total_samples >= checksum.self_samples + mix.self_samples,
        "Checksum total {} should cover its own {} plus Mix's {}",
        checksum.total_samples,
        checksum.self_samples,
        mix.self_samples
    );
    assert!(mix.total_samples <= checksum.total_samples, "a callee cannot exceed its caller");
}

#[test]
fn percentages_stay_within_bounds() {
    let result = collect_fixture();
    let hot = result.state.hot_methods(&result.methods, 100, false);

    for method in &hot.rows {
        assert!(
            (0.0..=100.0).contains(&method.self_percent),
            "{} self {}%",
            method.name,
            method.self_percent
        );
        assert!(
            (0.0..=100.0).contains(&method.total_percent),
            "{} total {}%",
            method.name,
            method.total_percent
        );
        assert!(method.total_samples >= method.self_samples, "{}", method.name);
    }
}

#[test]
fn resolved_names_are_real_method_names() {
    let result = collect_fixture();
    let hot = result.state.hot_methods(&result.methods, 40, false);

    // A misread address range yields either the native bucket or mojibake, never a plausible
    // dotted identifier — so requiring most names to look like methods catches drift.
    let named = hot
        .rows
        .iter()
        .filter(|m| m.name.contains('.') && m.name.is_ascii())
        .count();
    assert!(named > hot.rows.len() / 2, "only {named} of {} look like methods", hot.rows.len());
}

#[test]
fn the_captured_profile_renders() {
    let mut app = app();
    app.start_profile();
    app.finish_profile(collect_fixture());

    let output = render(&app, 120, 30);
    assert!(output.contains("Workload"), "the hot methods should be on screen");
    assert!(output.contains("Hot methods"));
}
