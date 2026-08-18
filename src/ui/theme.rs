//! Colours.
//!
//! The background is deliberately left as the terminal's own: countercow should sit inside the
//! user's colour scheme rather than paint over it, and that also makes light terminals work
//! without a second palette. Text and chrome therefore stick to the sixteen ANSI names, which the
//! terminal theme gets to define. The graphs are the exception — a ramp needs more colours than
//! that — so they carry their own RGB gradients, rendered at whatever depth the terminal has.

use ratatui::style::Color;

use super::gradient::{self, Depth, Gradient, Rgb};
use super::graph::Plot;

/// Base colours for the generation bars, youngest to oldest. Hue identifies the generation, so
/// each bar's ramp only varies brightness around its own colour.
const GENERATION_COLOURS: [Rgb; 5] = [
    Rgb(0x22, 0xd3, 0xee), // gen 0 — cyan
    Rgb(0x60, 0xa5, 0xfa), // gen 1 — blue
    Rgb(0xc0, 0x84, 0xfc), // gen 2 — violet
    Rgb(0xfb, 0xbf, 0x24), // LOH   — amber
    Rgb(0xfb, 0x71, 0x85), // POH   — rose
];

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub title: Color,
    pub accent: Color,
    pub good: Color,
    pub warn: Color,
    pub bad: Color,
    /// CPU and anything else where more means hotter.
    pub cpu: Gradient,
    /// Memory. A different hue family from [`Theme::cpu`], so a glance tells the heap chart from
    /// the CPU chart without reading either title.
    pub heap: Gradient,
    /// Throughput: requests and other rates.
    pub rate: Gradient,
    pub generations: [Rgb; 5],
    pub plot: Plot,
    pub depth: Depth,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            fg: Color::Reset,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            title: Color::Cyan,
            accent: Color::Cyan,
            good: Color::Green,
            warn: Color::Yellow,
            bad: Color::Red,
            cpu: gradient::FLAME,
            heap: gradient::COOL,
            rate: gradient::RATE,
            generations: GENERATION_COLOURS,
            plot: Plot::default(),
            depth: Depth::detect(),
        }
    }
}

impl Theme {
    /// Colour a percentage by severity: calm below 60, warning to 85, alarming above.
    pub fn for_percent(&self, percent: f64) -> Color {
        if percent >= 85.0 {
            self.bad
        } else if percent >= 60.0 {
            self.warn
        } else {
            self.good
        }
    }

    /// The ramp for one generation bar.
    pub fn generation_gradient(&self, index: usize) -> Gradient {
        Gradient::around(self.generations[index % self.generations.len()])
    }

    /// A flat colour from a ramp, for text that should match the graph it labels.
    pub fn on(&self, gradient: &Gradient, position: f64) -> Color {
        gradient.at(position, self.depth)
    }

    /// Cycle the graph glyph family.
    pub fn cycle_plot(&mut self) {
        self.plot = self.plot.next();
    }

    pub fn plot_name(&self) -> &'static str {
        self.plot.name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_severity_escalates() {
        let theme = Theme::default();
        assert_eq!(theme.for_percent(10.0), theme.good);
        assert_eq!(theme.for_percent(70.0), theme.warn);
        assert_eq!(theme.for_percent(99.0), theme.bad);
    }

    #[test]
    fn the_plot_family_cycles_and_returns_to_braille() {
        let mut theme = Theme::default();
        assert_eq!(theme.plot_name(), "braille");
        theme.cycle_plot();
        assert_eq!(theme.plot_name(), "block");
        theme.cycle_plot();
        assert_eq!(theme.plot_name(), "octant");
        theme.cycle_plot();
        assert_eq!(theme.plot_name(), "braille");
    }

    #[test]
    fn background_is_left_to_the_terminal() {
        assert_eq!(Theme::default().fg, Color::Reset);
    }

    #[test]
    fn every_generation_keeps_its_own_hue_in_the_middle_of_its_ramp() {
        let theme = Theme::default();
        for (index, base) in GENERATION_COLOURS.iter().enumerate() {
            assert_eq!(theme.generation_gradient(index).rgb_at(0.5), *base);
        }
    }

    #[test]
    fn generation_ramps_run_dark_to_light() {
        let ramp = Theme::default().generation_gradient(0);
        assert!(ramp.low.1 < ramp.mid.1 && ramp.mid.1 < ramp.high.1, "{ramp:?}");
    }

    #[test]
    fn the_chart_ramps_are_distinguishable_from_one_another() {
        let theme = Theme::default();
        assert_ne!(theme.cpu, theme.heap);
        assert_ne!(theme.cpu, theme.rate);
        assert_ne!(theme.heap, theme.rate);
    }
}
