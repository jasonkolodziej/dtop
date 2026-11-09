//! Themes: semantic color roles, the built-ins, and the `key = value` theme-file format.

use std::collections::BTreeMap;
use std::fmt;

use crate::builtin_themes;
use crate::color::{Attributes, Color, Style};
use crate::widgets::BorderCharacters;

macro_rules! roles {
    ($($variant:ident => $raw:literal),* $(,)?) => {
        /// Semantic color slots. Views ask for roles, never raw colors, so themes stay
        /// interchangeable.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Role { $($variant),* }

        impl Role {
            pub const ALL: &'static [Role] = &[$(Role::$variant),*];

            pub fn raw_value(self) -> &'static str {
                match self { $(Role::$variant => $raw),* }
            }

            pub fn from_raw(raw: &str) -> Option<Role> {
                match raw { $($raw => Some(Role::$variant),)* _ => None }
            }
        }
    };
}

roles! {
    Background => "background",
    Foreground => "foreground",
    Muted => "muted",
    Accent => "accent",
    Title => "title",
    Border => "border",
    BorderFocused => "border_focused",
    HeaderBackground => "header_background",
    HeaderForeground => "header_foreground",
    TabActive => "tab_active",
    TabInactive => "tab_inactive",
    TableHeader => "table_header",
    SelectionBackground => "selection_background",
    SelectionForeground => "selection_foreground",
    StatusRunning => "status_running",
    StatusPaused => "status_paused",
    StatusExited => "status_exited",
    StatusRestarting => "status_restarting",
    StatusDead => "status_dead",
    StatusCreated => "status_created",
    Healthy => "healthy",
    Unhealthy => "unhealthy",
    GraphLow => "graph_low",
    GraphMid => "graph_mid",
    GraphHigh => "graph_high",
    Cpu => "cpu",
    Memory => "memory",
    NetworkRx => "network_rx",
    NetworkTx => "network_tx",
    Disk => "disk",
    Gpu => "gpu",
    Success => "success",
    Warning => "warning",
    Error => "error",
    DialogBackground => "dialog_background",
    DialogBorder => "dialog_border",
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub colors: BTreeMap<Role, Color>,
    /// Monochrome themes lean on attributes (bold/reverse) instead of color.
    pub uses_attributes_for_emphasis: bool,
    pub border: BorderCharacters,
}

impl Theme {
    /// A theme with every role at the terminal default.
    pub fn blank(name: &str) -> Theme {
        Theme {
            name: name.into(),
            colors: BTreeMap::new(),
            uses_attributes_for_emphasis: false,
            border: BorderCharacters::ROUNDED,
        }
    }

    pub fn color(&self, role: Role) -> Color {
        self.colors.get(&role).copied().unwrap_or(Color::Default)
    }

    /// A role's color on the theme background.
    pub fn style(&self, role: Role) -> Style {
        Style::new(self.color(role), self.color(Role::Background))
    }

    pub fn base(&self) -> Style {
        self.style(Role::Foreground)
    }

    pub fn selection(&self) -> Style {
        let style = Style::new(
            self.color(Role::SelectionForeground),
            self.color(Role::SelectionBackground),
        );
        if self.uses_attributes_for_emphasis {
            style.adding(Attributes::REVERSE)
        } else {
            style
        }
    }

    /// Low → mid → high ramp for a 0..1 utilization fraction.
    pub fn gradient(&self, fraction: f64) -> Color {
        let t = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if t <= 0.5 {
            Color::blend(
                self.color(Role::GraphLow),
                self.color(Role::GraphMid),
                t * 2.0,
            )
        } else {
            Color::blend(
                self.color(Role::GraphMid),
                self.color(Role::GraphHigh),
                (t - 0.5) * 2.0,
            )
        }
    }

    /// The default dark theme.
    pub fn harbor() -> Theme {
        builtin_themes::harbor()
    }

    /// No colors at all; emphasis via reverse video.
    pub fn monochrome() -> Theme {
        let mut theme = Theme::blank("monochrome");
        for role in Role::ALL {
            theme.colors.insert(*role, Color::Default);
        }
        theme.uses_attributes_for_emphasis = true;
        theme.border = BorderCharacters::SINGLE;
        theme
    }

    pub fn built_in() -> Vec<Theme> {
        vec![
            builtin_themes::harbor(),
            builtin_themes::daylight(),
            builtin_themes::midnight(),
            builtin_themes::ember(),
            builtin_themes::forest(),
            builtin_themes::glacier(),
            builtin_themes::dusk(),
            builtin_themes::contrast(),
            builtin_themes::ansi(),
            Theme::monochrome(),
        ]
    }

    pub fn named(name: &str) -> Option<Theme> {
        let lowered = name.trim().to_lowercase();
        if lowered == "default" {
            return Some(Theme::harbor());
        }
        Theme::built_in().into_iter().find(|t| t.name == lowered)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ThemeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "theme line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ThemeParseError {}

fn border_name(border: BorderCharacters) -> &'static str {
    match border {
        b if b == BorderCharacters::SINGLE => "single",
        b if b == BorderCharacters::DOUBLE => "double",
        b if b == BorderCharacters::HEAVY => "heavy",
        b if b == BorderCharacters::ASCII => "ascii",
        _ => "rounded",
    }
}

/// Parses the theme format:
///
/// ```text
/// # comment
/// name = ocean
/// inherits = harbor
/// accent = #4fb0f5
/// border_style = rounded
/// ```
pub fn parse_theme_file(
    text: &str,
    fallback_name: &str,
    resolve_base: &dyn Fn(&str) -> Option<Theme>,
) -> Result<Theme, ThemeParseError> {
    let mut theme = Theme::harbor();
    theme.name = fallback_name.to_lowercase();
    let mut assigned: Vec<(Role, Color)> = Vec::new();
    let mut explicit_name = None;
    let mut border = None;
    let mut emphasis = None;
    for (offset, raw) in text.split('\n').enumerate() {
        let line_number = offset + 1;
        let line = raw.trim();
        let error = |message: String| ThemeParseError {
            line: line_number,
            message,
        };
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || line.starts_with("//")
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(error(format!("expected 'key = value', found '{line}'")));
        };
        let key = key.trim().to_lowercase().replace('-', "_");
        let mut value = value.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = &value[1..value.len() - 1];
        }
        match key.as_str() {
            "name" => {
                if value.is_empty() {
                    return Err(error("name must not be empty".into()));
                }
                explicit_name = Some(value.to_lowercase());
            }
            "inherits" | "base" => {
                let base = resolve_base(value)
                    .ok_or_else(|| error(format!("unknown base theme '{value}'")))?;
                let name = theme.name.clone();
                theme = base;
                theme.name = name;
            }
            "border_style" | "borders" => {
                border = Some(BorderCharacters::named(value).ok_or_else(|| {
                    error(format!(
                        "unknown border style '{value}' (rounded, single, double, heavy, ascii)"
                    ))
                })?);
            }
            "emphasis" => {
                emphasis = Some(match value.to_lowercase().as_str() {
                    "attributes" | "reverse" => true,
                    "color" | "colors" => false,
                    _ => return Err(error("emphasis must be 'color' or 'attributes'".into())),
                });
            }
            _ => {
                let role = Role::from_raw(&key)
                    .ok_or_else(|| error(format!("unknown theme key '{key}'")))?;
                let color = Color::parse(value).ok_or_else(|| {
                    error(format!(
                        "invalid color '{value}' (use #rrggbb, color(0-255), ansi:0-15 or a name)"
                    ))
                })?;
                assigned.push((role, color));
            }
        }
    }
    for (role, color) in assigned {
        theme.colors.insert(role, color);
    }
    if let Some(name) = explicit_name {
        theme.name = name;
    }
    if let Some(border) = border {
        theme.border = border;
    }
    if let Some(emphasis) = emphasis {
        theme.uses_attributes_for_emphasis = emphasis;
    }
    Ok(theme)
}

pub fn serialize_theme(theme: &Theme) -> String {
    let mut lines = vec![
        "# dtop theme".to_string(),
        format!("name = {}", theme.name),
        format!("border_style = {}", border_name(theme.border)),
        format!(
            "emphasis = {}",
            if theme.uses_attributes_for_emphasis {
                "attributes"
            } else {
                "color"
            }
        ),
    ];
    for role in Role::ALL {
        lines.push(format!("{} = {}", role.raw_value(), theme.color(*role)));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_ten_complete_built_ins() {
        let themes = Theme::built_in();
        assert_eq!(themes.len(), 10);
        for theme in &themes {
            assert_eq!(
                theme.colors.len(),
                Role::ALL.len(),
                "{} is missing roles",
                theme.name
            );
        }
        assert_eq!(
            Theme::named("default").map(|t| t.name),
            Some("harbor".into())
        );
        assert_eq!(
            Theme::named("glacier").map(|t| t.border),
            Some(BorderCharacters::SINGLE)
        );
    }

    #[test]
    fn parses_and_round_trips_theme_files() {
        let text = "# x\nname = Ocean\ninherits = midnight\naccent = #010203\nborder-style = double\nemphasis = attributes\n";
        let theme = parse_theme_file(text, "fallback", &Theme::named).unwrap();
        assert_eq!(theme.name, "ocean");
        assert_eq!(theme.color(Role::Accent), Color::Rgb(1, 2, 3));
        assert_eq!(
            theme.color(Role::Cpu),
            Theme::named("midnight").unwrap().color(Role::Cpu)
        );
        assert_eq!(theme.border, BorderCharacters::DOUBLE);
        let again = parse_theme_file(&serialize_theme(&theme), "x", &Theme::named).unwrap();
        assert_eq!(again, theme);
    }

    #[test]
    fn reports_line_numbers() {
        let err = parse_theme_file("\naccent = nope", "x", &Theme::named).unwrap_err();
        assert_eq!(err.line, 2);
        assert!(err.to_string().contains("invalid color"));
        assert!(parse_theme_file("bogus = #fff", "x", &Theme::named).is_err());
        assert!(parse_theme_file("just text", "x", &Theme::named).is_err());
    }

    #[test]
    fn gradient_ramps_low_mid_high() {
        let theme = Theme::harbor();
        assert_eq!(theme.gradient(0.0), theme.color(Role::GraphLow));
        assert_eq!(theme.gradient(0.5), theme.color(Role::GraphMid));
        assert_eq!(theme.gradient(1.0), theme.color(Role::GraphHigh));
    }
}
