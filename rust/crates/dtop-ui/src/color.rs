//! Terminal colors and styles, with truecolor → 256 → 16 downsampling and `NO_COLOR`.

use std::collections::HashMap;
use std::fmt;

/// `Ansi` is the 16-color palette (0-7 normal, 8-15 bright), `Indexed` the 256-color palette,
/// `Rgb` 24-bit truecolor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

/// xterm's default RGB for the 16 base colors, for nearest-color matching.
const ANSI_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

const CUBE_LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];

impl Color {
    pub const BLACK: Color = Color::Ansi(0);
    pub const RED: Color = Color::Ansi(1);
    pub const GREEN: Color = Color::Ansi(2);
    pub const YELLOW: Color = Color::Ansi(3);
    pub const BLUE: Color = Color::Ansi(4);
    pub const MAGENTA: Color = Color::Ansi(5);
    pub const CYAN: Color = Color::Ansi(6);
    pub const WHITE: Color = Color::Ansi(7);
    pub const BRIGHT_BLACK: Color = Color::Ansi(8);
    pub const BRIGHT_RED: Color = Color::Ansi(9);
    pub const BRIGHT_GREEN: Color = Color::Ansi(10);
    pub const BRIGHT_YELLOW: Color = Color::Ansi(11);
    pub const BRIGHT_BLUE: Color = Color::Ansi(12);
    pub const BRIGHT_MAGENTA: Color = Color::Ansi(13);
    pub const BRIGHT_CYAN: Color = Color::Ansi(14);
    pub const BRIGHT_WHITE: Color = Color::Ansi(15);

    /// Parses `#rgb`, `#rrggbb`, `rgb(r,g,b)`, `color(n)` / `256:n`, `ansi:n`, names and `default`.
    pub fn parse(raw: &str) -> Option<Color> {
        let text = raw.trim().to_lowercase();
        if text.is_empty() {
            return None;
        }
        if text == "default" || text == "none" {
            return Some(Color::Default);
        }
        if let Some(hex) = text.strip_prefix('#') {
            let value = |s: &str| u8::from_str_radix(s, 16).ok();
            if !hex.is_ascii() {
                return None;
            }
            return match hex.len() {
                6 => Some(Color::Rgb(
                    value(&hex[0..2])?,
                    value(&hex[2..4])?,
                    value(&hex[4..6])?,
                )),
                3 => Some(Color::Rgb(
                    value(&hex[0..1])? * 17,
                    value(&hex[1..2])? * 17,
                    value(&hex[2..3])? * 17,
                )),
                _ => None,
            };
        }
        if let Some(inner) = text.strip_prefix("rgb(").and_then(|t| t.strip_suffix(')')) {
            let parts: Vec<Option<u8>> = inner.split(',').map(|p| p.trim().parse().ok()).collect();
            if let [Some(r), Some(g), Some(b)] = parts[..] {
                return Some(Color::Rgb(r, g, b));
            }
            return None;
        }
        if let Some(inner) = text
            .strip_prefix("color(")
            .and_then(|t| t.strip_suffix(')'))
        {
            return inner.trim().parse().ok().map(Color::Indexed);
        }
        if let Some(n) = text.strip_prefix("256:") {
            return n.parse().ok().map(Color::Indexed);
        }
        if let Some(n) = text.strip_prefix("ansi:") {
            return n.parse::<u8>().ok().filter(|i| *i < 16).map(Color::Ansi);
        }
        let normalized = text.replace(['_', ' '], "-");
        let name = match normalized.as_str() {
            "gray" | "grey" => "bright-black",
            "bright-gray" | "bright-grey" => "white",
            other => other,
        };
        ANSI_NAMES
            .iter()
            .position(|n| *n == name)
            .map(|i| Color::Ansi(i as u8))
    }

    /// Approximate RGB (None for `Default`).
    pub fn rgb_components(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Default => None,
            Color::Ansi(i) => Some(ANSI_RGB[usize::from(i % 16)]),
            Color::Indexed(i) if i < 16 => Some(ANSI_RGB[usize::from(i)]),
            Color::Indexed(i) if i >= 232 => {
                let level = (8 + 10 * (i32::from(i) - 232)) as u8;
                Some((level, level, level))
            }
            Color::Indexed(i) => {
                let cube = usize::from(i) - 16;
                Some((
                    CUBE_LEVELS[cube / 36] as u8,
                    CUBE_LEVELS[(cube / 6) % 6] as u8,
                    CUBE_LEVELS[cube % 6] as u8,
                ))
            }
            Color::Rgb(r, g, b) => Some((r, g, b)),
        }
    }

    /// Converts to something the terminal's color mode can show.
    pub fn downsampled(self, mode: ColorMode) -> Color {
        match (self, mode) {
            (Color::Default, _) | (_, ColorMode::None) => Color::Default,
            (_, ColorMode::TrueColor) => self,
            (Color::Rgb(r, g, b), ColorMode::Palette256) => Color::Indexed(nearest256(r, g, b)),
            (Color::Indexed(_) | Color::Ansi(_), ColorMode::Palette256)
            | (Color::Ansi(_), ColorMode::Basic16) => self,
            (Color::Indexed(i), ColorMode::Basic16) => {
                if i < 16 {
                    return Color::Ansi(i);
                }
                match self.rgb_components() {
                    Some((r, g, b)) => Color::Ansi(nearest16(r, g, b)),
                    None => Color::Default,
                }
            }
            (Color::Rgb(r, g, b), ColorMode::Basic16) => Color::Ansi(nearest16(r, g, b)),
        }
    }

    /// SGR parameters for this (already downsampled) color.
    pub fn sgr_parameters(self, foreground: bool) -> String {
        match self {
            Color::Default => if foreground { "39" } else { "49" }.into(),
            Color::Ansi(i) => {
                let base = if foreground { 30 } else { 40 };
                if i < 8 {
                    (base + i32::from(i)).to_string()
                } else {
                    (base + 60 + i32::from(i) - 8).to_string()
                }
            }
            Color::Indexed(i) => format!("{}{i}", if foreground { "38;5;" } else { "48;5;" }),
            Color::Rgb(r, g, b) => {
                format!("{}{r};{g};{b}", if foreground { "38;2;" } else { "48;2;" })
            }
        }
    }

    /// Linear blend (falls back to the nearer endpoint for non-RGB colors).
    pub fn blend(from: Color, to: Color, fraction: f64) -> Color {
        let t = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        match (from, to) {
            (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
                let mix = |a: u8, b: u8| {
                    (f64::from(a) + (f64::from(b) - f64::from(a)) * t)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };
                Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
            }
            _ => {
                if t < 0.5 {
                    from
                } else {
                    to
                }
            }
        }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::Default => f.write_str("default"),
            Color::Ansi(i) if usize::from(*i) < ANSI_NAMES.len() => {
                f.write_str(ANSI_NAMES[usize::from(*i)])
            }
            Color::Ansi(i) => write!(f, "ansi:{i}"),
            Color::Indexed(i) => write!(f, "color({i})"),
            Color::Rgb(r, g, b) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// Weighted Euclidean distance; green matters most to perceived brightness.
fn distance(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
    let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    2 * dr * dr + 4 * dg * dg + 3 * db * db
}

fn nearest256(r: u8, g: u8, b: u8) -> u8 {
    let target = (i32::from(r), i32::from(g), i32::from(b));
    let cube_index = |value: i32| {
        let mut best = 0;
        for (i, level) in CUBE_LEVELS.iter().enumerate() {
            if (level - value).abs() < (CUBE_LEVELS[best] - value).abs() {
                best = i;
            }
        }
        best
    };
    let (ri, gi, bi) = (
        cube_index(target.0),
        cube_index(target.1),
        cube_index(target.2),
    );
    let cube_color = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let average = (target.0 + target.1 + target.2) / 3;
    let gray_step = ((average - 8 + 5) / 10).clamp(0, 23);
    let gray_level = 8 + 10 * gray_step;
    let gray = 232 + gray_step;
    if distance(target, (gray_level, gray_level, gray_level)) < distance(target, cube_color) {
        gray as u8
    } else {
        cube as u8
    }
}

fn nearest16(r: u8, g: u8, b: u8) -> u8 {
    let target = (i32::from(r), i32::from(g), i32::from(b));
    let mut best = 0;
    let mut best_distance = i32::MAX;
    for (i, rgb) in ANSI_RGB.iter().enumerate() {
        let candidate = distance(
            target,
            (i32::from(rgb.0), i32::from(rgb.1), i32::from(rgb.2)),
        );
        if candidate < best_distance {
            best = i;
            best_distance = candidate;
        }
    }
    best as u8
}

/// How many colors the terminal supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorMode {
    TrueColor,
    Palette256,
    Basic16,
    None,
}

impl ColorMode {
    pub const ALL: [ColorMode; 4] = [
        ColorMode::TrueColor,
        ColorMode::Palette256,
        ColorMode::Basic16,
        ColorMode::None,
    ];

    pub fn raw_value(self) -> &'static str {
        match self {
            ColorMode::TrueColor => "truecolor",
            ColorMode::Palette256 => "256",
            ColorMode::Basic16 => "16",
            ColorMode::None => "none",
        }
    }

    /// Detection from the environment, following the `NO_COLOR` convention.
    pub fn detect(environment: &HashMap<String, String>) -> ColorMode {
        let get = |k: &str| environment.get(k).map(String::as_str);
        if get("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return ColorMode::None;
        }
        let term = get("TERM").unwrap_or("").to_lowercase();
        if term == "dumb" {
            return ColorMode::None;
        }
        let color_term = get("COLORTERM").unwrap_or("").to_lowercase();
        if color_term == "truecolor" || color_term == "24bit" {
            return ColorMode::TrueColor;
        }
        if get("WT_SESSION").is_some() {
            return ColorMode::TrueColor; // Windows Terminal
        }
        if get("TERM_PROGRAM")
            .is_some_and(|p| ["iTerm.app", "WezTerm", "vscode", "ghostty"].contains(&p))
        {
            return ColorMode::TrueColor;
        }
        if ["truecolor", "24bit", "direct"]
            .iter()
            .any(|s| term.contains(s))
        {
            return ColorMode::TrueColor;
        }
        if term.contains("256") {
            return ColorMode::Palette256;
        }
        if term.is_empty() && get("ConEmuANSI") == Some("ON") {
            return ColorMode::Palette256;
        }
        ColorMode::Basic16
    }

    pub fn parse(raw: &str) -> Option<ColorMode> {
        match raw.trim().to_lowercase().as_str() {
            "truecolor" | "24bit" | "true" | "rgb" => Some(ColorMode::TrueColor),
            "256" | "palette" | "palette256" => Some(ColorMode::Palette256),
            "16" | "basic" | "ansi" => Some(ColorMode::Basic16),
            "none" | "off" | "mono" | "no" => Some(ColorMode::None),
            _ => None,
        }
    }
}

/// Bold, dim, italic, underline, reverse, strikethrough.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Attributes(pub u8);

impl Attributes {
    pub const NONE: Attributes = Attributes(0);
    pub const BOLD: Attributes = Attributes(1 << 0);
    pub const DIM: Attributes = Attributes(1 << 1);
    pub const ITALIC: Attributes = Attributes(1 << 2);
    pub const UNDERLINE: Attributes = Attributes(1 << 3);
    pub const REVERSE: Attributes = Attributes(1 << 4);
    pub const STRIKETHROUGH: Attributes = Attributes(1 << 5);

    pub fn contains(self, other: Attributes) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Attributes) -> Attributes {
        Attributes(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// SGR codes in a fixed order so output is deterministic.
    fn sgr_codes(self) -> Vec<&'static str> {
        [
            (Self::BOLD, "1"),
            (Self::DIM, "2"),
            (Self::ITALIC, "3"),
            (Self::UNDERLINE, "4"),
            (Self::REVERSE, "7"),
            (Self::STRIKETHROUGH, "9"),
        ]
        .into_iter()
        .filter(|(a, _)| self.contains(*a))
        .map(|(_, c)| c)
        .collect()
    }
}

impl std::ops::BitOr for Attributes {
    type Output = Attributes;
    fn bitor(self, rhs: Attributes) -> Attributes {
        self.union(rhs)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attributes: Attributes,
}

impl Style {
    pub const PLAIN: Style = Style {
        fg: Color::Default,
        bg: Color::Default,
        attributes: Attributes::NONE,
    };

    pub fn new(fg: Color, bg: Color) -> Style {
        Style {
            fg,
            bg,
            attributes: Attributes::NONE,
        }
    }

    pub fn fg(fg: Color) -> Style {
        Style { fg, ..Style::PLAIN }
    }

    pub fn with_fg(self, fg: Color) -> Style {
        Style { fg, ..self }
    }

    pub fn with_bg(self, bg: Color) -> Style {
        Style { bg, ..self }
    }

    pub fn adding(self, extra: Attributes) -> Style {
        Style {
            attributes: self.attributes.union(extra),
            ..self
        }
    }

    pub fn bold(self) -> Style {
        self.adding(Attributes::BOLD)
    }

    /// The full SGR sequence (starting from a reset) for this style in a color mode.
    pub fn sgr(self, mode: ColorMode) -> String {
        let mut codes: Vec<String> = vec!["0".into()];
        codes.extend(self.attributes.sgr_codes().into_iter().map(String::from));
        if mode != ColorMode::None {
            let fg = self.fg.downsampled(mode);
            let bg = self.bg.downsampled(mode);
            if fg != Color::Default {
                codes.push(fg.sgr_parameters(true));
            }
            if bg != Color::Default {
                codes.push(bg.sgr_parameters(false));
            }
        }
        format!("\u{1b}[{}m", codes.join(";"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_color_form() {
        assert_eq!(Color::parse("#ff8000"), Some(Color::Rgb(255, 128, 0)));
        assert_eq!(Color::parse("#f80"), Some(Color::Rgb(255, 136, 0)));
        assert_eq!(Color::parse("rgb(1, 2, 3)"), Some(Color::Rgb(1, 2, 3)));
        assert_eq!(Color::parse("color(42)"), Some(Color::Indexed(42)));
        assert_eq!(Color::parse("256:7"), Some(Color::Indexed(7)));
        assert_eq!(Color::parse("ansi:9"), Some(Color::Ansi(9)));
        assert_eq!(Color::parse("Bright Red"), Some(Color::BRIGHT_RED));
        assert_eq!(Color::parse("grey"), Some(Color::BRIGHT_BLACK));
        assert_eq!(Color::parse("none"), Some(Color::Default));
        assert_eq!(Color::parse("#12"), None);
        assert_eq!(Color::parse("ansi:16"), None);
    }

    #[test]
    fn round_trips_through_display() {
        for c in [
            Color::Rgb(1, 2, 255),
            Color::Indexed(200),
            Color::Ansi(3),
            Color::Default,
        ] {
            assert_eq!(Color::parse(&c.to_string()), Some(c));
        }
    }

    #[test]
    fn downsamples() {
        assert_eq!(
            Color::Rgb(255, 0, 0).downsampled(ColorMode::Palette256),
            Color::Indexed(196)
        );
        assert_eq!(
            Color::Rgb(128, 128, 128).downsampled(ColorMode::Palette256),
            Color::Indexed(244)
        );
        assert_eq!(
            Color::Rgb(250, 10, 10).downsampled(ColorMode::Basic16),
            Color::BRIGHT_RED
        );
        assert_eq!(
            Color::Indexed(9).downsampled(ColorMode::Basic16),
            Color::Ansi(9)
        );
        assert_eq!(
            Color::Rgb(1, 2, 3).downsampled(ColorMode::None),
            Color::Default
        );
    }

    #[test]
    fn builds_sgr_sequences() {
        let style = Style::new(Color::BRIGHT_RED, Color::Rgb(1, 2, 3)).bold();
        assert_eq!(style.sgr(ColorMode::TrueColor), "\u{1b}[0;1;91;48;2;1;2;3m");
        assert_eq!(style.sgr(ColorMode::None), "\u{1b}[0;1m");
    }

    #[test]
    fn detects_color_mode() {
        let env = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            ColorMode::detect(&env(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            ColorMode::None
        );
        assert_eq!(
            ColorMode::detect(&env(&[("COLORTERM", "truecolor")])),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::detect(&env(&[("TERM", "xterm-256color")])),
            ColorMode::Palette256
        );
        assert_eq!(
            ColorMode::detect(&env(&[("TERM", "xterm")])),
            ColorMode::Basic16
        );
    }

    #[test]
    fn blends_rgb() {
        assert_eq!(
            Color::blend(Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50), 0.5),
            Color::Rgb(100, 50, 25)
        );
        assert_eq!(Color::blend(Color::RED, Color::GREEN, 0.7), Color::GREEN);
    }
}
