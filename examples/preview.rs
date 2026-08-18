//! Render the dashboard to a text buffer using captured fixture data.
//!
//! Lets the layout be inspected without attaching to a live process, and makes UI changes
//! reviewable in a diff.
//!
//! ```text
//! cargo run --example preview -- [aspnet|generic|loaded|console|investigate|profile] \
//!     [width] [height] [--colour|--html] [--repeat N] [--plot braille|block|octant]
//! ```
//!
//! Plain text by default so the output diffs. The graphs colour by height, which plain text cannot
//! show, so `--colour` emits ANSI for a terminal and `--html` a self-contained page — see
//! `scripts/preview-png.sh`, which screenshots the latter. `--repeat` replays the fixture end to
//! end N times, the only way to review a full graph: the captures are a few seconds long, and a
//! chart drawn from four samples says nothing about how one drawn from four hundred looks.

use countercow::app::App;
use countercow::counters::sample;
use countercow::ipc::commands::ProcessInfo;
use countercow::ipc::discovery::DotnetProcess;
use countercow::nettrace::blocks::NettraceParser;
use countercow::profile::run as profile_run;
use countercow::runtime::session as runtime_session;
use countercow::ui::theme::Theme;
use countercow::ui::{dashboard, investigate, profile};

use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

const ASPNET: &[u8] = include_bytes!("../tests/fixtures/aspnet-net9.nettrace");
const GENERIC: &[u8] = include_bytes!("../tests/fixtures/generic-net10.nettrace");
const LOADED: &[u8] = include_bytes!("../tests/fixtures/aspnet-net10-loaded.nettrace");
const CONSOLE: &[u8] = include_bytes!("../tests/fixtures/console-net8.nettrace");
const RUNTIME: &[u8] = include_bytes!("../tests/fixtures/runtime-net10-loaded.nettrace");
const PROFILE: &[u8] = include_bytes!("../tests/fixtures/profile-net10-cpu.nettrace");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let colour = args.iter().any(|a| a == "--colour" || a == "--color");
    let html = args.iter().any(|a| a == "--html");
    let repeat: usize = match args.iter().position(|a| a == "--repeat") {
        Some(index) => args.get(index + 1).ok_or("--repeat needs a count")?.parse()?,
        None => 1,
    };

    // A flag's value is not a positional: without skipping it, `--repeat 60` would be read as a
    // request for a 60-column terminal.
    const VALUED_FLAGS: [&str; 2] = ["--repeat", "--plot"];
    let mut positional = Vec::new();
    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        if VALUED_FLAGS.contains(&arg.as_str()) {
            remaining.next();
        } else if !arg.starts_with("--") {
            positional.push(arg);
        }
    }

    let which = positional.first().map_or("aspnet", |a| a.as_str()).to_owned();
    let width: u16 = positional.get(1).map_or(Ok(120), |a| a.parse())?;
    let height: u16 = positional.get(2).map_or(Ok(40), |a| a.parse())?;

    // The runtime and profile captures came from the same sample app as the loaded one, so they
    // are named for it: a screenshot captioned with the wrong process is a small lie.
    let (fixture, name, version) = match which.as_str() {
        "generic" => (GENERIC, "Rider.Backend", "10.0.1"),
        "loaded" => (LOADED, "CounterCowSampleApi", "10.0.1"),
        "console" => (CONSOLE, "CounterCowSampleConsole", "8.0.19"),
        "investigate" | "profile" => (LOADED, "CounterCowSampleApi", "10.0.1"),
        _ => (ASPNET, "CrimeRate.VectorTileApi", "9.0.7"),
    };

    let process = DotnetProcess {
        pid: 77686,
        socket: "/tmp/socket".into(),
        name: name.into(),
        command: name.into(),
        start_key_verified: true,
    };
    let info = ProcessInfo {
        os: "macOS".into(),
        arch: "arm64".into(),
        clr_version: Some(version.into()),
        ..Default::default()
    };

    let mut app = App::new(process, info, 1.0);
    let investigating = which == "investigate";
    let profiling = which == "profile";

    if profiling {
        // Replay a captured profile through the same ranking the live one uses.
        app.start_profile();
        app.finish_profile(profile_run::collect(std::io::Cursor::new(PROFILE), |_| {})?);
    } else if investigating {
        // Replay a captured runtime session through the same path the live one uses.
        app.toggle_investigate();
        runtime_session::run(std::io::Cursor::new(RUNTIME), |event, qpc| {
            app.record_runtime(event, qpc);
            std::ops::ControlFlow::Continue(())
        })?;
    } else {
        // Wound back far enough that the last reading lands about now.
        let tick = std::time::Duration::from_millis(25);
        let mut clock = std::time::Instant::now() - tick * 4000;
        for _ in 0..repeat {
            let mut parser = NettraceParser::new(std::io::Cursor::new(fixture))?;
            while let Some(batch) = parser.next_events()? {
                for event in batch {
                    let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                        continue;
                    };
                    if let Some(s) = sample::extract(metadata, &event)? {
                        // Stamped as though the capture were arriving live, so the charts can
                        // report a span rather than the microseconds the replay actually took.
                        app.record_at(s, clock);
                        clock += tick;
                    }
                }
            }
        }
    }

    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    let mut theme = Theme::default();
    if let Some(index) = args.iter().position(|a| a == "--plot") {
        let wanted = args.get(index + 1).ok_or("--plot needs braille, block or octant")?;
        while theme.plot_name() != wanted {
            theme.cycle_plot();
            if theme.plot_name() == "braille" && wanted != "braille" {
                return Err(format!("unknown plot family {wanted:?}").into());
            }
        }
    }
    let theme = theme;
    terminal.draw(|frame| {
        if profiling {
            profile::render(frame, &app, &theme);
        } else if investigating {
            investigate::render(frame, &app, &theme);
        } else {
            dashboard::render(frame, &app, &theme);
        }
    })?;

    let buffer = terminal.backend().buffer();
    let rows = buffer.area.height;
    let columns = buffer.area.width;

    if html {
        print!("{}", HTML_HEAD);
    }
    for y in 0..rows {
        let mut line = String::new();
        // Only start a new run where the colour actually changes, so the output stays readable.
        let mut current = None;
        for x in 0..columns {
            let cell = &buffer[(x, y)];
            let changed = current != Some(cell.fg);
            current = Some(cell.fg);

            if html {
                // One box per cell: the braille and block glyphs come from fallback fonts whose
                // advance width does not match the base face, which would skew the whole line.
                line.push_str(&format!(
                    "<i style=\"color:{}\">{}</i>",
                    css(cell.fg),
                    escape(cell.symbol())
                ));
            } else {
                if colour && changed {
                    line.push_str(&ansi(cell.fg));
                }
                line.push_str(cell.symbol());
            }
        }

        if html {
            println!("<div>{line}</div>");
        } else {
            if colour {
                line.push_str("\x1b[0m");
            }
            println!("{}", line.trim_end());
        }
    }
    if html {
        print!("{}", HTML_TAIL);
    }

    Ok(())
}

const HTML_HEAD: &str = r#"<!doctype html><meta charset="utf-8"><title>countercow preview</title>
<style>
  body { background:#12141a; margin:0; padding:20px; }
  main { font-family:Menlo,"DejaVu Sans Mono",monospace; font-size:16px; }
  div { height:19px; line-height:0; white-space:nowrap; }
  i { display:inline-block; vertical-align:top; width:9.6px; height:19px;
      line-height:19px; font-style:normal; }
</style>
<main>
"#;

const HTML_TAIL: &str = "</main>\n";

fn escape(symbol: &str) -> String {
    symbol.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A CSS colour for one cell. The sixteen ANSI names have no fixed values — the terminal theme
/// decides — so the page substitutes a plausible dark-theme palette for them.
fn css(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(index) => indexed_css(index),
        Color::Reset | Color::White => "#d8dee9".into(),
        Color::Black => "#000000".into(),
        Color::Red => "#cc0000".into(),
        Color::Green => "#4e9a06".into(),
        Color::Yellow => "#c4a000".into(),
        Color::Blue => "#3465a4".into(),
        Color::Magenta => "#75507b".into(),
        Color::Cyan => "#06989a".into(),
        Color::Gray => "#d3d7cf".into(),
        Color::DarkGray => "#555753".into(),
        Color::LightRed => "#ef2929".into(),
        Color::LightGreen => "#8ae234".into(),
        Color::LightYellow => "#fce94f".into(),
        Color::LightBlue => "#729fcf".into(),
        Color::LightMagenta => "#ad7fa8".into(),
        Color::LightCyan => "#34e2e2".into(),
    }
}

/// The xterm-256 palette: sixteen system colours, a 6x6x6 cube, then a 24-step grey ramp.
fn indexed_css(index: u8) -> String {
    const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => css(SYSTEM_COLORS[index as usize]),
        16..=231 => {
            let offset = index as usize - 16;
            let (r, g, b) = (offset / 36, offset / 6 % 6, offset % 6);
            format!("#{:02x}{:02x}{:02x}", CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b])
        }
        _ => {
            let level = 8 + (index as u32 - 232) * 10;
            format!("#{level:02x}{level:02x}{level:02x}")
        }
    }
}

const SYSTEM_COLORS: [Color; 16] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// A foreground escape for one cell's colour, covering only what the theme actually produces.
fn ansi(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Indexed(i) => format!("\x1b[38;5;{i}m"),
        Color::Reset => "\x1b[39m".into(),
        Color::Black => "\x1b[30m".into(),
        Color::Red => "\x1b[31m".into(),
        Color::Green => "\x1b[32m".into(),
        Color::Yellow => "\x1b[33m".into(),
        Color::Blue => "\x1b[34m".into(),
        Color::Magenta => "\x1b[35m".into(),
        Color::Cyan => "\x1b[36m".into(),
        Color::Gray => "\x1b[37m".into(),
        Color::DarkGray => "\x1b[90m".into(),
        Color::LightRed => "\x1b[91m".into(),
        Color::LightGreen => "\x1b[92m".into(),
        Color::LightYellow => "\x1b[93m".into(),
        Color::LightBlue => "\x1b[94m".into(),
        Color::LightMagenta => "\x1b[95m".into(),
        Color::LightCyan => "\x1b[96m".into(),
        Color::White => "\x1b[97m".into(),
    }
}
