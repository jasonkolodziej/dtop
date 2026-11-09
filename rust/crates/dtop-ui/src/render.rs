//! Terminal escape sequences and the diff renderer, which emits only changed cells and
//! coalesces cursor moves and style changes.

use crate::buffer::ScreenBuffer;
use crate::color::{ColorMode, Style};

pub mod seq {
    pub const ENTER_ALTERNATE_SCREEN: &str = "\u{1b}[?1049h";
    pub const EXIT_ALTERNATE_SCREEN: &str = "\u{1b}[?1049l";
    pub const HIDE_CURSOR: &str = "\u{1b}[?25l";
    pub const SHOW_CURSOR: &str = "\u{1b}[?25h";
    pub const CLEAR_SCREEN: &str = "\u{1b}[2J";
    pub const HOME: &str = "\u{1b}[H";
    pub const RESET: &str = "\u{1b}[0m";
    /// Button events, drag tracking and SGR extended coordinates (no 223-column limit).
    pub const ENABLE_MOUSE: &str = "\u{1b}[?1000h\u{1b}[?1002h\u{1b}[?1006h";
    pub const DISABLE_MOUSE: &str = "\u{1b}[?1006l\u{1b}[?1002l\u{1b}[?1000l";
    pub const ENABLE_BRACKETED_PASTE: &str = "\u{1b}[?2004h";
    pub const DISABLE_BRACKETED_PASTE: &str = "\u{1b}[?2004l";
    pub const DISABLE_LINE_WRAP: &str = "\u{1b}[?7l";
    pub const ENABLE_LINE_WRAP: &str = "\u{1b}[?7h";

    pub fn move_to(x: i32, y: i32) -> String {
        format!("\u{1b}[{};{}H", y + 1, x + 1)
    }

    pub fn set_title(title: &str) -> String {
        let safe: String = title
            .chars()
            .filter(|c| *c >= ' ' && *c != '\u{7f}')
            .collect();
        format!("\u{1b}]0;{safe}\u{7}")
    }
}

#[derive(Debug, Clone)]
pub struct FrameRenderer {
    pub color_mode: ColorMode,
    previous: Option<ScreenBuffer>,
}

impl FrameRenderer {
    pub fn new(color_mode: ColorMode) -> Self {
        FrameRenderer {
            color_mode,
            previous: None,
        }
    }

    /// Forces the next frame to repaint everything (after resize or external output).
    pub fn invalidate(&mut self) {
        self.previous = None;
    }

    pub fn render(&mut self, next: ScreenBuffer) -> String {
        let output = diff(self.previous.as_ref(), &next, self.color_mode);
        self.previous = Some(next);
        output
    }
}

pub fn diff(previous: Option<&ScreenBuffer>, next: &ScreenBuffer, mode: ColorMode) -> String {
    let previous = previous.filter(|p| p.width == next.width && p.height == next.height);
    let full = previous.is_none();
    let mut out = String::new();
    if full {
        out.push_str(seq::RESET);
        out.push_str(seq::CLEAR_SCREEN);
    }
    let mut cursor: Option<(i32, i32)> = None;
    let mut current: Option<Style> = None;
    for y in 0..next.height {
        let mut x = 0;
        while x < next.width {
            let cell = next.get(x, y);
            let changed = previous.map_or(true, |p| p.get(x, y) != cell);
            if !changed {
                x += 1;
                continue;
            }
            // A changed continuation cell means its wide character must be redrawn.
            let (draw_x, draw_cell) = if cell.is_continuation && x > 0 {
                (x - 1, next.get(x - 1, y))
            } else {
                (x, cell)
            };
            if draw_cell.is_continuation {
                x += 1;
                continue;
            }
            if cursor != Some((draw_x, y)) {
                out.push_str(&seq::move_to(draw_x, y));
            }
            if current != Some(draw_cell.style) {
                out.push_str(&draw_cell.style.sgr(mode));
                current = Some(draw_cell.style);
            }
            draw_cell.glyph.push_to(&mut out);
            let advance = draw_cell.glyph.width().max(1) as i32;
            cursor = Some((draw_x + advance, y));
            x = draw_x + advance;
        }
    }
    if current.is_some() {
        out.push_str(seq::RESET);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changed_cells_are_emitted() {
        let mut renderer = FrameRenderer::new(ColorMode::None);
        let mut a = ScreenBuffer::new(5, 1);
        a.write("hello", 0, 0, Style::PLAIN);
        assert!(renderer.render(a.clone()).contains("hello"));
        let mut b = a.clone();
        b.write("J", 0, 0, Style::PLAIN);
        assert_eq!(renderer.render(b.clone()), "\u{1b}[1;1H\u{1b}[0mJ\u{1b}[0m");
        assert_eq!(renderer.render(b), "");
    }

    #[test]
    fn wide_characters_are_redrawn_whole() {
        let mut a = ScreenBuffer::new(4, 1);
        a.write("日x", 0, 0, Style::PLAIN);
        let mut b = ScreenBuffer::new(4, 1);
        b.write("本x", 0, 0, Style::PLAIN);
        let out = diff(Some(&a), &b, ColorMode::None);
        assert!(out.contains('本'));
        assert!(!out.contains('x'));
    }

    #[test]
    fn resize_forces_full_redraw() {
        let a = ScreenBuffer::new(2, 1);
        let b = ScreenBuffer::new(3, 1);
        assert!(diff(Some(&a), &b, ColorMode::None).contains(seq::CLEAR_SCREEN));
    }
}
