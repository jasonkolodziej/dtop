//! Boxes, gauges, braille graphs, sparklines and tables.

use crate::buffer::{Rect, ScreenBuffer};
use crate::color::{Color, Style};
use crate::text::{self, Alignment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderCharacters {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
}

impl BorderCharacters {
    pub const ROUNDED: BorderCharacters = BorderCharacters {
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
        horizontal: '─',
        vertical: '│',
    };
    pub const SINGLE: BorderCharacters = BorderCharacters {
        top_left: '┌',
        top_right: '┐',
        bottom_left: '└',
        bottom_right: '┘',
        horizontal: '─',
        vertical: '│',
    };
    pub const DOUBLE: BorderCharacters = BorderCharacters {
        top_left: '╔',
        top_right: '╗',
        bottom_left: '╚',
        bottom_right: '╝',
        horizontal: '═',
        vertical: '║',
    };
    pub const HEAVY: BorderCharacters = BorderCharacters {
        top_left: '┏',
        top_right: '┓',
        bottom_left: '┗',
        bottom_right: '┛',
        horizontal: '━',
        vertical: '┃',
    };
    /// For terminals or fonts without box-drawing glyphs.
    pub const ASCII: BorderCharacters = BorderCharacters {
        top_left: '+',
        top_right: '+',
        bottom_left: '+',
        bottom_right: '+',
        horizontal: '-',
        vertical: '|',
    };

    pub fn named(name: &str) -> Option<BorderCharacters> {
        match name.trim().to_lowercase().as_str() {
            "rounded" => Some(Self::ROUNDED),
            "single" | "square" => Some(Self::SINGLE),
            "double" => Some(Self::DOUBLE),
            "heavy" | "thick" => Some(Self::HEAVY),
            "ascii" => Some(Self::ASCII),
            _ => None,
        }
    }
}

/// Draws a border with an optional title (left) and label (right) in the top edge; returns the
/// inner rect.
pub fn draw_box(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    title: Option<&str>,
    label: Option<&str>,
    border: Style,
    title_style: Option<Style>,
    chars: BorderCharacters,
) -> Rect {
    if rect.width < 2 || rect.height < 2 {
        return Rect::ZERO;
    }
    let right = rect.max_x() - 1;
    let bottom = rect.max_y() - 1;
    buffer.put(chars.top_left, rect.x, rect.y, border);
    buffer.put(chars.top_right, right, rect.y, border);
    buffer.put(chars.bottom_left, rect.x, bottom, border);
    buffer.put(chars.bottom_right, right, bottom, border);
    for x in rect.x + 1..right {
        buffer.put(chars.horizontal, x, rect.y, border);
        buffer.put(chars.horizontal, x, bottom, border);
    }
    for y in rect.y + 1..bottom {
        buffer.put(chars.vertical, rect.x, y, border);
        buffer.put(chars.vertical, right, y, border);
    }
    let inner_width = rect.width - 4;
    let mut title_end = rect.x + 1;
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        if inner_width > 0 {
            let text = format!(
                " {} ",
                text::truncate(title, (inner_width - 2).max(1) as usize)
            );
            title_end = rect.x
                + 2
                + buffer.write_max(
                    &text,
                    rect.x + 2,
                    rect.y,
                    title_style.unwrap_or(border),
                    inner_width,
                );
        }
    }
    if let Some(label) = label.filter(|l| !l.is_empty()) {
        let available = right - 1 - title_end - 1;
        if available > 2 {
            let text = format!(" {} ", text::truncate(label, (available - 2) as usize));
            let start = right - 1 - text::width(&text) as i32;
            buffer.write(&text, start, rect.y, border);
        }
    }
    rect.inset(1)
}

const PARTIALS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

/// A horizontal meter with eighth-block resolution: `█████▍    `.
pub fn gauge_bar(fraction: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let clamped = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let eighths = (clamped * (width * 8) as f64).round() as usize;
    let (full, remainder) = (eighths / 8, eighths % 8);
    let mut text = "█".repeat(full);
    if full < width {
        text.push(PARTIALS[remainder]);
        text.push_str(&" ".repeat(width - full - 1));
    }
    text
}

/// Draws a gauge; empty track cells show as `·`.
pub fn draw_gauge(
    buffer: &mut ScreenBuffer,
    x: i32,
    y: i32,
    width: i32,
    fraction: f64,
    filled: Style,
    track: Style,
) {
    if width <= 0 {
        return;
    }
    for (i, c) in gauge_bar(fraction, width as usize).chars().enumerate() {
        if c == ' ' {
            buffer.put('·', x + i as i32, y, track);
        } else {
            buffer.put(c, x + i as i32, y, filled);
        }
    }
}

// Dot bits for the left and right braille columns, bottom row first.
const LEFT_DOTS: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
const RIGHT_DOTS: [u32; 4] = [0x80, 0x20, 0x10, 0x08];

/// btop-style braille graph: each cell is a 2x4 dot grid. Values are normalized (0..1, oldest
/// first); the newest are right-aligned and missing history is blank. Rows come top first.
pub fn braille_render(values: &[f64], width: usize, height: usize) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let capacity = width * 2;
    let recent = &values[values.len().saturating_sub(capacity)..];
    let mut padded = vec![f64::NAN; capacity - recent.len()];
    padded.extend_from_slice(recent);
    let total = (height * 4) as f64;
    let levels: Vec<i64> = padded
        .iter()
        .map(|v| {
            if !v.is_finite() {
                return 0;
            }
            let clamped = v.clamp(0.0, 1.0);
            let level = (clamped * total).round() as i64;
            // Any non-zero activity shows at least one dot so idle-but-alive is visible.
            if clamped > 0.0 {
                level.max(1)
            } else {
                0
            }
        })
        .collect();
    (0..height)
        .map(|row| {
            let floor = ((height - 1 - row) * 4) as i64;
            (0..width)
                .map(|cell| {
                    let left = levels[cell * 2] - floor;
                    let right = levels[cell * 2 + 1] - floor;
                    let mut bits = 0u32;
                    for dot in 0..4 {
                        if left > dot {
                            bits |= LEFT_DOTS[dot as usize];
                        }
                        if right > dot {
                            bits |= RIGHT_DOTS[dot as usize];
                        }
                    }
                    if bits == 0 {
                        ' '
                    } else {
                        char::from_u32(0x2800 + bits).unwrap_or(' ')
                    }
                })
                .collect()
        })
        .collect()
}

/// Draws a graph into a rect, styling each row by its height (0 = bottom, 1 = top).
pub fn draw_braille(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    values: &[f64],
    maximum: f64,
    style: impl Fn(f64) -> Style,
) {
    if rect.is_empty() {
        return;
    }
    let scale = if maximum > 0.0 && maximum.is_finite() {
        maximum
    } else {
        1.0
    };
    let normalized: Vec<f64> = values.iter().map(|v| v / scale).collect();
    for (offset, line) in braille_render(&normalized, rect.width as usize, rect.height as usize)
        .iter()
        .enumerate()
    {
        let fraction = if rect.height <= 1 {
            1.0
        } else {
            f64::from(rect.height - 1 - offset as i32) / f64::from(rect.height - 1)
        };
        buffer.write_max(
            line,
            rect.x,
            rect.y + offset as i32,
            style(fraction),
            rect.width,
        );
    }
}

const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Single-row block sparkline: `▁▂▄▇█`.
pub fn sparkline(values: &[f64], width: usize, maximum: Option<f64>) -> String {
    if width == 0 {
        return String::new();
    }
    let recent = &values[values.len().saturating_sub(width)..];
    let peak = maximum.unwrap_or_else(|| {
        recent
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .fold(0.0, f64::max)
    });
    let mut line = " ".repeat(width - recent.len());
    for &v in recent {
        if !v.is_finite() || peak <= 0.0 || v <= 0.0 {
            line.push(' ');
            continue;
        }
        let fraction = (v / peak).min(1.0);
        let index = ((fraction * (BLOCKS.len() - 1) as f64).round() as usize).min(BLOCKS.len() - 1);
        line.push(BLOCKS[index]);
    }
    line
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnWidth {
    Fixed(i32),
    /// Grows to share leftover space by weight, never below `min`.
    Flexible {
        min: i32,
        weight: i32,
    },
}

impl ColumnWidth {
    fn minimum(self) -> i32 {
        match self {
            ColumnWidth::Fixed(v) => v,
            ColumnWidth::Flexible { min, .. } => min,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub title: String,
    pub width: ColumnWidth,
    pub alignment: Alignment,
    /// Lower is more important; the highest numbers are hidden first when space runs out.
    pub priority: i32,
}

impl TableColumn {
    pub fn new(title: &str, width: ColumnWidth, alignment: Alignment, priority: i32) -> Self {
        TableColumn {
            title: title.into(),
            width,
            alignment,
            priority,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableCell {
    pub text: String,
    pub style: Option<Style>,
}

impl TableCell {
    pub fn new(text: impl Into<String>) -> Self {
        TableCell {
            text: text.into(),
            style: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        TableCell {
            text: text.into(),
            style: Some(style),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableStyles {
    pub header: Style,
    pub row: Style,
    pub selected: Style,
    pub scroll_indicator: Style,
}

/// Where each visible row landed, for mouse hit testing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableLayout {
    pub header_rect: Rect,
    pub row_rects: Vec<(usize, Rect)>,
    pub column_widths: Vec<Option<i32>>,
    pub scroll_offset: usize,
}

impl TableLayout {
    pub fn row_index_at(&self, x: i32, y: i32) -> Option<usize> {
        self.row_rects
            .iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| *i)
    }
}

/// Column widths for the available space; hidden columns are None.
pub fn column_widths(columns: &[TableColumn], total_width: i32, spacing: i32) -> Vec<Option<i32>> {
    let mut visible: Vec<usize> = (0..columns.len()).collect();
    let required = |indices: &[usize]| -> i32 {
        indices
            .iter()
            .map(|i| columns[*i].width.minimum())
            .sum::<i32>()
            + (indices.len() as i32 - 1).max(0) * spacing
    };
    while visible.len() > 1 && required(&visible) > total_width {
        // Drop the least important column (ties: the rightmost).
        let victim = *visible
            .iter()
            .max_by(|a, b| {
                columns[**a]
                    .priority
                    .cmp(&columns[**b].priority)
                    .then(a.cmp(b))
            })
            .unwrap_or(&0);
        visible.retain(|i| *i != victim);
    }
    let mut widths: Vec<Option<i32>> = vec![None; columns.len()];
    for &i in &visible {
        widths[i] = Some(columns[i].width.minimum());
    }
    if visible.len() == 1 {
        let only = visible[0];
        widths[only] = Some(match columns[only].width {
            ColumnWidth::Fixed(f) => f.min(total_width).max(0),
            ColumnWidth::Flexible { .. } => total_width.max(0),
        });
        return widths;
    }
    let mut leftover = total_width - required(&visible);
    let flexible: Vec<(usize, i32)> = visible
        .iter()
        .filter_map(|&i| match columns[i].width {
            ColumnWidth::Flexible { weight, .. } if weight > 0 => Some((i, weight)),
            _ => None,
        })
        .collect();
    let total_weight: i32 = flexible.iter().map(|f| f.1).sum();
    if leftover > 0 && total_weight > 0 {
        let mut given = 0;
        for &(i, w) in &flexible {
            let share = leftover * w / total_weight;
            widths[i] = widths[i].map(|v| v + share);
            given += share;
        }
        leftover -= given;
        for &(i, _) in &flexible {
            if leftover <= 0 {
                break;
            }
            widths[i] = widths[i].map(|v| v + 1);
            leftover -= 1;
        }
    }
    widths
}

/// Adjusts a scroll offset so `selected` stays visible.
pub fn scroll_offset(
    selected: Option<usize>,
    current: usize,
    visible_rows: usize,
    row_count: usize,
) -> usize {
    if visible_rows == 0 || row_count == 0 {
        return 0;
    }
    let max_offset = row_count.saturating_sub(visible_rows);
    let mut offset = current.min(max_offset);
    if let Some(selected) = selected {
        let clamped = selected.min(row_count - 1);
        if clamped < offset {
            offset = clamped;
        }
        if clamped >= offset + visible_rows {
            offset = clamped + 1 - visible_rows;
        }
    }
    offset.min(max_offset)
}

/// Layers a cell's style over the row style; default colors inherit from the row.
pub fn merge(base: Style, overlay: Option<Style>) -> Style {
    let Some(overlay) = overlay else { return base };
    Style {
        fg: if overlay.fg == Color::Default {
            base.fg
        } else {
            overlay.fg
        },
        bg: if overlay.bg == Color::Default {
            base.bg
        } else {
            overlay.bg
        },
        attributes: base.attributes.union(overlay.attributes),
    }
}

pub fn draw_table(
    buffer: &mut ScreenBuffer,
    rect: Rect,
    columns: &[TableColumn],
    rows: &[Vec<TableCell>],
    selected: Option<usize>,
    current_offset: usize,
    styles: TableStyles,
    spacing: i32,
    show_header: bool,
) -> TableLayout {
    let widths = column_widths(columns, rect.width, spacing);
    let mut y = rect.y;
    let header_rect = if show_header {
        Rect::new(rect.x, rect.y, rect.width, i32::from(rect.height > 0))
    } else {
        Rect::ZERO
    };
    if show_header && rect.height > 0 {
        buffer.fill(header_rect, ' ', styles.header);
        let titles: Vec<TableCell> = columns
            .iter()
            .map(|c| TableCell::new(c.title.clone()))
            .collect();
        draw_row(
            buffer,
            rect.x,
            y,
            rect.max_x(),
            &titles,
            columns,
            &widths,
            spacing,
            styles.header,
        );
        y += 1;
    }
    let visible_rows = (rect.max_y() - y).max(0) as usize;
    let offset = scroll_offset(selected, current_offset, visible_rows, rows.len());
    let mut row_rects = Vec::new();
    for (line, index) in (offset..rows.len().min(offset + visible_rows)).enumerate() {
        let row_y = y + line as i32;
        let is_selected = Some(index) == selected;
        let base = if is_selected {
            styles.selected
        } else {
            styles.row
        };
        let row_rect = Rect::new(rect.x, row_y, rect.width, 1);
        buffer.fill(row_rect, ' ', base);
        draw_row(
            buffer,
            rect.x,
            row_y,
            rect.max_x(),
            &rows[index],
            columns,
            &widths,
            spacing,
            base,
        );
        if is_selected {
            // Keep per-cell foreground accents but force the selection background.
            buffer.restyle(row_rect, |s| {
                s.with_bg(styles.selected.bg)
                    .adding(styles.selected.attributes)
            });
        }
        row_rects.push((index, row_rect));
    }
    if rows.len() > visible_rows && visible_rows > 0 && rect.width > 0 {
        if offset > 0 {
            buffer.put('▲', rect.max_x() - 1, y, styles.scroll_indicator);
        }
        if offset + visible_rows < rows.len() {
            buffer.put(
                '▼',
                rect.max_x() - 1,
                y + visible_rows as i32 - 1,
                styles.scroll_indicator,
            );
        }
    }
    TableLayout {
        header_rect,
        row_rects,
        column_widths: widths,
        scroll_offset: offset,
    }
}

fn draw_row(
    buffer: &mut ScreenBuffer,
    x: i32,
    y: i32,
    max_x: i32,
    cells: &[TableCell],
    columns: &[TableColumn],
    widths: &[Option<i32>],
    spacing: i32,
    base: Style,
) {
    let mut column = x;
    let mut first = true;
    let empty = TableCell::default();
    for (index, width) in widths.iter().enumerate() {
        let Some(width) = *width else { continue };
        if !first {
            column += spacing;
        }
        first = false;
        if column >= max_x {
            break;
        }
        let cell = cells.get(index).unwrap_or(&empty);
        let style = merge(base, cell.style);
        let text = text::fit(
            &cell.text,
            width.min(max_x - column).max(0) as usize,
            columns[index].alignment,
        );
        buffer.write_max(&text, column, y, style, max_x - column);
        column += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauges_use_eighths() {
        assert_eq!(gauge_bar(0.5, 4), "██  ");
        assert_eq!(gauge_bar(0.53, 4), "██▏ ");
        assert_eq!(gauge_bar(1.0, 3), "███");
        assert_eq!(gauge_bar(f64::NAN, 2), "  ");
    }

    #[test]
    fn braille_graph_shows_levels() {
        let rows = braille_render(&[1.0, 0.0, 0.5, 0.01], 2, 1);
        assert_eq!(rows, vec!["⡇⣄"]);
        let full = braille_render(&[1.0, 1.0], 1, 2);
        assert_eq!(full, vec!["⣿", "⣿"]);
        assert_eq!(braille_render(&[], 2, 1), vec!["  "]);
    }

    #[test]
    fn sparklines_scale_to_peak() {
        assert_eq!(sparkline(&[0.0, 1.0, 2.0], 4, None), "  ▅█");
        assert_eq!(sparkline(&[5.0], 1, Some(10.0)), "▅");
    }

    #[test]
    fn hides_low_priority_columns_first() {
        let cols = vec![
            TableColumn::new("A", ColumnWidth::Fixed(5), Alignment::Left, 0),
            TableColumn::new(
                "B",
                ColumnWidth::Flexible { min: 5, weight: 1 },
                Alignment::Left,
                0,
            ),
            TableColumn::new("C", ColumnWidth::Fixed(5), Alignment::Left, 2),
        ];
        assert_eq!(column_widths(&cols, 11, 1), vec![Some(5), Some(5), None]);
        assert_eq!(column_widths(&cols, 20, 1), vec![Some(5), Some(8), Some(5)]);
    }

    #[test]
    fn scroll_keeps_selection_visible() {
        assert_eq!(scroll_offset(Some(9), 0, 5, 20), 5);
        assert_eq!(scroll_offset(Some(2), 5, 5, 20), 2);
        assert_eq!(scroll_offset(None, 50, 5, 20), 15);
    }

    #[test]
    fn draws_tables_with_selection_and_indicators() {
        let mut buffer = ScreenBuffer::new(10, 3);
        let cols = vec![TableColumn::new(
            "NAME",
            ColumnWidth::Flexible { min: 4, weight: 1 },
            Alignment::Left,
            0,
        )];
        let rows: Vec<Vec<TableCell>> = (0..5)
            .map(|i| vec![TableCell::new(format!("row{i}"))])
            .collect();
        let styles = TableStyles {
            header: Style::PLAIN,
            row: Style::PLAIN,
            selected: Style::PLAIN.bold(),
            scroll_indicator: Style::PLAIN,
        };
        let bounds = buffer.bounds();
        let layout = draw_table(
            &mut buffer,
            bounds,
            &cols,
            &rows,
            Some(3),
            0,
            styles,
            1,
            true,
        );
        assert_eq!(layout.scroll_offset, 2);
        assert_eq!(buffer.text_row(1).trim_end(), "row2     ▲");
        assert_eq!(layout.row_index_at(0, 2), Some(3));
    }

    #[test]
    fn boxes_embed_titles_and_labels() {
        let mut buffer = ScreenBuffer::new(20, 3);
        let bounds = buffer.bounds();
        let inner = draw_box(
            &mut buffer,
            bounds,
            Some("cpu"),
            Some("4%"),
            Style::PLAIN,
            None,
            BorderCharacters::ROUNDED,
        );
        assert_eq!(inner, Rect::new(1, 1, 18, 1));
        assert_eq!(buffer.text_row(0), "╭─ cpu ─────── 4% ─╮");
    }
}
