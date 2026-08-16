//! Time-series plotting.
//!
//! Every chart here draws a **single** series. That is deliberate: ratatui's braille grid ORs
//! overlapping cells together, so two coloured lines crossing produce a blended third colour.
//! `bottom` vendors a patched canvas to fix this; drawing one series per chart and using a bar
//! chart for side-by-side comparison avoids the problem outright.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType};
use ratatui::Frame;

use super::theme::Theme;

/// A single-series line chart over recent history.
pub struct TimeSeries<'a> {
    pub title: &'a str,
    /// Oldest first. Rendered left to right.
    pub values: &'a [f64],
    pub color: ratatui::style::Color,
    pub marker: Marker,
    /// Formats axis labels and the current-value annotation.
    pub format: fn(f64) -> String,
    /// Seconds between samples, for the time axis label.
    pub interval: f64,
}

impl TimeSeries<'_> {
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let points: Vec<(f64, f64)> = self
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| (i as f64, *v))
            .collect();

        let max = self
            .values
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        // A flat-zero series still needs a sane axis rather than a degenerate one.
        let upper = if max.is_finite() && max > 0.0 { max * 1.15 } else { 1.0 };

        let latest = self.values.last().copied().unwrap_or(0.0);
        let title = format!(" {} — {} ", self.title, (self.format)(latest));

        // Two points minimum, else the x bounds collapse.
        let x_max = (self.values.len().saturating_sub(1)).max(1) as f64;
        let span_secs = x_max * self.interval;

        let dataset = Dataset::default()
            .marker(self.marker)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(self.color))
            .data(&points);

        let chart = Chart::new(vec![dataset])
            .block(
                Block::bordered()
                    .border_style(Style::default().fg(theme.border))
                    .title(Span::from(title).fg(theme.title).bold()),
            )
            .x_axis(
                Axis::default()
                    .style(Style::default().fg(theme.dim))
                    .bounds([0.0, x_max])
                    .labels(vec![
                        Span::from(format!("-{span_secs:.0}s")),
                        Span::from("now"),
                    ]),
            )
            .y_axis(
                Axis::default()
                    .style(Style::default().fg(theme.dim))
                    .bounds([0.0, upper])
                    .labels(y_labels(upper, self.format)),
            )
            .legend_position(None);

        frame.render_widget(chart, area);
    }
}

/// Axis labels from zero to `upper`, dropping any that format identically.
///
/// Without this, a counter sitting near zero renders an axis reading "0/s, 0/s, 1/s" — three
/// labels implying a scale the numbers do not support.
fn y_labels(upper: f64, format: fn(f64) -> String) -> Vec<Span<'static>> {
    let mut labels: Vec<String> = [0.0, upper / 2.0, upper].iter().map(|v| format(*v)).collect();
    labels.dedup();
    if labels.len() == 1 {
        labels.push(format(upper));
        labels.dedup();
    }
    labels.into_iter().map(Span::from).collect()
}

/// Render a chart area that has no data yet, so the layout does not jump once data arrives.
pub fn placeholder(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) {
    let block = Block::bordered()
        .border_style(Style::default().fg(theme.border))
        .title(Span::from(format!(" {title} ")).fg(theme.title).bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let waiting = ratatui::widgets::Paragraph::new("waiting for data…")
        .style(Style::default().fg(theme.dim))
        .alignment(Alignment::Center);
    // Vertically centre the message in the block.
    let y = inner.y + inner.height / 2;
    if inner.height > 0 {
        frame.render_widget(waiting, Rect { y, height: 1, ..inner });
    }
}
