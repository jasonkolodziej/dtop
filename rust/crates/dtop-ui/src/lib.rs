//! Terminal UI toolkit for dtop: colors, a screen buffer with wide-character handling, a diff
//! renderer, layout, widgets, themes, an input parser and terminal I/O. It does not depend on
//! `dtop-core`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod buffer;
mod builtin_themes;
pub mod color;
pub mod input;
pub mod render;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod widgets;

pub use buffer::{Axis, Cell, Constraint, Glyph, Rect, ScreenBuffer};
pub use color::{Attributes, Color, ColorMode, Style};
pub use input::{
    InputEvent, InputParser, Key, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
};
pub use terminal::{Terminal, TerminalError, TerminalSize};
pub use text::Alignment;
pub use theme::{Role, Theme};
pub use widgets::BorderCharacters;
