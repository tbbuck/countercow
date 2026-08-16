//! Render the dashboard to a text buffer using captured fixture data.
//!
//! Lets the layout be inspected without attaching to a live process, and makes UI changes
//! reviewable in a diff.
//!
//! ```text
//! cargo run --example preview -- [aspnet|generic] [width] [height]
//! ```

use countercow::app::App;
use countercow::counters::sample;
use countercow::ipc::commands::ProcessInfo;
use countercow::ipc::discovery::DotnetProcess;
use countercow::nettrace::blocks::NettraceParser;
use countercow::ui::dashboard;
use countercow::ui::theme::Theme;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

const ASPNET: &[u8] = include_bytes!("../tests/fixtures/aspnet-net9.nettrace");
const GENERIC: &[u8] = include_bytes!("../tests/fixtures/generic-net10.nettrace");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "aspnet".into());
    let width: u16 = args.next().unwrap_or_else(|| "120".into()).parse()?;
    let height: u16 = args.next().unwrap_or_else(|| "40".into()).parse()?;

    let (fixture, name, version) = match which.as_str() {
        "generic" => (GENERIC, "Rider.Backend", "10.0.1"),
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
    let mut parser = NettraceParser::new(std::io::Cursor::new(fixture))?;
    while let Some(batch) = parser.next_events()? {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };
            if let Some(s) = sample::extract(metadata, &event)? {
                app.record(s);
            }
        }
    }

    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    let theme = Theme::default();
    terminal.draw(|frame| dashboard::render(frame, &app, &theme))?;

    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        println!("{}", line.trim_end());
    }

    Ok(())
}
