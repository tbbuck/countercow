//! The dashboard: a header, a body chosen by process type, and a footer.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::app::{format_uptime, App, Status};
use crate::counters::catalog;

use super::chart::{self, Scale, TimeSeries};
use super::panels;
use super::theme::Theme;

/// Below this the layout cannot show anything useful, so say so rather than render mush.
const MIN_WIDTH: u16 = 64;
const MIN_HEIGHT: u16 = 20;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let area = frame.area();

    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = Paragraph::new(format!(
            "Terminal too small\nneed {MIN_WIDTH}x{MIN_HEIGHT}, have {}x{}",
            area.width, area.height
        ))
        .style(Style::default().fg(theme.warn))
        .centered();
        frame.render_widget(message, area);
        return;
    }

    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header_area, app, theme);
    if app.is_aspnet() {
        render_aspnet_body(frame, body_area, app, theme);
    } else {
        render_generic_body(frame, body_area, app, theme);
    }
    render_footer(frame, footer_area, app, theme);

    if app.show_help {
        render_help(frame, area, theme);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // The rate lives in the top border rather than among the header's own detail, which is built
    // up only while it fits and so would drop it first on a narrow terminal. It is the one piece
    // of state you need to read the graphs at all — the x axis means nothing without it — so it
    // stays put at every size. The keys come with it, which is why the footer does not repeat it.
    let block = chart::bordered(theme).title_top(
        Line::from(vec![
            Span::from(" -/+ ").fg(theme.dim),
            Span::from(format_interval(app.interval)).fg(theme.accent).bold(),
            Span::from(" "),
        ])
        .right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Build up detail only while it fits, so a narrow terminal drops the least important parts
    // rather than truncating mid-word.
    let name = app.info.display_name(&app.process.name);
    let mut spans = vec![
        Span::from(name.to_owned()).fg(theme.accent).bold(),
        Span::from("  pid ").fg(theme.dim),
        Span::from(app.process.pid.to_string()),
    ];
    let mut used: usize = name.chars().count() + 6 + app.process.pid.to_string().len();

    let push_if_fits = |text: String, dim: bool, used: &mut usize, spans: &mut Vec<Span>| {
        let needed = text.chars().count() + 2;
        if *used + needed <= inner.width as usize {
            spans.push(Span::from("  ").fg(theme.dim));
            spans.push(if dim {
                Span::from(text).fg(theme.dim)
            } else {
                Span::from(text)
            });
            *used += needed;
        }
    };

    if let Some(framework) = app.info.framework_label() {
        push_if_fits(framework, false, &mut used, &mut spans);
    }
    let kind = if app.is_aspnet() { "ASP.NET Core" } else { ".NET" };
    push_if_fits(kind.to_owned(), true, &mut used, &mut spans);

    if !app.info.os.is_empty() || !app.info.arch.is_empty() {
        push_if_fits(
            format!("{} {}", app.info.os, app.info.arch).trim().to_owned(),
            true,
            &mut used,
            &mut spans,
        );
    }
    push_if_fits(format_uptime(app.uptime()), true, &mut used, &mut spans);

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let (status_text, status_color) = match &app.status {
        Status::Connecting => ("connecting…".to_string(), theme.warn),
        Status::Live if app.paused => ("paused".to_string(), theme.warn),
        Status::Live if app.is_stalled() => ("stalled".to_string(), theme.warn),
        Status::Live => (format!("{} counters", app.counter_count()), theme.good),
        Status::Ended => ("process exited".to_string(), theme.warn),
        Status::Failed(e) => (e.clone(), theme.bad),
    };

    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(24)]).areas(area);

    // Least useful last: a narrow terminal drops whole hints from the right rather than clipping
    // one in half. Help comes second despite reading oddly there, because it is the hint that
    // gets you to all the others — losing it first would hide the rest of the keys entirely.
    // The refresh rate is absent on purpose: the header carries it at every width.
    let hints = [
        ("q", "quit".to_owned()),
        ("?", "help".to_owned()),
        ("i", "investigate".to_owned()),
        ("c", "cpu".to_owned()),
        ("d", "detach".to_owned()),
        ("p", "pause".to_owned()),
        ("m", theme.plot_name().to_owned()),
    ];

    let mut spans = Vec::new();
    let mut used = 0usize;
    for (key, label) in &hints {
        let needed = key.chars().count() + label.chars().count() + 3;
        if used + needed > left.width as usize {
            break;
        }
        spans.push(Span::from(format!(" {key}")).fg(theme.accent));
        spans.push(Span::from(format!(" {label} ")).fg(theme.dim));
        used += needed;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), left);
    frame.render_widget(
        Paragraph::new(Line::from(Span::from(status_text).fg(status_color))).right_aligned(),
        right,
    );
}

/// Refresh rate as the footer shows it: a whole number of seconds loses its decimal point, and a
/// fraction keeps only the digits it needs.
fn format_interval(seconds: f64) -> String {
    if seconds.fract().abs() < f64::EPSILON {
        format!("{seconds:.0}s")
    } else {
        format!("{seconds}s")
    }
}

/// Height the generation bars need before they are worth showing at all.
const BARS_HEIGHT: u16 = 9;
/// Smallest chart that still conveys a trend.
const MIN_CHART_HEIGHT: u16 = 6;

/// GC and memory, which gets the most space on both layouts.
fn render_gc_section(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // The stats column needs a fixed width; the charts take what is left.
    let stats_width = 30.min(area.width / 3);

    // On a short terminal the bars would squeeze the chart out of existence entirely, so drop
    // them and keep the trend, which is the more useful of the two.
    if area.height < BARS_HEIGHT + MIN_CHART_HEIGHT {
        let [chart_area, memory_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(stats_width)]).areas(area);
        render_bytes_chart(frame, chart_area, app, theme, "Heap size", "gc-heap-size");
        panels::stats(frame, memory_area, theme, "Memory", &panels::memory_rows(app));
        return;
    }

    let [top, bottom] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(BARS_HEIGHT)]).areas(area);

    let [chart_area, memory_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(stats_width)]).areas(top);
    let [bars_area, activity_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(stats_width)]).areas(bottom);

    render_bytes_chart(frame, chart_area, app, theme, "Heap size", "gc-heap-size");
    panels::stats(frame, memory_area, theme, "Memory", &panels::memory_rows(app));
    panels::generation_bars(frame, bars_area, theme, app);
    panels::stats(frame, activity_area, theme, "GC activity", &panels::gc_activity_rows(app));
}

fn render_aspnet_body(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    // GC keeps the largest share; the runtime row gets just enough that its gauge survives on a
    // normal-sized terminal.
    let [gc, requests, runtime] = Layout::vertical([
        Constraint::Percentage(48),
        Constraint::Percentage(27),
        Constraint::Percentage(25),
    ])
    .areas(area);

    render_gc_section(frame, gc, app, theme);

    let [rps_area, request_area, connection_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(20),
        Constraint::Length(24),
    ])
    .areas(requests);

    let rps = app.history(catalog::ASPNET_HOSTING, "requests-per-second");
    if rps.is_empty() {
        chart::placeholder(frame, rps_area, "Requests/sec", theme);
    } else {
        TimeSeries {
            title: "Requests/sec",
            values: rps,
            gradient: theme.rate,
            scale: Scale::Auto,
            format: |v| format!("{v:.0}/s"),
            interval: app.interval,
        }
        .render(frame, rps_area, theme);
    }
    panels::stats(frame, request_area, theme, "Requests", &panels::request_rows(app));
    panels::stats(frame, connection_area, theme, "Kestrel", &panels::connection_rows(app));

    render_runtime_section(frame, runtime, app, theme, false);
}

fn render_generic_body(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let [gc, runtime] =
        Layout::vertical([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(area);

    render_gc_section(frame, gc, app, theme);
    render_runtime_section(frame, runtime, app, theme, true);
}

/// CPU, threadpool and (where there is room) JIT.
fn render_runtime_section(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    with_jit: bool,
) {
    let mut constraints = vec![Constraint::Fill(1), Constraint::Length(26)];
    if with_jit {
        constraints.push(Constraint::Length(24));
    }
    let chunks = Layout::horizontal(constraints).split(area);

    // The gauge is a nice-to-have; give it room only once the chart has enough of its own.
    let show_gauge = area.height >= MIN_CHART_HEIGHT + 3;
    let (cpu_chart_area, gauge_area) = if show_gauge {
        let [chart_area, gauge_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(chunks[0]);
        (chart_area, Some(gauge_area))
    } else {
        (chunks[0], None)
    };

    let cpu = app.history(catalog::SYSTEM_RUNTIME, "cpu-usage");
    if cpu.is_empty() {
        chart::placeholder(frame, cpu_chart_area, "CPU usage", theme);
    } else {
        TimeSeries {
            title: "CPU usage",
            values: cpu,
            gradient: theme.cpu,
            // The one counter with a real ceiling, so hold it: a process ticking over at 3% must
            // not rescale into a chart that looks like it is on fire.
            scale: Scale::Fixed(100.0),
            format: |v| format!("{v:.1}%"),
            interval: app.interval,
        }
        .render(frame, cpu_chart_area, theme);
    }

    if let Some(gauge_area) = gauge_area {
        let fragmentation = app
            .latest(catalog::SYSTEM_RUNTIME, "gc-fragmentation")
            .and_then(catalog::percentage);
        panels::percent_gauge(frame, gauge_area, theme, "GC fragmentation", fragmentation);
    }

    panels::stats(frame, chunks[1], theme, "Runtime", &panels::runtime_rows(app));
    if with_jit {
        panels::stats(frame, chunks[2], theme, "JIT", &panels::jit_rows(app));
    }
}

/// Chart a memory counter, scaling its history to bytes so the axis reads correctly whether the
/// runtime reported megabytes or raw bytes.
fn render_bytes_chart(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    title: &str,
    counter: &str,
) {
    let history = app.history(catalog::SYSTEM_RUNTIME, counter);
    if history.is_empty() {
        chart::placeholder(frame, area, title, theme);
        return;
    }

    let scale = app
        .latest(catalog::SYSTEM_RUNTIME, counter)
        .and_then(catalog::byte_scale)
        .unwrap_or(1.0);
    let scaled: Vec<f64> = history.iter().map(|v| v * scale).collect();

    TimeSeries {
        title,
        values: &scaled,
        gradient: theme.heap,
        scale: Scale::Auto,
        format: catalog::format_bytes,
        interval: app.interval,
    }
    .render(frame, area, theme);
}

fn render_help(frame: &mut Frame, area: Rect, theme: &Theme) {
    let lines = vec![
        Line::from(Span::from("countercow").fg(theme.accent).bold()),
        Line::from(""),
        Line::from("  q / Esc   quit"),
        Line::from("  d         detach and pick another process"),
        Line::from("  i         investigate: allocations, GC causes, exceptions"),
        Line::from("  c         cpu profile: which methods are burning time"),
        Line::from("  p         pause history (session keeps running)"),
        Line::from("  - / +     refresh faster / slower"),
        Line::from("  m         cycle braille / block / octant plotting"),
        Line::from("  ?         toggle this help"),
        Line::from(""),
        Line::from(Span::from("  Panels appear once their provider reports.").fg(theme.dim)),
        Line::from(Span::from("  ASP.NET panels are hidden for other processes.").fg(theme.dim)),
        Line::from(""),
        Line::from(
            Span::from("  Changing the refresh rate reopens the counter session,").fg(theme.dim),
        ),
        Line::from(Span::from("  so the graphs restart from empty.").fg(theme.dim)),
        Line::from(""),
        Line::from(
            Span::from("  Investigating costs the target process CPU, so it runs").fg(theme.dim),
        ),
        Line::from(Span::from("  only while that screen is open.").fg(theme.dim)),
    ];

    let width = 52.min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = chart::bordered(theme)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::from(" Help ").fg(theme.title).bold());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
}
