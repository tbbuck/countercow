//! Time-series plotting.
//!
//! One chart, one series. Two braille series in one plot would share cells and blend into a third
//! colour where they cross; and with the gradient fill these use, height already carries meaning,
//! so a second series would fight the first for it. Side-by-side comparison is the bar chart's job.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;

use crate::app::Reading;

use super::gradient::Gradient;
use super::graph::Graph;
use super::panels::titled_block;
use super::theme::Theme;

/// How a chart decides its full-scale value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scale {
    /// Fit the window's own peak. The gradient then reads as height relative to what has been
    /// seen recently, not as an absolute severity — there is no meaningful ceiling for a heap
    /// size or a request rate.
    Auto,
    /// A fixed ceiling, so the same colour means the same thing every frame. Only worth using
    /// where the counter has a real maximum. Still grows if a reading somehow exceeds it, because
    /// silently clipping the line would be worse than rescaling.
    Fixed(f64),
}

/// A single-series filled chart over recent history.
pub struct TimeSeries<'a> {
    pub title: &'a str,
    /// Oldest first. Rendered left to right, newest at the right edge.
    pub values: &'a [Reading],
    pub gradient: Gradient,
    pub scale: Scale,
    /// Formats the scale label and the current-value annotation.
    pub format: fn(f64) -> String,
}

impl TimeSeries<'_> {
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let latest = self.values.last().map_or(0.0, |reading| reading.value);
        let max = self.full_scale();
        let inner_width = area.width.saturating_sub(2);

        // The current value takes its colour from where it sits on the scale, so the number and
        // the top of the trace agree with each other.
        let heat = theme.on(&self.gradient, if max > 0.0 { latest / max } else { 0.0 });
        let current = Span::from(format!(" {} ", (self.format)(latest))).fg(heat).bold();
        let mut block =
            titled_block(self.title, theme).title_top(Line::from(current).right_aligned());

        // How far back the trace reaches goes in the bottom border rather than over the plot: at
        // the bottom left the graph is at its densest, and a label there covers real readings.
        if let Some(span) = self.span_label(theme, inner_width) {
            block = block.title_bottom(Span::from(format!(" {span} ")).fg(theme.dim));
        }

        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let values: Vec<f64> = self.values.iter().map(|reading| reading.value).collect();
        Graph {
            values: &values,
            max,
            gradient: self.gradient,
            plot: theme.plot,
            depth: theme.depth,
        }
        .render(frame.buffer_mut(), inner);

        // The scale does sit over the plot, as btop's does. There is no room for a gutter wide
        // enough for "118.5 MiB" on every chart, and the top left corner it covers is where the
        // trace least often reaches.
        let ceiling = (self.format)(max);
        if ceiling.chars().count() < inner.width as usize {
            frame.buffer_mut().set_string(
                inner.x,
                inner.y,
                &ceiling,
                Style::default().fg(theme.dim),
            );
        }
    }

    /// How far back the drawn trace goes, once it goes back at all.
    ///
    /// Measured from the readings themselves rather than from the sample count and the current
    /// rate. The rate can change while a series is on screen, which would make the arithmetic
    /// answer confidently wrong about every sample gathered before the change.
    fn span_label(&self, theme: &Theme, inner_width: u16) -> Option<String> {
        let drawn = self.values.len().min(Graph::capacity(theme.plot, inner_width));
        if drawn < 2 {
            return None;
        }
        let window = &self.values[self.values.len() - drawn..];
        let span = window.last()?.at.saturating_duration_since(window.first()?.at);
        Some(format!("-{:.0}s", span.as_secs_f64()))
    }

    /// Full-scale value for the plot.
    fn full_scale(&self) -> f64 {
        let peak = self
            .values
            .iter()
            .map(|reading| reading.value)
            .filter(|value| value.is_finite())
            .fold(0.0, f64::max);
        match self.scale {
            // A little headroom, so the peak sits just below the top rather than jammed into it.
            Scale::Auto if peak > 0.0 => peak * 1.08,
            // A flat-zero series still needs a sane axis rather than a degenerate one.
            Scale::Auto => 1.0,
            Scale::Fixed(ceiling) => ceiling.max(peak),
        }
    }
}

/// Render a chart area that has no data yet, so the layout does not jump once data arrives.
pub fn placeholder(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) {
    let block = titled_block(title, theme);
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

/// Blocks that are not charts still want the same frame, so it lives with the chart code's
/// neighbours rather than being duplicated per screen.
pub fn bordered(theme: &Theme) -> Block<'static> {
    Block::bordered()
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::default().fg(theme.border))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Readings a second apart, oldest first.
    fn readings(values: &[f64]) -> Vec<Reading> {
        let start = Instant::now();
        values
            .iter()
            .enumerate()
            .map(|(index, &value)| Reading { at: start + Duration::from_secs(index as u64), value })
            .collect()
    }

    fn series(values: &[Reading], scale: Scale) -> TimeSeries<'_> {
        TimeSeries {
            title: "Test",
            values,
            gradient: crate::ui::gradient::FLAME,
            scale,
            format: |v| format!("{v:.0}"),
        }
    }

    #[test]
    fn an_auto_scale_leaves_headroom_above_the_peak() {
        let scale = series(&readings(&[10.0, 40.0, 25.0]), Scale::Auto).full_scale();
        assert!(scale > 40.0 && scale < 45.0, "{scale}");
    }

    #[test]
    fn a_flat_zero_series_gets_a_usable_scale_rather_than_zero() {
        assert_eq!(series(&readings(&[0.0, 0.0]), Scale::Auto).full_scale(), 1.0);
        assert_eq!(series(&readings(&[]), Scale::Auto).full_scale(), 1.0);
    }

    #[test]
    fn a_fixed_scale_is_held_regardless_of_the_peak() {
        // 3% CPU on a 0-100 scale must not rescale to look like a spike.
        let quiet = readings(&[3.0, 2.0, 3.5]);
        assert_eq!(series(&quiet, Scale::Fixed(100.0)).full_scale(), 100.0);
    }

    #[test]
    fn a_fixed_scale_still_grows_rather_than_clipping_the_trace() {
        let over = readings(&[10.0, 140.0]);
        assert_eq!(series(&over, Scale::Fixed(100.0)).full_scale(), 140.0);
    }

    #[test]
    fn non_finite_readings_do_not_poison_the_scale() {
        let scale = series(&readings(&[10.0, f64::NAN, 20.0]), Scale::Auto).full_scale();
        assert!(scale.is_finite() && scale > 20.0, "{scale}");
    }

    #[test]
    fn the_span_is_measured_from_the_readings_not_the_sample_count() {
        // Ten readings a second apart span nine seconds, whatever rate is in force now.
        let theme = Theme::default();
        let ten = readings(&[1.0; 10]);
        assert_eq!(series(&ten, Scale::Auto).span_label(&theme, 40), Some("-9s".into()));
    }

    #[test]
    fn a_span_covers_only_the_readings_that_fit_on_screen() {
        // Sixty readings into a graph four cells wide: eight fit, so seven seconds are drawn.
        let theme = Theme::default();
        let many = readings(&[1.0; 60]);
        assert_eq!(series(&many, Scale::Auto).span_label(&theme, 4), Some("-7s".into()));
    }

    #[test]
    fn a_single_reading_has_no_span_to_report() {
        let theme = Theme::default();
        assert_eq!(series(&readings(&[1.0]), Scale::Auto).span_label(&theme, 40), None);
    }
}
