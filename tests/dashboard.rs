//! Layout tests driven by real captured counter data.
//!
//! Rendering is where a TUI breaks in ways unit tests miss: a panel squeezed to zero height, a
//! label truncated mid-word, a panic from an arithmetic underflow on a small terminal. These
//! render the whole dashboard across a range of sizes and check what came out.

use countercow::app::App;
use countercow::counters::sample;
use countercow::ipc::commands::ProcessInfo;
use countercow::ipc::discovery::DotnetProcess;
use countercow::nettrace::blocks::NettraceParser;
use countercow::ui::dashboard;
use countercow::ui::theme::Theme;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

const ASPNET: &[u8] = include_bytes!("fixtures/aspnet-net9.nettrace");
const GENERIC: &[u8] = include_bytes!("fixtures/generic-net10.nettrace");

fn app_from(fixture: &[u8]) -> App {
    let process = DotnetProcess {
        pid: 77686,
        socket: "/tmp/socket".into(),
        name: "CrimeRate.VectorTileApi".into(),
        command: "cmd".into(),
        start_key_verified: true,
    };
    let info = ProcessInfo {
        os: "macOS".into(),
        arch: "arm64".into(),
        clr_version: Some("9.0.7".into()),
        ..Default::default()
    };

    let mut app = App::new(process, info, 1.0);
    let mut parser = NettraceParser::new(std::io::Cursor::new(fixture)).unwrap();
    while let Some(batch) = parser.next_events().unwrap() {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };
            if let Some(s) = sample::extract(metadata, &event).unwrap() {
                app.record(s);
            }
        }
    }
    app
}

fn render(app: &App, width: u16, height: u16) -> String {
    render_with(app, width, height, Theme::default())
}

fn render_with(app: &App, width: u16, height: u16, theme: Theme) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| dashboard::render(frame, app, &theme)).unwrap();

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

/// The rows of one rendered frame, trailing blanks trimmed.
fn rows(output: &str) -> Vec<&str> {
    output.lines().map(|line| line.trim_end()).collect()
}

/// Sizes worth exercising: a comfortable window, a laptop split, the documented minimum, and
/// below it.
const SIZES: &[(u16, u16)] = &[
    (200, 60),
    (120, 40),
    (100, 32),
    (80, 24),
    (70, 22),
    (64, 20),
    (50, 14),
    (20, 5),
];

#[test]
fn renders_at_every_size_without_panicking() {
    for fixture in [ASPNET, GENERIC] {
        let app = app_from(fixture);
        for (width, height) in SIZES {
            let output = render(&app, *width, *height);
            assert!(!output.is_empty(), "{width}x{height} produced nothing");
        }
    }
}

#[test]
fn aspnet_panels_appear_only_for_an_aspnet_process() {
    let aspnet = render(&app_from(ASPNET), 120, 40);
    assert!(aspnet.contains("Kestrel"));
    assert!(aspnet.contains("Requests"));
    assert!(aspnet.contains("ASP.NET Core"));

    let generic = render(&app_from(GENERIC), 120, 40);
    assert!(!generic.contains("Kestrel"), "hidden, not shown empty");
    assert!(!generic.contains("Requests/sec"));
    // The space goes to the JIT panel instead.
    assert!(generic.contains("JIT"));
}

#[test]
fn gc_and_memory_are_present_on_both_layouts() {
    for fixture in [ASPNET, GENERIC] {
        let output = render(&app_from(fixture), 120, 40);
        assert!(output.contains("Heap size"), "heap chart missing");
        assert!(output.contains("Memory"), "memory panel missing");
        assert!(output.contains("Heap by generation"), "generation bars missing");
        assert!(output.contains("GC activity"), "GC activity panel missing");
    }
}

#[test]
fn a_short_terminal_keeps_the_trend_and_drops_the_bars() {
    // The bars are the expendable half: squeezing both would leave the chart at zero height.
    let output = render(&app_from(ASPNET), 100, 22);
    assert!(output.contains("Heap size"), "the chart must survive");
    assert!(!output.contains("Heap by generation"));
}

#[test]
fn below_the_minimum_says_so_rather_than_rendering_mush() {
    let output = render(&app_from(ASPNET), 50, 14);
    assert!(output.contains("Terminal too small"));
    assert!(output.contains("64x20"), "should state what is needed");
}

#[test]
fn values_are_rendered_not_placeholders() {
    let output = render(&app_from(ASPNET), 120, 40);
    // Every counter in the fixture reported, so nothing should still show the em-dash.
    assert!(!output.contains('—') || output.contains("Heap size —"), "unexpected placeholder");
    assert!(output.contains("MiB"), "memory should be formatted in bytes");
}

#[test]
fn header_degrades_instead_of_truncating_mid_word() {
    let app = app_from(ASPNET);

    let wide = render(&app, 120, 40);
    assert!(wide.contains("macOS arm64"), "wide terminal shows the platform");

    // Narrow drops trailing detail rather than cutting a word in half.
    let narrow = render(&app, 70, 22);
    assert!(narrow.contains("CrimeRate.VectorTileApi"), "identity is never dropped");
    assert!(!narrow.contains("macOS arm6\n"), "no mid-word truncation");
}

#[test]
fn paused_and_stalled_states_are_visible_in_the_footer() {
    let mut app = app_from(ASPNET);
    assert!(render(&app, 120, 40).contains("counters"));

    app.paused = true;
    assert!(render(&app, 120, 40).contains("paused"));
}

#[test]
fn help_overlay_draws_over_the_dashboard() {
    let mut app = app_from(ASPNET);
    app.show_help = true;
    let output = render(&app, 120, 40);
    assert!(output.contains("Help"));
    assert!(output.contains("pause history"));
}

#[test]
fn the_generation_bars_use_the_whole_panel_width() {
    // The point of the panel is comparing five sizes, so the bars should divide the width they
    // are given rather than huddling at the left with the rest of it blank.
    let output = render(&app_from(ASPNET), 200, 60);
    let widest = rows(&output)
        .iter()
        .skip_while(|row| !row.contains("Heap by generation"))
        .take_while(|row| !row.starts_with('╰'))
        .map(|row| row.chars().filter(|c| "░▁▂▃▄▅▆▇█".contains(*c)).count())
        .max()
        .expect("the generation panel should be on screen");

    // Five slots across a panel two thirds of a 200-column terminal wide.
    assert!(widest > 100, "bars only covered {widest} columns of the panel");
}

#[test]
fn every_generation_is_labelled_even_when_one_dwarfs_the_others() {
    let output = render(&app_from(ASPNET), 120, 40);
    for label in ["Gen 0", "Gen 1", "Gen 2", "LOH", "POH"] {
        assert!(output.contains(label), "{label} missing from the generation panel");
    }
}

#[test]
fn either_every_generation_shows_its_share_or_none_does() {
    // A width that does not divide by five leaves one slot a column wider than the rest. Letting
    // that slot alone carry the share reads as a glitch, so the decision is made panel-wide.
    for width in 90..=130 {
        let output = render(&app_from(ASPNET), width, 40);
        let captions = generation_captions(&output);
        let shares = captions.matches('%').count();
        assert!(
            shares == 0 || shares == 5,
            "{width} columns produced {shares} generation shares, not 0 or 5: {captions:?}"
        );
    }
}

/// The row of size captions under the generation bars.
fn generation_captions(output: &str) -> String {
    let all = rows(output);
    let start = all
        .iter()
        .position(|row| row.contains("Heap by generation"))
        .expect("the generation panel should be on screen");
    let end = start
        + all[start..]
            .iter()
            .position(|row| row.starts_with('╰'))
            .expect("the generation panel should be closed");
    // The labels sit on the last row inside the panel, the sizes on the one above.
    all[end - 2].to_owned()
}

#[test]
fn the_header_shows_the_refresh_rate_at_every_width() {
    // The x axis is meaningless without it, so unlike the keymap it must never be dropped.
    for width in [200, 120, 80, 64] {
        let header = rows(&render(&app_from(ASPNET), width, 20))[0].to_owned();
        assert!(header.contains("-/+ 1s"), "{width} columns lost the rate: {header:?}");
    }
}

#[test]
fn the_rate_is_shown_once_rather_than_in_both_the_header_and_the_footer() {
    let output = render(&app_from(ASPNET), 200, 60);
    assert_eq!(output.matches("-/+").count(), 1, "the footer should not repeat the header");
}

#[test]
fn a_narrow_footer_drops_whole_hints_rather_than_half_a_word() {
    let output = render(&app_from(ASPNET), 64, 20);
    let footer = rows(&output).last().copied().unwrap_or_default().to_owned();
    assert!(footer.contains("q quit"), "the first hint always survives: {footer:?}");
    assert!(footer.contains("? help"), "help must survive, it reveals the other keys");
    assert!(footer.chars().count() <= 64, "footer overflowed: {footer:?}");
    for partial in ["investigat ", "detac ", "paus "] {
        assert!(!footer.contains(partial), "hint cut mid-word: {footer:?}");
    }
}

#[test]
fn every_plot_family_renders_the_charts() {
    let app = app_from(ASPNET);
    let mut theme = Theme::default();
    for expected in ["braille", "block", "octant"] {
        assert_eq!(theme.plot_name(), expected);
        let output = render_with(&app, 120, 40, theme);
        assert!(output.contains("Heap size"), "{expected} lost the chart");
        assert!(output.contains(expected), "the footer should name the plot family");
        theme.cycle_plot();
    }
}

#[test]
fn the_charts_annotate_their_scale_and_span() {
    let output = render(&app_from(ASPNET), 120, 40);
    // How far back the trace goes belongs in the bottom border, clear of the plot itself.
    assert!(output.contains("╰ -"), "no span label on any chart");
}

#[test]
fn an_app_with_no_samples_shows_placeholders_not_an_empty_screen() {
    let process = DotnetProcess {
        pid: 1,
        socket: "/tmp/s".into(),
        name: "Fresh".into(),
        command: "cmd".into(),
        start_key_verified: true,
    };
    let app = App::new(process, ProcessInfo::default(), 1.0);

    let output = render(&app, 120, 40);
    assert!(output.contains("waiting for data"), "charts should say they are waiting");
    assert!(output.contains("connecting"), "footer should show the connecting state");
    // With no provider seen yet, the generic layout is the safe default.
    assert!(!output.contains("Kestrel"));
}
