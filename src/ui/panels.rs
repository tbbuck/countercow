//! Reusable dashboard panels.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, Gauge, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::counters::catalog;

use super::theme::Theme;

pub fn titled_block<'a>(title: &str, theme: &Theme) -> Block<'a> {
    Block::bordered()
        .border_style(Style::default().fg(theme.border))
        .title(Span::from(format!(" {title} ")).fg(theme.title).bold())
}

/// A label/value list. Values are right-aligned so magnitudes line up and are scannable.
///
/// The value column is sized to its content rather than fixed, so labels keep as much room as
/// possible — a truncated "Allocation ra" is worse than a narrow number column.
pub fn stats(frame: &mut Frame, area: Rect, theme: &Theme, title: &str, rows: &[(&str, String)]) {
    let block = titled_block(title, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let value_width = rows
        .iter()
        .map(|(_, value)| value.chars().count())
        .max()
        .unwrap_or(6)
        .clamp(4, 14) as u16;

    let table_rows: Vec<Row> = rows
        .iter()
        .map(|(label, value)| {
            Row::new(vec![
                Line::from(Span::from(*label).fg(theme.dim)),
                Line::from(Span::from(value.clone()).fg(theme.fg)).right_aligned(),
            ])
        })
        .collect();

    let table = Table::new(table_rows, [Constraint::Fill(1), Constraint::Length(value_width)])
        .column_spacing(1);
    frame.render_widget(table, inner);
}

/// GC generation sizes side by side.
///
/// A bar chart rather than overlaid lines: five series in one braille chart would blend colours
/// where they cross, and the interesting comparison here is relative size at a glance anyway.
pub fn generation_bars(frame: &mut Frame, area: Rect, theme: &Theme, app: &App) {
    let block = titled_block("Heap by generation", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let labels = ["Gen 0", "Gen 1", "Gen 2", "LOH", "POH"];
    let bars: Vec<Bar> = catalog::GENERATION_COUNTERS
        .iter()
        .zip(labels)
        .zip(theme.generations)
        .map(|((counter, label), color)| {
            let bytes = app
                .latest(catalog::SYSTEM_RUNTIME, counter)
                .and_then(catalog::bytes_value)
                .unwrap_or(0.0);
            Bar::default()
                .value(bytes.max(0.0) as u64)
                .label(Line::from(label))
                .text_value(catalog::format_bytes(bytes))
                .style(Style::default().fg(color))
                .value_style(Style::default().fg(color).reversed())
        })
        .collect();

    // Share the width between five bars, leaving a column of gap between each.
    let bar_width = ((inner.width as usize).saturating_sub(4) / 5).clamp(3, 12) as u16;

    let chart = BarChart::default()
        .data(BarGroup::default().bars(&bars))
        .bar_width(bar_width)
        .bar_gap(1)
        .label_style(Style::default().fg(theme.dim));
    frame.render_widget(chart, inner);
}

/// A labelled percentage bar.
pub fn percent_gauge(frame: &mut Frame, area: Rect, theme: &Theme, title: &str, percent: Option<f64>) {
    let block = titled_block(title, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let value = percent.unwrap_or(0.0).clamp(0.0, 100.0);
    let label = match percent {
        Some(v) => format!("{v:.1} %"),
        None => "—".into(),
    };

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(theme.for_percent(value)))
        .ratio(value / 100.0)
        .label(Span::from(label).fg(theme.fg));
    frame.render_widget(gauge, Rect { height: 1, ..inner });
}

/// Rows for the GC activity panel.
pub fn gc_activity_rows(app: &App) -> Vec<(&'static str, String)> {
    let runtime = catalog::SYSTEM_RUNTIME;
    vec![
        ("Gen 0 GCs", app.display(runtime, "gen-0-gc-count")),
        ("Gen 1 GCs", app.display(runtime, "gen-1-gc-count")),
        ("Gen 2 GCs", app.display(runtime, "gen-2-gc-count")),
        ("Time in GC", app.display(runtime, "time-in-gc")),
        ("Pause time", app.display(runtime, "total-pause-time-by-gc")),
        ("Gen 0 budget", app.display(runtime, "gen-0-gc-budget")),
    ]
}

/// Rows for the memory summary panel.
pub fn memory_rows(app: &App) -> Vec<(&'static str, String)> {
    let runtime = catalog::SYSTEM_RUNTIME;
    vec![
        ("Working set", app.display(runtime, "working-set")),
        ("Heap size", app.display(runtime, "gc-heap-size")),
        ("Committed", app.display(runtime, "gc-committed")),
        ("Alloc rate", app.display(runtime, "alloc-rate")),
        ("Fragmented", app.display(runtime, "gc-fragmentation")),
    ]
}

/// Rows for the runtime panel.
pub fn runtime_rows(app: &App) -> Vec<(&'static str, String)> {
    let runtime = catalog::SYSTEM_RUNTIME;
    vec![
        ("TP threads", app.display(runtime, "threadpool-thread-count")),
        ("TP queue", app.display(runtime, "threadpool-queue-length")),
        ("TP completed", app.display(runtime, "threadpool-completed-items-count")),
        ("Lock waits", app.display(runtime, "monitor-lock-contention-count")),
        ("Exceptions", app.display(runtime, "exception-count")),
        ("Timers", app.display(runtime, "active-timer-count")),
    ]
}

/// Rows for the JIT/assembly panel, shown on the generic dashboard where there is room.
pub fn jit_rows(app: &App) -> Vec<(&'static str, String)> {
    let runtime = catalog::SYSTEM_RUNTIME;
    vec![
        ("Methods", app.display(runtime, "methods-jitted-count")),
        ("IL bytes", app.display(runtime, "il-bytes-jitted")),
        ("Time in JIT", app.display(runtime, "time-in-jit")),
        ("Assemblies", app.display(runtime, "assembly-count")),
    ]
}

/// Rows for the ASP.NET request panel.
pub fn request_rows(app: &App) -> Vec<(&'static str, String)> {
    vec![
        ("Rate", app.display(catalog::ASPNET_HOSTING, "requests-per-second")),
        ("Current", app.display(catalog::ASPNET_HOSTING, "current-requests")),
        ("Total", app.display(catalog::ASPNET_HOSTING, "total-requests")),
        ("Failed", app.display(catalog::ASPNET_HOSTING, "failed-requests")),
    ]
}

/// Rows for the Kestrel connection panel.
pub fn connection_rows(app: &App) -> Vec<(&'static str, String)> {
    vec![
        ("Open", app.display(catalog::KESTREL, "current-connections")),
        ("Total", app.display(catalog::KESTREL, "total-connections")),
        ("Conn queue", app.display(catalog::KESTREL, "connection-queue-length")),
        ("Req queue", app.display(catalog::KESTREL, "request-queue-length")),
        ("TLS", app.display(catalog::KESTREL, "total-tls-handshakes")),
        ("WebSockets", app.display(catalog::KESTREL, "current-upgraded-requests")),
    ]
}
