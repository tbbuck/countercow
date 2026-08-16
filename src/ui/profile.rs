//! The CPU profile screen: collect for a fixed window, then show the hot methods.
//!
//! Unlike every other screen this one is not live. Method names arrive only when the session
//! stops, so the screen has three states — collecting, resolving, done — and says which it is in.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, ProfilePhase};
use crate::counters::catalog::format_count;
use crate::profile::state::HotProfile;

use super::panels::titled_block;
use super::theme::Theme;

const MIN_WIDTH: u16 = 64;
const MIN_HEIGHT: u16 = 14;

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

    match &app.profile_phase {
        ProfilePhase::Collecting { .. } => render_collecting(frame, body_area, app, theme),
        ProfilePhase::Resolving => render_resolving(frame, body_area, theme),
        ProfilePhase::Failed(error) => render_error(frame, body_area, error, theme),
        ProfilePhase::Done => render_results(frame, body_area, app, theme),
    }

    render_footer(frame, footer_area, app, theme);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = Block::bordered().border_style(Style::default().fg(theme.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![
        Span::from("CPU profile ").fg(theme.dim),
        Span::from(app.info.display_name(&app.process.name).to_owned()).fg(theme.accent).bold(),
        Span::from("  pid ").fg(theme.dim),
        Span::from(app.process.pid.to_string()),
    ];

    if let Some(hot) = &app.profile {
        spans.push(Span::from("   ").fg(theme.dim));
        spans.push(Span::from(format!("{} samples", format_count(hot.working_samples as f64))));
        spans.push(Span::from("  ").fg(theme.dim));
        spans.push(Span::from(format!("{:.0}% parked", hot.waiting_percent())).fg(theme.dim));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut spans = vec![
        Span::from(" c / Esc").fg(theme.accent),
        Span::from(" back  ").fg(theme.dim),
    ];
    if matches!(app.profile_phase, ProfilePhase::Done | ProfilePhase::Failed(_)) {
        spans.push(Span::from("r").fg(theme.accent));
        spans.push(Span::from(" again  ").fg(theme.dim));
        spans.push(Span::from("w").fg(theme.accent));
        spans.push(Span::from(
            if app.profile_show_waiting { " hide parked  " } else { " show parked  " },
        ).fg(theme.dim));
    }
    spans.push(Span::from("q").fg(theme.accent));
    spans.push(Span::from(" quit").fg(theme.dim));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_collecting(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let block = titled_block("Collecting", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [gauge_area, text_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    let progress = app.profile_progress().clamp(0.0, 1.0);
    let remaining = app.profile_remaining_secs();
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(theme.accent))
        .ratio(progress)
        .label(Span::from(format!("{remaining:.0}s remaining")).fg(theme.fg));
    frame.render_widget(gauge, gauge_area);

    let samples = app.profile_samples;
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::from(format!("{} samples collected", format_count(samples as f64)))),
            Line::from(""),
            Line::from(
                Span::from("Method names arrive when the session stops, so the").fg(theme.dim),
            ),
            Line::from(Span::from("results appear at the end rather than live.").fg(theme.dim)),
        ]),
        text_area,
    );
}

fn render_resolving(frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = titled_block("Resolving", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from("Reading the method table…").fg(theme.dim)),
            Line::from(""),
            Line::from(
                Span::from("The runtime sends every loaded method when a session ends.").fg(theme.dim),
            ),
        ]),
        inner,
    );
}

fn render_error(frame: &mut Frame, area: Rect, error: &str, theme: &Theme) {
    let block = titled_block("Profile failed", theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::from(error.to_owned()).fg(theme.bad)),
            Line::from(""),
            Line::from(Span::from("Press r to try again, or Esc to go back.").fg(theme.dim)),
        ]),
        inner,
    );
}

fn render_results(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(hot) = &app.profile else {
        return;
    };

    let title = if app.profile_show_waiting {
        "Hot methods (including parked threads)"
    } else {
        "Hot methods"
    };
    let block = titled_block(title, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if hot.rows.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::from("No managed code was executing.").fg(theme.dim)),
                Line::from(""),
                Line::from(
                    Span::from("Every thread was parked in a wait for the whole window.")
                        .fg(theme.dim),
                ),
            ]),
            inner,
        );
        return;
    }

    render_table(frame, inner, hot, theme);
}

fn render_table(frame: &mut Frame, area: Rect, hot: &HotProfile, theme: &Theme) {
    let header = Row::new(vec![
        Line::from("SELF").right_aligned(),
        Line::from("TOTAL").right_aligned(),
        Line::from("METHOD"),
    ])
    .style(Style::default().fg(theme.title).bold());

    let rows: Vec<Row> = hot
        .rows
        .iter()
        .take(area.height.saturating_sub(1) as usize)
        .map(|method| {
            Row::new(vec![
                Line::from(
                    Span::from(format!("{:.1}%", method.self_percent))
                        .fg(heat(method.self_percent, theme)),
                )
                .right_aligned(),
                Line::from(Span::from(format!("{:.1}%", method.total_percent)).fg(theme.dim))
                    .right_aligned(),
                Line::from(Span::from(method.name.clone()).fg(theme.fg)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Length(7), Constraint::Length(7), Constraint::Fill(1)],
    )
    .header(header)
    .column_spacing(1);
    frame.render_widget(table, area);
}

/// Colour a method by how much of the process's time it accounts for.
fn heat(percent: f64, theme: &Theme) -> ratatui::style::Color {
    if percent >= 20.0 {
        theme.bad
    } else if percent >= 5.0 {
        theme.warn
    } else if percent >= 1.0 {
        theme.fg
    } else {
        theme.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heat_escalates_with_share_of_time() {
        let theme = Theme::default();
        assert_eq!(heat(45.0, &theme), theme.bad);
        assert_eq!(heat(8.0, &theme), theme.warn);
        assert_eq!(heat(2.0, &theme), theme.fg);
        assert_eq!(heat(0.1, &theme), theme.dim);
    }
}
