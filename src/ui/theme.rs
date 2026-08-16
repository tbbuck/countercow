//! Colours.
//!
//! The background is deliberately left as the terminal's own: countercow should sit inside the
//! user's colour scheme rather than paint over it, and that also makes light terminals work
//! without a second palette.

use ratatui::style::Color;
use ratatui::symbols::Marker;

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
    /// Per-generation colours for the GC panel, coolest (youngest) to warmest.
    pub generations: [Color; 5],
    pub marker: Marker,
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
            generations: [
                Color::Cyan,
                Color::Blue,
                Color::Magenta,
                Color::Yellow,
                Color::LightRed,
            ],
            marker: Marker::Braille,
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

    /// Switch between Braille and Octant plotting.
    ///
    /// Both give 2x4 sub-cell resolution, but Octant packs densely with no gaps between cells,
    /// which looks markedly better — where the terminal font has the glyphs. Braille has far
    /// wider support, so it stays the default.
    pub fn toggle_marker(&mut self) {
        self.marker = match self.marker {
            Marker::Braille => Marker::Octant,
            _ => Marker::Braille,
        };
    }

    pub fn marker_name(&self) -> &'static str {
        match self.marker {
            Marker::Braille => "braille",
            Marker::Octant => "octant",
            _ => "other",
        }
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
    fn marker_toggles_between_the_two_high_resolution_options() {
        let mut theme = Theme::default();
        assert_eq!(theme.marker_name(), "braille");
        theme.toggle_marker();
        assert_eq!(theme.marker_name(), "octant");
        theme.toggle_marker();
        assert_eq!(theme.marker_name(), "braille");
    }

    #[test]
    fn background_is_left_to_the_terminal() {
        assert_eq!(Theme::default().fg, Color::Reset);
    }
}
