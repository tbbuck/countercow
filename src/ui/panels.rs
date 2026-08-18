//! Reusable dashboard panels.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::counters::catalog;

use super::graph;
use super::theme::Theme;

pub fn titled_block<'a>(title: &str, theme: &Theme) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
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

/// Rows the generation bars give up to the value and label beneath each bar.
const BAR_CAPTION_ROWS: u16 = 2;

/// Slot width at which each caption can carry its share of the heap as well as its size.
const SHARE_FITS_AT: u16 = 13;

/// GC generation sizes side by side.
///
/// Bars rather than overlaid lines: five series in one chart would share cells and blend, and the
/// interesting comparison here is relative size at a glance anyway. The bars divide the full width
/// between them, so the panel is as wide as whatever it has been given rather than leaving a gap.
pub fn generation_bars(frame: &mut Frame, area: Rect, theme: &Theme, app: &App) {
    let block = titled_block("Heap by generation", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height <= BAR_CAPTION_ROWS || inner.width < catalog::GENERATION_COUNTERS.len() as u16 {
        return;
    }

    let labels = ["Gen 0", "Gen 1", "Gen 2", "LOH", "POH"];
    let sizes: Vec<f64> = catalog::GENERATION_COUNTERS
        .iter()
        .map(|counter| {
            app.latest(catalog::SYSTEM_RUNTIME, counter)
                .and_then(catalog::bytes_value)
                .unwrap_or(0.0)
                .max(0.0)
        })
        .collect();

    let largest = sizes.iter().copied().fold(0.0, f64::max);
    let total: f64 = sizes.iter().sum();
    let slots = sizes.len() as u16;

    // Decided once for the panel, from the narrowest slot. Deciding per slot would let a width
    // that does not divide evenly give the share to whichever slot got the spare column, which
    // reads as a glitch rather than as a layout.
    let show_share = total > 0.0 && inner.width / slots >= SHARE_FITS_AT;

    let bar_area = Rect { height: inner.height - BAR_CAPTION_ROWS, ..inner };
    let value_y = inner.y + inner.height - 2;
    let label_y = inner.y + inner.height - 1;

    for (index, (&bytes, label)) in sizes.iter().zip(labels).enumerate() {
        // Divide by position rather than by a fixed width, so the leftover columns of an
        // indivisible width are spread across the slots instead of left blank at the right edge.
        let start = inner.width * index as u16 / slots;
        let end = inner.width * (index as u16 + 1) / slots;
        let slot = Rect { x: inner.x + start, width: end - start, ..inner };

        // A column of breathing room each side, once there is width to spare for it.
        let padding = u16::from(slot.width >= 5);
        let column = Rect {
            x: slot.x + padding,
            width: slot.width - padding * 2,
            y: bar_area.y,
            height: bar_area.height,
        };
        if largest > 0.0 {
            let gradient = theme.generation_gradient(index);
            graph::vertical_bar(
                frame.buffer_mut(),
                column,
                bytes / largest,
                &gradient,
                theme.depth,
                theme.dim,
            );
        }

        let mut caption = catalog::format_bytes(bytes);
        if show_share {
            caption = format!("{caption} {:.0}%", bytes / total * 100.0);
        }
        let colour = theme.on(&theme.generation_gradient(index), 0.5);
        centred(frame, slot, value_y, &caption, Style::default().fg(colour));
        centred(frame, slot, label_y, label, Style::default().fg(theme.dim));
    }
}

/// Write `text` centred in `slot` on row `y`, or nothing if it does not fit.
fn centred(frame: &mut Frame, slot: Rect, y: u16, text: &str, style: Style) {
    let width = text.chars().count() as u16;
    if width > slot.width {
        return;
    }
    frame.buffer_mut().set_string(slot.x + (slot.width - width) / 2, y, text, style);
}

/// A labelled percentage meter, filled along the ramp so the colour says how full it is.
pub fn percent_gauge(frame: &mut Frame, area: Rect, theme: &Theme, title: &str, percent: Option<f64>) {
    let block = titled_block(title, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let label = match percent {
        Some(value) => format!("{value:.1} %"),
        None => "—".into(),
    };
    // A leading space keeps the number off the meter's last block.
    let label_width = (label.chars().count() as u16 + 1).min(inner.width);
    let meter_width = inner.width - label_width;

    graph::meter(
        frame.buffer_mut(),
        Rect { width: meter_width, height: 1, ..inner },
        percent.unwrap_or(0.0) / 100.0,
        &theme.cpu,
        theme.depth,
        theme.dim,
    );

    // Bounded rather than written straight out: a counter reporting something absurd would
    // otherwise run the label past this panel and into whatever is drawn to the right of it.
    let colour = theme.for_percent(percent.unwrap_or(0.0));
    frame.buffer_mut().set_stringn(
        inner.x + meter_width + 1,
        inner.y,
        &label,
        label_width.saturating_sub(1) as usize,
        Style::default().fg(colour),
    );
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
