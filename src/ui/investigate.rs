//! The investigation screen: what is allocating, why collections are happening, what is throwing,
//! and what is blocking.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{format_uptime, App};
use crate::counters::catalog::{format_bytes, format_count};
use crate::runtime::state::{Collection, RuntimeState};

use super::chart::bordered;
use super::panels::titled_block;
use super::theme::Theme;

const MIN_WIDTH: u16 = 64;
const MIN_HEIGHT: u16 = 16;

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

    if let Some(error) = &app.runtime_error {
        render_error(frame, body_area, error, theme);
    } else if app.runtime.is_empty() {
        render_waiting(frame, body_area, theme);
    } else {
        render_body(frame, body_area, app, theme);
    }

    render_footer(frame, footer_area, app, theme);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = bordered(theme).border_style(Style::default().fg(theme.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let name = app.info.display_name(&app.process.name);
    let state = &app.runtime;

    let mut spans = vec![
        Span::from("Investigating ").fg(theme.dim),
        Span::from(name.to_owned()).fg(theme.accent).bold(),
        Span::from("  pid ").fg(theme.dim),
        Span::from(app.process.pid.to_string()),
    ];

    if !state.is_empty() {
        spans.push(Span::from("   ").fg(theme.dim));
        spans.push(Span::from(format!(
            "{} allocated",
            format_bytes(state.total_allocated_bytes as f64)
        )));
        spans.push(Span::from("  ").fg(theme.dim));
        spans.push(Span::from(format!("{} GCs", state.total_gcs())));
        if state.total_pause_ms > 0.0 {
            spans.push(Span::from("  ").fg(theme.dim));
            spans.push(Span::from(format!("{:.1} ms paused", state.total_pause_ms)));
        }
    }

    spans.push(Span::from("   sampling ").fg(theme.dim));
    spans.push(Span::from(format_uptime(app.investigating_for())).fg(theme.dim));

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let line = Line::from(vec![
        Span::from(" i / Esc").fg(theme.accent),
        Span::from(" back  ").fg(theme.dim),
        Span::from("r").fg(theme.accent),
        Span::from(" reset  ").fg(theme.dim),
        Span::from("q").fg(theme.accent),
        Span::from(" quit").fg(theme.dim),
    ]);

    // Say plainly that this costs the target something, because unlike counters it does.
    let cost = Span::from(format!("{} runtime events ", format_count(app.runtime.events_seen as f64)))
        .fg(theme.dim);

    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(26)]).areas(area);
    frame.render_widget(Paragraph::new(line), left);
    frame.render_widget(Paragraph::new(Line::from(cost)).right_aligned(), right);
}

fn render_error(frame: &mut Frame, area: Rect, error: &str, theme: &Theme) {
    let block = titled_block("Investigation unavailable", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from(error.to_owned()).fg(theme.bad)),
            Line::from(""),
            Line::from(
                Span::from("Press i or Esc to return to the dashboard.").fg(theme.dim),
            ),
        ]),
        inner,
    );
}

fn render_waiting(frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = titled_block("Investigating", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from("Listening for runtime events…").fg(theme.dim)),
            Line::from(""),
            Line::from(
                Span::from("Allocation samples appear as the application allocates;").fg(theme.dim),
            ),
            Line::from(
                Span::from("an idle process may report nothing at all.").fg(theme.dim),
            ),
        ]),
        inner,
    );
}

fn render_body(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(area);

    let collections_width = 34.min(area.width / 3);
    let [alloc_area, gc_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(collections_width)]).areas(top);
    let [exception_area, contention_area] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(collections_width)])
            .areas(bottom);

    render_allocations(frame, alloc_area, &app.runtime, theme);
    render_collections(frame, gc_area, &app.runtime, theme);
    render_exceptions(frame, exception_area, &app.runtime, theme);
    render_contention(frame, contention_area, &app.runtime, theme);
}

fn render_allocations(frame: &mut Frame, area: Rect, state: &RuntimeState, theme: &Theme) {
    let block = titled_block("Allocations by type", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sites = state.top_allocations(inner.height as usize);
    if sites.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from("no allocation samples yet").fg(theme.dim)),
            inner,
        );
        return;
    }

    let rows: Vec<Row> = sites
        .iter()
        .map(|site| {
            let share = state.allocation_share(site);
            Row::new(vec![
                Line::from(Span::from(site.type_name.clone()).fg(theme.fg)),
                Line::from(Span::from(site.kind.label()).fg(theme.dim)),
                Line::from(Span::from(format_bytes(site.bytes as f64))).right_aligned(),
                Line::from(Span::from(format!("{share:.1}%")).fg(share_colour(share, theme)))
                    .right_aligned(),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Length(6),
        ],
    )
    .column_spacing(1);
    frame.render_widget(table, inner);
}

/// A type dominating allocation is the thing to look at, so make it stand out.
fn share_colour(share: f64, theme: &Theme) -> ratatui::style::Color {
    if share >= 50.0 {
        theme.warn
    } else if share >= 20.0 {
        theme.accent
    } else {
        theme.dim
    }
}

fn render_collections(frame: &mut Frame, area: Rect, state: &RuntimeState, theme: &Theme) {
    let block = titled_block("Collections", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let collections = state.recent_collections(inner.height as usize);
    if collections.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from("no collections yet").fg(theme.dim)),
            inner,
        );
        return;
    }

    let rows: Vec<Row> = collections.iter().map(|gc| collection_row(gc, theme)).collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(8),
        ],
    )
    .column_spacing(1);
    frame.render_widget(table, inner);
}

fn collection_row<'a>(gc: &Collection, theme: &Theme) -> Row<'a> {
    // Generation is the cost signal: gen 2 collections are the expensive ones.
    let generation_colour = match gc.generation {
        0 => theme.good,
        1 => theme.accent,
        _ => theme.warn,
    };
    let pause = match gc.pause_ms {
        Some(ms) => format!("{ms:.1} ms"),
        None => "—".into(),
    };
    let reason_colour = if gc.reason.is_pressure() { theme.bad } else { theme.dim };

    Row::new(vec![
        Line::from(Span::from(format!("#{}", gc.count)).fg(theme.dim)),
        Line::from(Span::from(format!("gen{}", gc.generation)).fg(generation_colour)),
        Line::from(Span::from(gc.reason.label()).fg(reason_colour)),
        Line::from(Span::from(pause)).right_aligned(),
    ])
}

fn render_exceptions(frame: &mut Frame, area: Rect, state: &RuntimeState, theme: &Theme) {
    let block = titled_block("Exceptions thrown", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sites = state.top_exceptions(inner.height as usize);
    if sites.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::from("none thrown").fg(theme.dim)),
            inner,
        );
        return;
    }

    let rows: Vec<Row> = sites
        .iter()
        .map(|site| {
            Row::new(vec![
                Line::from(Span::from(site.type_name.clone()).fg(theme.fg)),
                Line::from(Span::from(site.message.clone()).fg(theme.dim)),
                Line::from(Span::from(format_count(site.count as f64)).fg(theme.warn))
                    .right_aligned(),
            ])
        })
        .collect();

    // The type name identifies the problem; the message is supporting detail, so it yields first.
    let table = Table::new(
        rows,
        [Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(8)],
    )
    .column_spacing(1);
    frame.render_widget(table, inner);
}

fn render_contention(frame: &mut Frame, area: Rect, state: &RuntimeState, theme: &Theme) {
    let block = titled_block("Lock contention", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let contention = &state.contention;
    if contention.events == 0 {
        frame.render_widget(
            Paragraph::new(Span::from("no contention").fg(theme.good)),
            inner,
        );
        return;
    }

    let rows = vec![
        ("Waits", format_count(contention.events as f64)),
        ("Total", format_duration_ns(contention.total_ns)),
        ("Mean", format_duration_ns(contention.mean_ns())),
        ("Worst", format_duration_ns(contention.max_ns)),
    ];

    let table_rows: Vec<Row> = rows
        .into_iter()
        .map(|(label, value)| {
            Row::new(vec![
                Line::from(Span::from(label).fg(theme.dim)),
                Line::from(Span::from(value).fg(theme.fg)).right_aligned(),
            ])
        })
        .collect();

    let table = Table::new(table_rows, [Constraint::Fill(1), Constraint::Length(12)])
        .column_spacing(1);
    frame.render_widget(table, inner);
}

/// Nanoseconds at a readable scale.
pub fn format_duration_ns(ns: f64) -> String {
    if ns < 1_000.0 {
        format!("{ns:.0} ns")
    } else if ns < 1_000_000.0 {
        format!("{:.1} µs", ns / 1_000.0)
    } else if ns < 1_000_000_000.0 {
        format!("{:.1} ms", ns / 1_000_000.0)
    } else {
        format!("{:.2} s", ns / 1_000_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_scale_to_readable_units() {
        assert_eq!(format_duration_ns(500.0), "500 ns");
        assert_eq!(format_duration_ns(1_500.0), "1.5 µs");
        assert_eq!(format_duration_ns(2_500_000.0), "2.5 ms");
        assert_eq!(format_duration_ns(3_000_000_000.0), "3.00 s");
    }

    #[test]
    fn a_dominant_type_is_highlighted() {
        let theme = Theme::default();
        assert_eq!(share_colour(96.0, &theme), theme.warn);
        assert_eq!(share_colour(25.0, &theme), theme.accent);
        assert_eq!(share_colour(1.0, &theme), theme.dim);
    }
}
