//! The interactive theme menu. Every change goes to `draft`, which the whole UI renders with
//! while the menu is open. Nothing touches disk until the user saves; closing without saving
//! leaves the original theme in place.

use dtop_ui::buffer::Rect;
use dtop_ui::{
    BorderCharacters, Color, InputEvent, Key, Modifiers, MouseAction, MouseButton, Role, Theme,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Base,
    Border,
    Emphasis,
    Role(Role),
}

impl Row {
    pub fn label(self) -> String {
        match self {
            Row::Base => "Theme".into(),
            Row::Border => "Borders".into(),
            Row::Emphasis => "Emphasis".into(),
            Row::Role(role) => role.raw_value().replace('_', " "),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Red,
    Green,
    Blue,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Red => "R",
            Channel::Green => "G",
            Channel::Blue => "B",
        }
    }

    fn rotate(self, forward: bool) -> Channel {
        match (self, forward) {
            (Channel::Red, true) | (Channel::Blue, false) => Channel::Green,
            (Channel::Green, true) | (Channel::Red, false) => Channel::Blue,
            (Channel::Blue, true) | (Channel::Green, false) => Channel::Red,
        }
    }
}

/// What the bottom input line is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    None,
    ColorInput(String),
    SaveName(String),
    ConfirmDiscard,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Editing,
    Cancelled,
    Save { name: String, theme: Theme },
}

pub const SMALL_STEP: i32 = 8;
pub const LARGE_STEP: i32 = 32;

pub fn rows() -> Vec<Row> {
    let mut rows = vec![Row::Base, Row::Border, Row::Emphasis];
    rows.extend(Role::ALL.iter().map(|r| Row::Role(*r)));
    rows
}

pub const BORDER_STYLES: [(&str, BorderCharacters); 5] = [
    ("rounded", BorderCharacters::ROUNDED),
    ("single", BorderCharacters::SINGLE),
    ("double", BorderCharacters::DOUBLE),
    ("heavy", BorderCharacters::HEAVY),
    ("ascii", BorderCharacters::ASCII),
];

fn is_built_in_name(name: &str) -> bool {
    Theme::built_in().iter().any(|t| t.name == name)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEditor {
    /// The theme in effect when the editor opened; restored on cancel.
    pub original: Theme,
    /// Themes the "Theme" row switches between.
    pub available: Vec<Theme>,
    /// The theme the draft started from; `d` resets a row to its value.
    pub base: Theme,
    pub draft: Theme,
    pub selected: usize,
    pub channel: Channel,
    pub field: Field,
    pub message: Option<String>,
}

impl ThemeEditor {
    pub fn new(current: Theme, available: Vec<Theme>) -> Self {
        let mut themes = available;
        if !themes.iter().any(|t| t.name == current.name) {
            themes.insert(0, current.clone());
        }
        let base = themes
            .iter()
            .find(|t| t.name == current.name)
            .cloned()
            .unwrap_or_else(|| current.clone());
        ThemeEditor {
            original: current.clone(),
            available: themes,
            base,
            draft: current,
            selected: 0,
            channel: Channel::Red,
            field: Field::None,
            message: None,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.draft != self.original
    }

    pub fn selected_row(&self) -> Row {
        rows()[self.selected]
    }

    pub fn default_save_name(&self) -> String {
        if !is_built_in_name(&self.draft.name) {
            return self.draft.name.clone();
        }
        if self.draft == self.base {
            self.base.name.clone()
        } else {
            format!("{}-custom", self.base.name)
        }
    }

    pub fn handle(&mut self, event: &InputEvent, row_hits: &[(usize, Rect)]) -> Outcome {
        match self.field.clone() {
            Field::None => self.handle_navigation(event, row_hits),
            Field::ColorInput(text) => self.handle_color_input(event, &text),
            Field::SaveName(text) => self.handle_save_name(event, &text),
            Field::ConfirmDiscard => self.handle_confirm_discard(event),
        }
    }

    fn handle_navigation(&mut self, event: &InputEvent, row_hits: &[(usize, Rect)]) -> Outcome {
        match event {
            InputEvent::Key(key) => {
                let big = key.modifiers.contains(Modifiers::SHIFT);
                match key.key {
                    Key::Up | Key::Char('k') => self.select(self.selected as i64 - 1),
                    Key::Down | Key::Char('j') => self.select(self.selected as i64 + 1),
                    Key::PageUp => self.select(self.selected as i64 - 10),
                    Key::PageDown => self.select(self.selected as i64 + 10),
                    Key::Home | Key::Char('g') => self.select(0),
                    Key::End | Key::Char('G') => self.select(rows().len() as i64 - 1),
                    Key::Left | Key::Char('h') => self.adjust(-1, big),
                    Key::Right | Key::Char('l') => self.adjust(1, big),
                    Key::Char('H') => self.adjust(-1, true),
                    Key::Char('L') => self.adjust(1, true),
                    Key::Tab => self.channel = self.channel.rotate(true),
                    Key::BackTab => self.channel = self.channel.rotate(false),
                    Key::Enter => {
                        if let Row::Role(role) = self.selected_row() {
                            self.field = Field::ColorInput(self.draft.color(role).to_string());
                            self.message = None;
                        } else {
                            self.adjust(1, false);
                        }
                    }
                    Key::Char('d') | Key::Delete | Key::Backspace => self.reset_selected_row(),
                    Key::Char('r') => {
                        self.draft = self.original.clone();
                        self.base = self
                            .available
                            .iter()
                            .find(|t| t.name == self.original.name)
                            .cloned()
                            .unwrap_or_else(|| self.original.clone());
                        self.message = Some("All changes reverted".into());
                    }
                    Key::Char('s') => {
                        self.field = Field::SaveName(self.default_save_name());
                        self.message = None;
                    }
                    Key::Escape | Key::Char('q') => {
                        if self.is_dirty() {
                            self.field = Field::ConfirmDiscard;
                        } else {
                            return Outcome::Cancelled;
                        }
                    }
                    _ => {}
                }
            }
            InputEvent::Mouse(mouse) => match mouse.button {
                MouseButton::WheelUp => self.select(self.selected as i64 - 1),
                MouseButton::WheelDown => self.select(self.selected as i64 + 1),
                MouseButton::Left if mouse.action == MouseAction::Press => {
                    if let Some((index, _)) =
                        row_hits.iter().find(|(_, r)| r.contains(mouse.x, mouse.y))
                    {
                        if *index == self.selected {
                            self.adjust(1, false);
                        } else {
                            self.select(*index as i64);
                        }
                    }
                }
                _ => {}
            },
            InputEvent::Paste(text) => {
                if matches!(self.selected_row(), Row::Role(_)) {
                    self.field = Field::ColorInput(text.trim().to_string());
                }
            }
            InputEvent::Focus(_) => {}
        }
        Outcome::Editing
    }

    fn handle_color_input(&mut self, event: &InputEvent, text: &str) -> Outcome {
        match event {
            InputEvent::Key(key) => match key.key {
                Key::Escape => self.field = Field::None,
                Key::Enter => {
                    let Row::Role(role) = self.selected_row() else {
                        self.field = Field::None;
                        return Outcome::Editing;
                    };
                    match Color::parse(text) {
                        Some(color) => {
                            self.draft.colors.insert(role, color);
                            self.field = Field::None;
                            self.message = None;
                        }
                        None => {
                            self.message =
                                Some(format!("'{text}' is not a color — try #4fb0f5, rgb(1,2,3), color(42), red or default"));
                        }
                    }
                }
                Key::Backspace => {
                    let mut t = text.to_string();
                    t.pop();
                    self.field = Field::ColorInput(t);
                }
                Key::Char(c) if key.modifiers.contains(Modifiers::CTRL) => {
                    if c == 'u' {
                        self.field = Field::ColorInput(String::new());
                    }
                }
                Key::Char(c)
                    if !key.modifiers.contains(Modifiers::ALT) && text.chars().count() < 32 =>
                {
                    self.field = Field::ColorInput(format!("{text}{c}"));
                }
                _ => {}
            },
            InputEvent::Paste(pasted) => {
                let cleaned: String = pasted
                    .chars()
                    .filter(|c| *c != '\n' && *c != '\r')
                    .collect();
                self.field = Field::ColorInput(format!("{text}{cleaned}"));
            }
            _ => {}
        }
        Outcome::Editing
    }

    fn handle_save_name(&mut self, event: &InputEvent, text: &str) -> Outcome {
        match event {
            InputEvent::Key(key) => match key.key {
                Key::Escape => self.field = Field::None,
                Key::Enter => {
                    if let Some(problem) = validate_name(text, &self.draft) {
                        self.message = Some(problem);
                        return Outcome::Editing;
                    }
                    let mut theme = self.draft.clone();
                    theme.name = text.to_string();
                    self.message = None;
                    return Outcome::Save {
                        name: text.to_string(),
                        theme,
                    };
                }
                Key::Backspace => {
                    let mut t = text.to_string();
                    t.pop();
                    self.field = Field::SaveName(t);
                }
                Key::Char(c) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
                    let lowered = c.to_ascii_lowercase();
                    if is_name_character(lowered) && text.chars().count() < 40 {
                        self.field = Field::SaveName(format!("{text}{lowered}"));
                    }
                }
                _ => {}
            },
            InputEvent::Paste(pasted) => {
                let cleaned: String = pasted
                    .to_lowercase()
                    .chars()
                    .filter(|c| is_name_character(*c))
                    .collect();
                self.field = Field::SaveName(format!("{text}{cleaned}").chars().take(40).collect());
            }
            _ => {}
        }
        Outcome::Editing
    }

    fn handle_confirm_discard(&mut self, event: &InputEvent) -> Outcome {
        let InputEvent::Key(key) = event else {
            return Outcome::Editing;
        };
        match key.key {
            Key::Char('y' | 'Y') => return Outcome::Cancelled,
            Key::Char('n' | 'N') | Key::Escape => self.field = Field::None,
            Key::Char('s') => self.field = Field::SaveName(self.default_save_name()),
            _ => {}
        }
        Outcome::Editing
    }

    pub fn select(&mut self, index: i64) {
        self.selected = index.clamp(0, rows().len() as i64 - 1) as usize;
        self.message = None;
    }

    pub fn adjust(&mut self, direction: i32, big: bool) {
        self.message = None;
        match self.selected_row() {
            Row::Base => {
                if self.available.is_empty() {
                    return;
                }
                let n = self.available.len() as i32;
                let current = self
                    .available
                    .iter()
                    .position(|t| t.name == self.base.name)
                    .unwrap_or(0) as i32;
                self.base = self.available[((current + direction + n) % n) as usize].clone();
                self.draft = self.base.clone();
            }
            Row::Border => {
                let n = BORDER_STYLES.len() as i32;
                let current = BORDER_STYLES
                    .iter()
                    .position(|(_, b)| *b == self.draft.border)
                    .unwrap_or(0) as i32;
                self.draft.border = BORDER_STYLES[((current + direction + n) % n) as usize].1;
            }
            Row::Emphasis => {
                self.draft.uses_attributes_for_emphasis = !self.draft.uses_attributes_for_emphasis
            }
            Row::Role(role) => {
                let fallback = self
                    .draft
                    .color(Role::Foreground)
                    .rgb_components()
                    .unwrap_or((128, 128, 128));
                let (mut r, mut g, mut b) =
                    self.draft.color(role).rgb_components().unwrap_or(fallback);
                let step = if big { LARGE_STEP } else { SMALL_STEP } * direction;
                let moved = |v: u8| (i32::from(v) + step).clamp(0, 255) as u8;
                match self.channel {
                    Channel::Red => r = moved(r),
                    Channel::Green => g = moved(g),
                    Channel::Blue => b = moved(b),
                }
                self.draft.colors.insert(role, Color::Rgb(r, g, b));
            }
        }
    }

    pub fn reset_selected_row(&mut self) {
        match self.selected_row() {
            Row::Base => self.draft = self.base.clone(),
            Row::Border => self.draft.border = self.base.border,
            Row::Emphasis => {
                self.draft.uses_attributes_for_emphasis = self.base.uses_attributes_for_emphasis
            }
            Row::Role(role) => {
                self.draft.colors.insert(role, self.base.color(role));
            }
        }
        self.message = None;
    }

    /// Text for a row's current value.
    pub fn value_text(&self, row: Row) -> String {
        match row {
            Row::Base => format!("‹ {} ›", self.base.name),
            Row::Border => {
                format!(
                    "‹ {} ›",
                    BORDER_STYLES
                        .iter()
                        .find(|(_, b)| *b == self.draft.border)
                        .map_or("custom", |(n, _)| n)
                )
            }
            Row::Emphasis => if self.draft.uses_attributes_for_emphasis {
                "‹ attributes ›"
            } else {
                "‹ color ›"
            }
            .into(),
            Row::Role(role) => self.draft.color(role).to_string(),
        }
    }

    pub fn is_modified(&self, row: Row) -> bool {
        match row {
            Row::Base => self.base.name != self.original.name,
            Row::Border => self.draft.border != self.original.border,
            Row::Emphasis => {
                self.draft.uses_attributes_for_emphasis
                    != self.original.uses_attributes_for_emphasis
            }
            Row::Role(role) => self.draft.color(role) != self.original.color(role),
        }
    }
}

pub fn is_name_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Theme names become file names: keep them simple and never shadow a built-in theme.
pub fn validate_name(name: &str, draft: &Theme) -> Option<String> {
    if name.is_empty() {
        return Some("Enter a name for the theme".into());
    }
    if !name.chars().all(is_name_character) {
        return Some("Use only letters, digits, - and _".into());
    }
    if name == "default" {
        return Some("'default' is reserved — pick another name".into());
    }
    if let Some(built_in) = Theme::named(name).filter(|t| t.name == name) {
        let mut unchanged = draft.clone();
        unchanged.name = built_in.name.clone();
        if unchanged != built_in {
            return Some(format!(
                "'{name}' is a built-in theme — save your changes under a new name"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtop_ui::KeyEvent;

    fn key(k: Key) -> InputEvent {
        InputEvent::Key(KeyEvent::new(k))
    }

    fn editor() -> ThemeEditor {
        ThemeEditor::new(Theme::harbor(), Theme::built_in())
    }

    #[test]
    fn nudges_channels_and_reverts() {
        let mut e = editor();
        e.select(3 + Role::ALL.iter().position(|r| *r == Role::Accent).unwrap() as i64);
        let before = e.draft.color(Role::Accent).rgb_components().unwrap();
        e.handle(&key(Key::Right), &[]);
        assert_eq!(
            e.draft.color(Role::Accent).rgb_components().unwrap().0,
            before.0 + 8
        );
        assert!(e.is_dirty());
        e.handle(&key(Key::Char('d')), &[]);
        assert!(!e.is_dirty());
    }

    #[test]
    fn typed_colors_and_save_names() {
        let mut e = editor();
        e.select(3);
        e.handle(&key(Key::Enter), &[]);
        e.field = Field::ColorInput(String::new());
        for c in "#010203".chars() {
            e.handle(&key(Key::Char(c)), &[]);
        }
        e.handle(&key(Key::Enter), &[]);
        assert_eq!(e.draft.color(Role::Background), Color::Rgb(1, 2, 3));
        e.handle(&key(Key::Char('s')), &[]);
        assert_eq!(e.field, Field::SaveName("harbor-custom".into()));
        assert!(matches!(
            e.handle(&key(Key::Enter), &[]),
            Outcome::Save { .. }
        ));
    }

    #[test]
    fn built_in_names_cannot_be_overwritten_with_changes() {
        let mut draft = Theme::harbor();
        draft.colors.insert(Role::Accent, Color::Rgb(0, 0, 0));
        assert!(validate_name("harbor", &draft).is_some());
        assert!(validate_name("harbor", &Theme::harbor()).is_none());
        assert!(validate_name("bad name", &draft).is_some());
        assert!(validate_name("default", &draft).is_some());
    }

    #[test]
    fn dirty_escape_asks_before_discarding() {
        let mut e = editor();
        e.select(2);
        e.handle(&key(Key::Right), &[]);
        assert_eq!(e.handle(&key(Key::Escape), &[]), Outcome::Editing);
        assert_eq!(e.field, Field::ConfirmDiscard);
        assert_eq!(e.handle(&key(Key::Char('y')), &[]), Outcome::Cancelled);
    }
}
