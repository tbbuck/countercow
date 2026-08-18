//! Colour ramps.
//!
//! The graphs colour every cell by how high up the plot it sits, so a quiet trace stays green and
//! a spike burns through amber into red. That needs many more colours than the sixteen ANSI names
//! the rest of the theme sticks to, so ramps are defined in RGB and rendered at whatever depth the
//! terminal actually has.

use ratatui::style::Color;

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Depth {
    /// 24-bit colour: ramps render exactly.
    #[default]
    True,
    /// The xterm 256-colour palette: ramps snap to the nearest entry.
    Ansi256,
}

impl Depth {
    /// What the environment claims this terminal can do.
    pub fn detect() -> Self {
        Self::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// `COLORTERM` is the only variable terminals set at all consistently for 24-bit colour;
    /// `TERM` only says so on the handful of entries built with direct-colour support. Anything
    /// else is assumed to be 256-colour, which every terminal worth drawing on has had for
    /// decades, and which a ramp survives being quantised to.
    fn from_env(colorterm: Option<&str>, term: Option<&str>) -> Self {
        let claims_truecolor = colorterm.is_some_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) || term.is_some_and(|value| value.contains("truecolor") || value.contains("direct"));

        if claims_truecolor {
            Depth::True
        } else {
            Depth::Ansi256
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The six values the xterm colour cube samples each channel at.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

impl Rgb {
    /// Mix towards `other`. `t` is clamped, so callers need not.
    pub fn mix(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
        Rgb(channel(self.0, other.0), channel(self.1, other.1), channel(self.2, other.2))
    }

    /// Scale brightness. Above 1.0 lightens towards white rather than clipping to it, which keeps
    /// the hue instead of washing every bright end out to the same grey-white.
    pub fn scale(self, factor: f64) -> Self {
        if factor <= 1.0 {
            let channel = |v: u8| (v as f64 * factor.max(0.0)).round().min(255.0) as u8;
            Rgb(channel(self.0), channel(self.1), channel(self.2))
        } else {
            self.mix(Rgb(255, 255, 255), (factor - 1.0).min(1.0))
        }
    }

    pub fn to_color(self, depth: Depth) -> Color {
        match depth {
            Depth::True => Color::Rgb(self.0, self.1, self.2),
            Depth::Ansi256 => Color::Indexed(self.to_ansi256()),
        }
    }

    /// Squared distance, for picking the closest palette entry.
    fn distance(self, other: Self) -> i32 {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(self.0, other.0) + d(self.1, other.1) + d(self.2, other.2)
    }

    /// The nearest xterm-256 entry, considering both the colour cube and the grey ramp.
    ///
    /// The grey ramp matters: the cube's darkest non-black step is 95, so quantising a near-black
    /// to the cube alone would brighten it hard enough to be visible as a band.
    fn to_ansi256(self) -> u8 {
        let nearest_level = |v: u8| {
            CUBE_LEVELS
                .iter()
                .enumerate()
                .min_by_key(|(_, level)| (**level as i32 - v as i32).abs())
                .map(|(index, _)| index)
                .unwrap_or(0)
        };
        let (r, g, b) = (nearest_level(self.0), nearest_level(self.1), nearest_level(self.2));
        let cube = Rgb(CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]);

        let average = (self.0 as u32 + self.1 as u32 + self.2 as u32) / 3;
        let grey_index = (((average as i32 - 8) + 5) / 10).clamp(0, 23);
        let grey_value = (8 + grey_index * 10) as u8;
        let grey = Rgb(grey_value, grey_value, grey_value);

        if grey.distance(self) < cube.distance(self) {
            232 + grey_index as u8
        } else {
            16 + (36 * r + 6 * g + b) as u8
        }
    }
}

/// A three-stop ramp, coolest at the base and hottest at the peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gradient {
    pub low: Rgb,
    pub mid: Rgb,
    pub high: Rgb,
}

impl Gradient {
    pub const fn new(low: Rgb, mid: Rgb, high: Rgb) -> Self {
        Self { low, mid, high }
    }

    /// Build a ramp around one colour: dimmed at the base, the colour itself in the middle,
    /// lightened at the peak. Used where the hue has to identify something — the generation bars —
    /// so the ramp can only vary brightness.
    pub fn around(base: Rgb) -> Self {
        Self::new(base.scale(0.45), base, base.scale(1.35))
    }

    pub fn rgb_at(&self, t: f64) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            self.low.mix(self.mid, t * 2.0)
        } else {
            self.mid.mix(self.high, (t - 0.5) * 2.0)
        }
    }

    pub fn at(&self, t: f64, depth: Depth) -> Color {
        self.rgb_at(t).to_color(depth)
    }
}

/// CPU and anything else where more means hotter.
pub const FLAME: Gradient =
    Gradient::new(Rgb(0x50, 0xf0, 0x95), Rgb(0xf2, 0xe2, 0x66), Rgb(0xfa, 0x1e, 0x1e));

/// Memory. Deliberately a different hue family from [`FLAME`] so a glance tells the heap chart
/// from the CPU chart without reading either title.
pub const COOL: Gradient =
    Gradient::new(Rgb(0x2d, 0xd4, 0xbf), Rgb(0x3b, 0x82, 0xf6), Rgb(0xa8, 0x55, 0xf7));

/// Throughput: requests and other rates.
pub const RATE: Gradient =
    Gradient::new(Rgb(0xa3, 0xe6, 0x35), Rgb(0xfb, 0x92, 0x3c), Rgb(0xe1, 0x1d, 0x48));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_is_taken_from_colorterm() {
        assert_eq!(Depth::from_env(Some("truecolor"), None), Depth::True);
        assert_eq!(Depth::from_env(Some("24bit"), None), Depth::True);
        assert_eq!(Depth::from_env(Some("TrueColor"), None), Depth::True);
    }

    #[test]
    fn anything_unproven_falls_back_to_256_colours() {
        assert_eq!(Depth::from_env(None, Some("xterm-256color")), Depth::Ansi256);
        assert_eq!(Depth::from_env(Some(""), Some("screen")), Depth::Ansi256);
        assert_eq!(Depth::from_env(None, None), Depth::Ansi256);
    }

    #[test]
    fn a_direct_colour_terminfo_entry_counts_as_truecolor() {
        assert_eq!(Depth::from_env(None, Some("xterm-direct")), Depth::True);
    }

    #[test]
    fn a_ramp_runs_through_all_three_stops() {
        assert_eq!(FLAME.rgb_at(0.0), FLAME.low);
        assert_eq!(FLAME.rgb_at(0.5), FLAME.mid);
        assert_eq!(FLAME.rgb_at(1.0), FLAME.high);
    }

    #[test]
    fn a_ramp_is_clamped_at_both_ends() {
        assert_eq!(FLAME.rgb_at(-3.0), FLAME.low);
        assert_eq!(FLAME.rgb_at(9.0), FLAME.high);
    }

    #[test]
    fn quarter_way_is_between_the_low_and_mid_stops() {
        let quarter = FLAME.rgb_at(0.25);
        assert!(quarter.0 > FLAME.low.0 && quarter.0 < FLAME.mid.0, "{quarter:?}");
    }

    #[test]
    fn truecolor_renders_the_exact_channel_values() {
        assert_eq!(Rgb(1, 2, 3).to_color(Depth::True), Color::Rgb(1, 2, 3));
    }

    #[test]
    fn palette_indices_stay_inside_the_cube_and_grey_ramp() {
        for r in 0..=255u8 {
            let index = Rgb(r, 255 - r, r / 2).to_ansi256();
            assert!((16..=255).contains(&index), "{r} produced {index}");
        }
    }

    #[test]
    fn pure_colours_quantise_to_their_own_cube_corner() {
        // 16 is the cube's black corner, 231 its white one.
        assert_eq!(Rgb(0, 0, 0).to_ansi256(), 16);
        assert_eq!(Rgb(255, 255, 255).to_ansi256(), 231);
        assert_eq!(Rgb(255, 0, 0).to_ansi256(), 196);
    }

    #[test]
    fn near_neutral_colours_use_the_grey_ramp_rather_than_the_cube() {
        // The cube would round this to 0 or 95 and shift it visibly.
        assert!((232..=255).contains(&Rgb(58, 58, 58).to_ansi256()));
    }

    #[test]
    fn scaling_down_darkens_and_scaling_up_lightens() {
        let base = Rgb(100, 100, 100);
        assert_eq!(base.scale(0.5), Rgb(50, 50, 50));
        let brighter = base.scale(1.5);
        assert!(brighter.0 > base.0, "{brighter:?}");
    }

    #[test]
    fn brightening_never_overflows_a_channel() {
        let white = Rgb(250, 10, 10).scale(4.0);
        assert_eq!(white, Rgb(255, 255, 255));
    }

    #[test]
    fn a_ramp_around_a_colour_keeps_it_in_the_middle() {
        let base = Rgb(0x22, 0xd3, 0xee);
        assert_eq!(Gradient::around(base).rgb_at(0.5), base);
    }
}
