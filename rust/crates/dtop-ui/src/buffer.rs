//! Rectangles, the layout solver, and the screen buffer views draw into.

use std::fmt;

use crate::color::Style;
use crate::text::{cluster_width, clusters};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width: width.max(0),
            height: height.max(0),
        }
    }

    pub fn max_x(&self) -> i32 {
        self.x + self.width
    }

    pub fn max_y(&self) -> i32 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.max_x() && py < self.max_y()
    }

    pub fn intersection(&self, other: &Rect) -> Rect {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.max_x().min(other.max_x());
        let bottom = self.max_y().min(other.max_y());
        if right <= left || bottom <= top {
            return Rect::new(left, top, 0, 0);
        }
        Rect::new(left, top, right - left, bottom - top)
    }

    pub fn inset(&self, amount: i32) -> Rect {
        self.inset_edges(amount, amount, amount, amount)
    }

    pub fn inset_edges(&self, top: i32, left: i32, bottom: i32, right: i32) -> Rect {
        Rect::new(
            self.x + left,
            self.y + top,
            self.width - left - right,
            self.height - top - bottom,
        )
    }

    /// A rect of the given size centered inside this one (clamped to fit).
    pub fn centered(&self, w: i32, h: i32) -> Rect {
        let (cw, ch) = (w.min(self.width), h.min(self.height));
        Rect::new(
            self.x + (self.width - cw) / 2,
            self.y + (self.height - ch) / 2,
            cw,
            ch,
        )
    }

    pub fn row(&self, offset: i32) -> Rect {
        Rect::new(
            self.x,
            self.y + offset,
            self.width,
            if offset >= 0 && offset < self.height {
                1
            } else {
                0
            },
        )
    }

    /// Splits along an axis. Fixed and percent sizes are honoured first (shrinking from the last
    /// constraint if they overflow); the remainder is shared by fill weights.
    pub fn split(&self, axis: Axis, constraints: &[Constraint], spacing: i32) -> Vec<Rect> {
        if constraints.is_empty() {
            return Vec::new();
        }
        let length = if axis == Axis::Horizontal {
            self.width
        } else {
            self.height
        };
        let mut offset = 0;
        Constraint::solve(constraints, length, spacing)
            .into_iter()
            .map(|size| {
                let rect = match axis {
                    Axis::Horizontal => Rect::new(self.x + offset, self.y, size, self.height),
                    Axis::Vertical => Rect::new(self.x, self.y + offset, self.width, size),
                };
                offset += size + spacing;
                rect
            })
            .collect()
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({},{} {}x{})", self.x, self.y, self.width, self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    Fixed(i32),
    Percent(i32),
    Fill(i32),
}

impl Constraint {
    pub fn solve(constraints: &[Constraint], length: i32, spacing: i32) -> Vec<i32> {
        let available = (length - spacing.max(0) * (constraints.len() as i32 - 1).max(0)).max(0);
        let mut sizes: Vec<i32> = constraints
            .iter()
            .map(|c| match c {
                Constraint::Fixed(v) => (*v).max(0),
                Constraint::Percent(v) => (available * (*v).clamp(0, 100) / 100).max(0),
                Constraint::Fill(_) => 0,
            })
            .collect();
        let mut overflow = sizes.iter().sum::<i32>() - available;
        for size in sizes.iter_mut().rev() {
            if overflow <= 0 {
                break;
            }
            let cut = overflow.min(*size);
            *size -= cut;
            overflow -= cut;
        }
        let remaining = (available - sizes.iter().sum::<i32>()).max(0);
        let weights: Vec<i32> = constraints
            .iter()
            .map(|c| {
                if let Constraint::Fill(w) = c {
                    (*w).max(0)
                } else {
                    0
                }
            })
            .collect();
        let total: i32 = weights.iter().sum();
        if total <= 0 || remaining <= 0 {
            return sizes;
        }
        let mut distributed = 0;
        for (i, w) in weights.iter().enumerate().filter(|(_, w)| **w > 0) {
            let share = remaining * w / total;
            sizes[i] += share;
            distributed += share;
        }
        // Hand out the rounding remainder one column at a time, first fills first.
        let mut leftover = remaining - distributed;
        for (i, _) in weights.iter().enumerate().filter(|(_, w)| **w > 0) {
            if leftover <= 0 {
                break;
            }
            sizes[i] += 1;
            leftover -= 1;
        }
        sizes
    }
}

/// What a cell shows: a single character (no allocation) or a multi-scalar cluster.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Glyph {
    Char(char),
    Cluster(Box<str>),
}

impl Glyph {
    pub fn from_cluster(cluster: &str) -> Glyph {
        let mut chars = cluster.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Glyph::Char(c),
            _ => Glyph::Cluster(cluster.into()),
        }
    }

    pub fn width(&self) -> usize {
        match self {
            Glyph::Char(c) => crate::text::char_width(*c),
            Glyph::Cluster(s) => cluster_width(s),
        }
    }

    pub fn push_to(&self, out: &mut String) {
        match self {
            Glyph::Char(c) => out.push(*c),
            Glyph::Cluster(s) => out.push_str(s),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    pub glyph: Glyph,
    pub style: Style,
    /// The right half of a double-width character; renderers skip these.
    pub is_continuation: bool,
}

impl Cell {
    pub const BLANK: Cell = Cell {
        glyph: Glyph::Char(' '),
        style: Style::PLAIN,
        is_continuation: false,
    };

    pub fn new(c: char, style: Style) -> Cell {
        Cell {
            glyph: Glyph::Char(c),
            style,
            is_continuation: false,
        }
    }
}

/// An in-memory grid of styled cells. Views draw into it; the renderer diffs consecutive
/// buffers. Out-of-bounds writes are clipped silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenBuffer {
    pub width: i32,
    pub height: i32,
    pub cells: Vec<Cell>,
}

impl ScreenBuffer {
    pub fn new(width: i32, height: i32) -> ScreenBuffer {
        let (w, h) = (width.max(0), height.max(0));
        ScreenBuffer {
            width: w,
            height: h,
            cells: vec![Cell::BLANK; (w * h) as usize],
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && x < self.width && y < self.height
    }

    pub fn get(&self, x: i32, y: i32) -> &Cell {
        if !self.contains(x, y) {
            return &Cell::BLANK;
        }
        &self.cells[(y * self.width + x) as usize]
    }

    pub fn set(&mut self, x: i32, y: i32, cell: Cell) {
        if self.contains(x, y) {
            let index = (y * self.width + x) as usize;
            self.cells[index] = cell;
        }
    }

    /// Places one glyph, keeping wide characters and their continuation cells consistent.
    pub fn put_glyph(&mut self, glyph: Glyph, x: i32, y: i32, style: Style, clip: Option<Rect>) {
        let area = clip.map_or_else(|| self.bounds(), |c| c.intersection(&self.bounds()));
        if !area.contains(x, y) {
            return;
        }
        let w = glyph.width();
        if w == 0 {
            return;
        }
        if w == 2 && !area.contains(x + 1, y) {
            // No room for the wide glyph; draw a placeholder instead of half a character.
            self.put(' ', x, y, style);
            return;
        }
        self.break_wide_character(x, y);
        if w == 2 {
            self.break_wide_character(x + 1, y);
        }
        self.set(
            x,
            y,
            Cell {
                glyph,
                style,
                is_continuation: false,
            },
        );
        if w == 2 {
            self.set(
                x + 1,
                y,
                Cell {
                    glyph: Glyph::Char(' '),
                    style,
                    is_continuation: true,
                },
            );
        }
    }

    pub fn put(&mut self, c: char, x: i32, y: i32, style: Style) {
        self.put_glyph(Glyph::Char(c), x, y, style, None);
    }

    /// If (x, y) is half of a wide character, blank the other half so no orphan remains.
    fn break_wide_character(&mut self, x: i32, y: i32) {
        if !self.contains(x, y) {
            return;
        }
        let cell = self.get(x, y).clone();
        if cell.is_continuation && self.contains(x - 1, y) {
            let style = self.get(x - 1, y).style;
            self.set(x - 1, y, Cell::new(' ', style));
        } else if cell.glyph.width() == 2
            && self.contains(x + 1, y)
            && self.get(x + 1, y).is_continuation
        {
            let style = self.get(x + 1, y).style;
            self.set(x + 1, y, Cell::new(' ', style));
        }
    }

    /// Writes text starting at (x, y), returning how many columns landed on screen.
    pub fn write(&mut self, text: &str, x: i32, y: i32, style: Style) -> i32 {
        self.write_clipped(text, x, y, style, None, None)
    }

    pub fn write_max(&mut self, text: &str, x: i32, y: i32, style: Style, max_width: i32) -> i32 {
        self.write_clipped(text, x, y, style, Some(max_width), None)
    }

    pub fn write_clipped(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        style: Style,
        max_width: Option<i32>,
        clip: Option<Rect>,
    ) -> i32 {
        let area = clip.map_or_else(|| self.bounds(), |c| c.intersection(&self.bounds()));
        let limit = max_width
            .map_or(i32::MAX, |m| x + m.max(0))
            .min(x.max(area.max_x()));
        let mut column = x;
        for cluster in clusters(text) {
            let w = cluster_width(cluster) as i32;
            if w == 0 {
                continue;
            }
            if column + w > limit {
                break;
            }
            self.put_glyph(Glyph::from_cluster(cluster), column, y, style, clip);
            column += w;
        }
        column - x
    }

    pub fn fill(&mut self, rect: Rect, c: char, style: Style) {
        let area = rect.intersection(&self.bounds());
        for y in area.y..area.max_y() {
            for x in area.x..area.max_x() {
                self.set(x, y, Cell::new(c, style));
            }
        }
    }

    /// Re-styles cells in a rect (e.g. a selection highlight over drawn text).
    pub fn restyle(&mut self, rect: Rect, transform: impl Fn(Style) -> Style) {
        let area = rect.intersection(&self.bounds());
        for y in area.y..area.max_y() {
            for x in area.x..area.max_x() {
                let index = (y * self.width + x) as usize;
                self.cells[index].style = transform(self.cells[index].style);
            }
        }
    }

    /// Plain text of one row (wide characters appear once).
    pub fn text_row(&self, y: i32) -> String {
        let mut line = String::new();
        if y < 0 || y >= self.height {
            return line;
        }
        for x in 0..self.width {
            let cell = self.get(x, y);
            if !cell.is_continuation {
                cell.glyph.push_to(&mut line);
            }
        }
        line
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.height).map(|y| self.text_row(y)).collect()
    }

    /// The whole buffer as text with trailing spaces trimmed per line.
    pub fn plain_text(&self) -> String {
        self.lines()
            .iter()
            .map(|l| l.trim_end_matches(' ').to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_layouts() {
        use Constraint::*;
        assert_eq!(
            Constraint::solve(&[Fixed(3), Fill(1), Fill(1)], 10, 0),
            vec![3, 4, 3]
        );
        assert_eq!(
            Constraint::solve(&[Percent(50), Fill(1)], 11, 1),
            vec![5, 5]
        );
        assert_eq!(Constraint::solve(&[Fixed(8), Fixed(8)], 10, 0), vec![8, 2]);
        let rects = Rect::new(0, 0, 10, 4).split(Axis::Vertical, &[Fixed(1), Fill(1)], 0);
        assert_eq!(rects[1], Rect::new(0, 1, 10, 3));
    }

    #[test]
    fn wide_characters_keep_continuations_consistent() {
        let mut buffer = ScreenBuffer::new(4, 1);
        buffer.write("日本", 0, 0, Style::PLAIN);
        assert_eq!(buffer.text_row(0), "日本");
        buffer.put('x', 1, 0, Style::PLAIN);
        assert_eq!(buffer.text_row(0), " x本");
        let mut narrow = ScreenBuffer::new(3, 1);
        assert_eq!(narrow.write("日本", 0, 0, Style::PLAIN), 2);
        assert_eq!(narrow.text_row(0), "日 ");
    }

    #[test]
    fn writes_are_clipped() {
        let mut buffer = ScreenBuffer::new(5, 2);
        assert_eq!(buffer.write("hello world", 2, 0, Style::PLAIN), 3);
        assert_eq!(buffer.write_max("abcdef", 0, 1, Style::PLAIN, 2), 2);
        assert_eq!(buffer.plain_text(), "  hel\nab");
        buffer.write("zz", -1, 5, Style::PLAIN);
    }

    #[test]
    fn rect_math() {
        let r = Rect::new(0, 0, 10, 10);
        assert_eq!(
            r.intersection(&Rect::new(5, 5, 10, 10)),
            Rect::new(5, 5, 5, 5)
        );
        assert_eq!(r.centered(4, 2), Rect::new(3, 4, 4, 2));
        assert!(r.intersection(&Rect::new(20, 20, 1, 1)).is_empty());
    }
}
