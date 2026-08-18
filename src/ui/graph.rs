//! btop-style filled graphs and bars.
//!
//! These paint straight into the buffer rather than going through ratatui's `Chart`, for two
//! reasons. A line chart interpolates between samples, which invents readings the counter never
//! reported; and a `Dataset` carries one colour for the whole series, where the point of these
//! graphs is that the colour changes with height — quiet traces stay cool, spikes burn through the
//! ramp into red. One sample per sub-column, filled from the baseline, is both honest about what
//! arrived and the thing that makes the plot look like btop.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::{braille, pixel};

use super::gradient::{Depth, Gradient};

/// Vertical eighths, indexed by how many of a cell's eight sub-rows are filled.
const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The unfilled remainder of a meter.
const METER_EMPTY: char = '░';

/// The glyph family a graph plots with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Plot {
    /// Braille dots: two samples per cell at 2x4 sub-cell resolution. The densest option, and the
    /// one with by far the widest font support, so it is the default.
    #[default]
    Braille,
    /// Octants: the same 2x4 resolution packed solid, with no gaps between cells. Better looking
    /// where the font has the glyphs, which is a much shorter list — Unicode 16 added these.
    Octant,
    /// Solid eighth-blocks: one chunky bar per sample. Coarser, but it renders anywhere and reads
    /// clearly on a small terminal.
    Block,
}

impl Plot {
    /// Samples drawn per terminal column.
    pub fn samples_per_cell(self) -> usize {
        match self {
            Plot::Block => 1,
            Plot::Braille | Plot::Octant => 2,
        }
    }

    /// Vertical resolution within one cell.
    fn rows_per_cell(self) -> usize {
        match self {
            Plot::Block => 8,
            Plot::Braille | Plot::Octant => 4,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Plot::Braille => Plot::Block,
            Plot::Block => Plot::Octant,
            Plot::Octant => Plot::Braille,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Plot::Braille => "braille",
            Plot::Octant => "octant",
            Plot::Block => "block",
        }
    }
}

/// A filled history graph: newest sample at the right edge, oldest to the left.
///
/// Anchoring "now" to the right edge rather than stretching the samples across the width means a
/// half-filled graph reads as history still accumulating, and the time labels stay true throughout.
pub struct Graph<'a> {
    /// Oldest first.
    pub values: &'a [f64],
    /// Full-scale value. Anything at or above this fills a column.
    pub max: f64,
    pub gradient: Gradient,
    pub plot: Plot,
    pub depth: Depth,
}

impl Graph<'_> {
    /// How many samples fit across `width` columns.
    pub fn capacity(plot: Plot, width: u16) -> usize {
        width as usize * plot.samples_per_cell()
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 || self.max <= 0.0 {
            return;
        }

        let rows_per_cell = self.plot.rows_per_cell();
        let columns = Self::capacity(self.plot, area.width);
        let sub_rows = area.height as usize * rows_per_cell;

        // Sub-column heights, right-aligned: the newest sample lands in the last column and any
        // shortfall leaves empty columns on the left rather than stretching what we have.
        let mut heights = vec![0usize; columns];
        let recent = &self.values[self.values.len().saturating_sub(columns)..];
        let offset = columns - recent.len();
        for (index, value) in recent.iter().enumerate() {
            heights[offset + index] = self.sub_rows_for(*value, sub_rows);
        }

        for cell_y in 0..area.height {
            // Colour by where the cell sits in the plot, not by the value under it: that is what
            // gives a tall column a cool base and a hot tip.
            let position = 1.0 - (cell_y as f64 + 0.5) / area.height as f64;
            let style = Style::default().fg(self.gradient.at(position, self.depth));

            for cell_x in 0..area.width {
                let Some(symbol) = self.symbol(&heights, sub_rows, cell_x, cell_y) else {
                    // Leave empty cells alone, so labels drawn under the graph survive.
                    continue;
                };
                buf[(area.x + cell_x, area.y + cell_y)].set_char(symbol).set_style(style);
            }
        }
    }

    /// The glyph for one cell, or `None` where nothing is filled.
    fn symbol(&self, heights: &[usize], sub_rows: usize, cell_x: u16, cell_y: u16) -> Option<char> {
        let rows_per_cell = self.plot.rows_per_cell();

        if self.plot == Plot::Block {
            let base = sub_rows - (cell_y as usize + 1) * rows_per_cell;
            let eighths = heights[cell_x as usize].saturating_sub(base).min(8);
            return (eighths > 0).then(|| EIGHTHS[eighths]);
        }

        // The 2x4 bit pattern, in the row-major order both symbol tables are indexed by.
        let mut pattern = 0usize;
        for row in 0..rows_per_cell {
            // Rows are numbered from the top; the fill is measured from the bottom.
            let from_bottom = sub_rows - (cell_y as usize * rows_per_cell + row);
            for column in 0..2 {
                if heights[cell_x as usize * 2 + column] >= from_bottom {
                    pattern |= 1 << (row * 2 + column);
                }
            }
        }

        if pattern == 0 {
            return None;
        }
        Some(match self.plot {
            Plot::Octant => pixel::OCTANTS[pattern],
            _ => braille::BRAILLE[pattern],
        })
    }

    /// Height of one sample in sub-rows. A non-zero reading always draws something, so a counter
    /// ticking along near zero is visible as a floor rather than as an empty chart.
    fn sub_rows_for(&self, value: f64, sub_rows: usize) -> usize {
        if !value.is_finite() || value <= 0.0 {
            return 0;
        }
        let scaled = (value / self.max * sub_rows as f64).round();
        (scaled.clamp(0.0, sub_rows as f64) as usize).max(1)
    }
}

/// One value as a chunky vertical bar filling `area`, coloured by height like a [`Graph`].
///
/// `track` shades the unfilled remainder. Bars in a group share a scale, so a small one is a
/// sliver at the bottom of its column; without the column drawn behind it, that sliver reads as a
/// stray rule rather than as a bar next to much larger neighbours.
pub fn vertical_bar(
    buf: &mut Buffer,
    area: Rect,
    fraction: f64,
    gradient: &Gradient,
    depth: Depth,
    track: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sub_rows = area.height as usize * 8;
    let filled = if fraction.is_finite() && fraction > 0.0 {
        ((fraction.clamp(0.0, 1.0) * sub_rows as f64).round() as usize).max(1)
    } else {
        0
    };

    for cell_y in 0..area.height {
        let base = sub_rows - (cell_y as usize + 1) * 8;
        let eighths = filled.saturating_sub(base).min(8);

        let (symbol, style) = if eighths == 0 {
            (METER_EMPTY, Style::default().fg(track))
        } else {
            let position = 1.0 - (cell_y as f64 + 0.5) / area.height as f64;
            (EIGHTHS[eighths], Style::default().fg(gradient.at(position, depth)))
        };

        for cell_x in 0..area.width {
            buf[(area.x + cell_x, area.y + cell_y)].set_char(symbol).set_style(style);
        }
    }
}

/// A horizontal meter across one row, filled left to right and coloured along the ramp.
pub fn meter(
    buf: &mut Buffer,
    area: Rect,
    fraction: f64,
    gradient: &Gradient,
    depth: Depth,
    empty: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let width = area.width as usize;
    let fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
    let filled = (fraction * width as f64).round() as usize;

    for cell_x in 0..width {
        let (symbol, color) = if cell_x < filled {
            // Ramp across the meter's own length, so a full meter shows the whole gradient.
            let position = if width > 1 { cell_x as f64 / (width - 1) as f64 } else { 1.0 };
            (EIGHTHS[8], gradient.at(position, depth))
        } else {
            (METER_EMPTY, empty)
        };
        buf[(area.x + cell_x as u16, area.y)]
            .set_char(symbol)
            .set_style(Style::default().fg(color));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::gradient::FLAME;

    fn graph(values: &[f64], max: f64, plot: Plot) -> Graph<'_> {
        Graph { values, max, gradient: FLAME, plot, depth: Depth::True }
    }

    /// Render into a fresh buffer and return it as lines of text.
    fn draw(values: &[f64], max: f64, plot: Plot, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        graph(values, max, plot).render(&mut buf, area);

        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn braille_packs_two_samples_into_every_cell() {
        assert_eq!(Graph::capacity(Plot::Braille, 40), 80);
        assert_eq!(Graph::capacity(Plot::Octant, 40), 80);
        assert_eq!(Graph::capacity(Plot::Block, 40), 40);
    }

    #[test]
    fn a_full_value_fills_the_column_to_the_top() {
        let lines = draw(&[100.0; 8], 100.0, Plot::Block, 8, 4);
        assert!(lines.iter().all(|line| line == "████████"), "{lines:?}");
    }

    #[test]
    fn an_empty_series_draws_nothing() {
        let lines = draw(&[], 100.0, Plot::Braille, 8, 3);
        assert!(lines.iter().all(|line| line.trim().is_empty()), "{lines:?}");
    }

    #[test]
    fn zeroes_draw_nothing_but_the_smallest_positive_value_still_shows() {
        let zeroes = draw(&[0.0; 8], 100.0, Plot::Block, 8, 4);
        assert!(zeroes.iter().all(|line| line.trim().is_empty()), "{zeroes:?}");

        // Well under a single eighth of one cell, and still visible.
        let tiny = draw(&[0.0001; 8], 100.0, Plot::Block, 8, 4);
        assert_eq!(tiny[3], "▁▁▁▁▁▁▁▁", "a live counter must never look dead");
    }

    #[test]
    fn history_is_anchored_to_the_right_edge() {
        // Two samples in an eight-wide block graph: they belong at the right, not stretched.
        let lines = draw(&[100.0, 100.0], 100.0, Plot::Block, 8, 2);
        assert_eq!(lines[0], "      ██", "{lines:?}");
    }

    #[test]
    fn older_samples_scroll_off_the_left() {
        // Ten samples, room for four: the last four win.
        let values = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 100.0, 100.0, 100.0, 100.0];
        let lines = draw(&values, 100.0, Plot::Block, 4, 2);
        assert_eq!(lines[0], "████", "{lines:?}");
    }

    #[test]
    fn a_half_height_value_fills_the_bottom_half_only() {
        let lines = draw(&[50.0; 4], 100.0, Plot::Block, 4, 2);
        assert!(lines[0].trim().is_empty(), "top row should be clear: {lines:?}");
        assert_eq!(lines[1], "████");
    }

    #[test]
    fn values_above_the_scale_are_clamped_rather_than_overflowing() {
        let lines = draw(&[400.0; 4], 100.0, Plot::Block, 4, 2);
        assert!(lines.iter().all(|line| line == "████"), "{lines:?}");
    }

    #[test]
    fn negative_and_non_finite_values_are_treated_as_empty() {
        let lines = draw(&[-5.0, f64::NAN, f64::INFINITY, -0.0], 100.0, Plot::Block, 4, 2);
        assert!(lines.iter().all(|line| line.trim().is_empty()), "{lines:?}");
    }

    #[test]
    fn braille_and_octant_draw_their_own_glyph_families() {
        let braille = draw(&[100.0; 16], 100.0, Plot::Braille, 8, 2);
        assert!(braille[0].chars().all(|c| c == '⣿'), "{braille:?}");

        let octant = draw(&[100.0; 16], 100.0, Plot::Octant, 8, 2);
        assert!(octant[0].chars().all(|c| c == '█'), "{octant:?}");
    }

    #[test]
    fn a_braille_blank_is_left_as_a_space_not_a_dotless_cell() {
        // BRAILLE[0] is U+2800, which is not a space and would overwrite anything beneath it.
        let lines = draw(&[0.0; 16], 100.0, Plot::Braille, 8, 2);
        assert!(!lines.concat().contains('\u{2800}'), "{lines:?}");
    }

    #[test]
    fn colour_climbs_the_ramp_with_height_not_with_the_value() {
        let area = Rect::new(0, 0, 4, 4);
        let mut buf = Buffer::empty(area);
        graph(&[100.0; 8], 100.0, Plot::Braille).render(&mut buf, area);

        let top = buf[(0, 0)].fg;
        let bottom = buf[(0, 3)].fg;
        assert_eq!(bottom, FLAME.at(0.125, Depth::True), "the base is the cool end");
        assert_eq!(top, FLAME.at(0.875, Depth::True), "the peak is the hot end");
        assert_ne!(top, bottom);
    }

    #[test]
    fn a_zero_scale_draws_nothing_rather_than_dividing_by_it() {
        let lines = draw(&[5.0; 4], 0.0, Plot::Block, 4, 2);
        assert!(lines.iter().all(|line| line.trim().is_empty()), "{lines:?}");
    }

    #[test]
    fn a_degenerate_area_is_survivable() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        graph(&[1.0], 1.0, Plot::Braille).render(&mut buf, Rect::new(0, 0, 0, 5));
        graph(&[1.0], 1.0, Plot::Braille).render(&mut buf, Rect::new(0, 0, 5, 0));
    }

    #[test]
    fn a_bar_fills_from_the_bottom_over_its_track() {
        let area = Rect::new(0, 0, 3, 4);
        let mut buf = Buffer::empty(area);
        vertical_bar(&mut buf, area, 0.5, &FLAME, Depth::True, Color::DarkGray);

        assert_eq!(buf[(0, 0)].symbol(), "░", "the unfilled half shows the track");
        assert_eq!(buf[(0, 2)].symbol(), "█");
        assert_eq!(buf[(0, 3)].symbol(), "█");
    }

    #[test]
    fn a_bar_of_almost_nothing_still_shows_a_sliver() {
        let area = Rect::new(0, 0, 2, 4);
        let mut buf = Buffer::empty(area);
        vertical_bar(&mut buf, area, 0.0001, &FLAME, Depth::True, Color::DarkGray);
        assert_eq!(buf[(0, 3)].symbol(), "▁");
    }

    #[test]
    fn an_empty_bar_is_all_track() {
        let area = Rect::new(0, 0, 2, 2);
        let mut buf = Buffer::empty(area);
        vertical_bar(&mut buf, area, 0.0, &FLAME, Depth::True, Color::DarkGray);
        assert_eq!(buf[(0, 1)].symbol(), "░");
        assert_eq!(buf[(0, 1)].fg, Color::DarkGray);
    }

    #[test]
    fn a_meter_fills_left_to_right_and_marks_the_rest_as_empty() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        meter(&mut buf, area, 0.4, &FLAME, Depth::True, Color::DarkGray);

        let row: String = (0..10).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, "████░░░░░░");
    }

    #[test]
    fn a_meter_clamps_instead_of_running_past_its_area() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        meter(&mut buf, area, 5.0, &FLAME, Depth::True, Color::DarkGray);

        let row: String = (0..4).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row, "████");
    }

    #[test]
    fn the_plot_mode_cycles_through_every_family_and_back() {
        let mut plot = Plot::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(plot.name());
            plot = plot.next();
        }
        assert_eq!(seen, ["braille", "block", "octant"]);
        assert_eq!(plot, Plot::Braille, "cycling must return to the start");
    }
}
