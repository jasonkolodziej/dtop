//! Terminal column widths: a compact wcwidth. Control characters and combining marks take no
//! space; East Asian wide/fullwidth characters and emoji take two. Text is handled in
//! "clusters" (a base character plus the zero-width characters that follow it, and ZWJ
//! sequences), a small stand-in for grapheme clusters without a Unicode-segmentation crate.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
    Center,
}

fn is_zero_width(v: u32) -> bool {
    matches!(v,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
        | 0x0E31 | 0x0E34..=0x0E3A | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x200B..=0x200F | 0x2028..=0x202E
        | 0x2060..=0x2064 | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0xFEFF | 0xE0100..=0xE01EF)
}

fn is_wide(v: u32) -> bool {
    matches!(v,
        0x1100..=0x115F | 0x231A..=0x231B | 0x2329..=0x232A | 0x23E9..=0x23EC | 0x23F0 | 0x23F3
        | 0x25FD..=0x25FE | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1 | 0x26AA..=0x26AB
        | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA
        | 0x26FD | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757
        | 0x2795..=0x2797 | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
        | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF
        | 0xA960..=0xA97F | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F004 | 0x1F0CF | 0x1F18E | 0x1F191..=0x1F19A
        | 0x1F200..=0x1F251 | 0x1F300..=0x1F64F | 0x1F680..=0x1F6FF | 0x1F7E0..=0x1F7EB
        | 0x1F900..=0x1F9FF | 0x1FA70..=0x1FAFF | 0x20000..=0x2FFFD | 0x30000..=0x3FFFD)
}

/// Width of one scalar.
pub fn char_width(c: char) -> usize {
    let v = c as u32;
    if v < 0x20 || (0x7F..0xA0).contains(&v) || is_zero_width(v) {
        0
    } else if is_wide(v) {
        2
    } else {
        1
    }
}

/// Width of one cluster: an emoji presentation selector or ZWJ sequence renders as one wide
/// glyph; a leading zero-width scalar takes the width of what follows.
pub fn cluster_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    let Some(first) = chars.next() else { return 0 };
    if cluster.chars().count() > 1 && cluster.chars().any(|c| c == '\u{FE0F}' || c == '\u{200D}') {
        return 2;
    }
    match char_width(first) {
        0 => chars.map(char_width).max().unwrap_or(0),
        w => w,
    }
}

fn is_modifier(c: char) -> bool {
    // Skin tones attach to the emoji before them.
    matches!(c as u32, 0x1F3FB..=0x1F3FF)
}

/// Characters that must never reach the terminal: controls (C0, DEL, C1 — ESC, CSI, newlines…)
/// move the cursor or start escape sequences, and bidirectional marks, embeddings, overrides and
/// isolates make text appear reordered.
pub fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Splits text into clusters. An unsafe character (see `is_unsafe`) is always a cluster of its
/// own, taking no columns, so drawing skips it and nothing can attach to it or hide it inside a
/// visible cluster.
pub fn clusters(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut previous_zwj = false;
    let mut previous_unsafe = false;
    for (index, c) in text.char_indices() {
        let unsafe_char = is_unsafe(c);
        let attaches = start.is_some()
            && !unsafe_char
            && !previous_unsafe
            && (previous_zwj || char_width(c) == 0 || is_modifier(c));
        if !attaches {
            if let Some(s) = start {
                out.push(&text[s..index]);
            }
            start = Some(index);
        }
        previous_zwj = c == '\u{200D}';
        previous_unsafe = unsafe_char;
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// Text from outside dtop (container names, labels, daemon messages) made safe to print: tabs
/// become a space and every other unsafe character (see `is_unsafe`) becomes U+FFFD, so it
/// cannot move the cursor, clear the screen, start a new line or reorder the text around it.
pub fn sanitize(text: &str) -> String {
    if !text.chars().any(is_unsafe) {
        return text.to_string();
    }
    text.chars()
        .map(|c| match c {
            '\t' => ' ',
            c if is_unsafe(c) => '\u{FFFD}',
            c => c,
        })
        .collect()
}

/// Columns `text` occupies.
pub fn width(text: &str) -> usize {
    if text.is_ascii() {
        return text.bytes().filter(|b| *b >= 0x20 && *b != 0x7F).count();
    }
    clusters(text).into_iter().map(cluster_width).sum()
}

/// Longest prefix that fits in `max` columns (never splits a wide character).
pub fn prefix(text: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for cluster in clusters(text) {
        let w = cluster_width(cluster);
        if used + w > max {
            break;
        }
        used += w;
        out.push_str(cluster);
    }
    out
}

/// Cuts text to fit `max` columns, appending `ellipsis` when something was removed.
pub fn truncate_with(text: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    if width(text) <= max {
        return text.to_string();
    }
    let ellipsis_width = width(ellipsis);
    if ellipsis_width > max {
        return prefix(ellipsis, max);
    }
    prefix(text, max - ellipsis_width) + ellipsis
}

pub fn truncate(text: &str, max: usize) -> String {
    truncate_with(text, max, "…")
}

/// Truncates or pads to exactly `max` columns.
pub fn fit(text: &str, max: usize, alignment: Alignment) -> String {
    if max == 0 {
        return String::new();
    }
    let cut = truncate(text, max);
    let padding = max.saturating_sub(width(&cut));
    match alignment {
        Alignment::Left => cut + &" ".repeat(padding),
        Alignment::Right => " ".repeat(padding) + &cut,
        Alignment::Center => {
            let left = padding / 2;
            format!("{}{cut}{}", " ".repeat(left), " ".repeat(padding - left))
        }
    }
}

/// Greedy word wrap; words longer than the width are hard-broken.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let mut remaining = word.to_string();
            while !remaining.is_empty() {
                let word_width = width(&remaining);
                let needed = if line_width == 0 {
                    word_width
                } else {
                    line_width + 1 + word_width
                };
                if needed <= max {
                    if line_width > 0 {
                        line.push(' ');
                    }
                    line.push_str(&remaining);
                    line_width = needed;
                    remaining.clear();
                } else if line_width > 0 {
                    lines.push(std::mem::take(&mut line));
                    line_width = 0;
                } else {
                    // Hard-break a word longer than the line; the piece becomes the current line
                    // so a final fragment is not followed by an empty line.
                    let head = prefix(&remaining, max);
                    let head = if head.is_empty() {
                        clusters(&remaining)
                            .first()
                            .map_or_else(String::new, |c| c.to_string())
                    } else {
                        head
                    };
                    line_width = max.max(width(&head));
                    remaining = remaining[head.len()..].to_string();
                    line = head;
                }
            }
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_wide_and_zero_width_text() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("👍🏽"), 2);
        assert_eq!(width("👨‍👩‍👧"), 2);
        assert_eq!(width("\u{2764}\u{FE0F}"), 2);
        assert_eq!(clusters("a👨‍👩‍👧b").len(), 3);
    }

    #[test]
    fn sanitizes_control_characters() {
        assert_eq!(sanitize("plain · 日本"), "plain · 日本");
        assert_eq!(sanitize("a\tb"), "a b");
        assert_eq!(
            sanitize("x\u{1b}[2Jy\nz\r"),
            "x\u{FFFD}[2Jy\u{FFFD}z\u{FFFD}"
        );
        assert_eq!(sanitize("\u{9b}31m\u{7f}"), "\u{FFFD}31m\u{FFFD}");
        assert_eq!(sanitize("a\u{202E}b"), "a\u{FFFD}b");
        assert_eq!(width(&sanitize("\u{1b}x")), 2);
    }

    #[test]
    fn unsafe_characters_never_join_a_visible_cluster() {
        assert_eq!(clusters("a\u{9b}b"), vec!["a", "\u{9b}", "b"]);
        assert_eq!(clusters("\u{1b}\u{FE0F}"), vec!["\u{1b}", "\u{FE0F}"]);
        assert_eq!(clusters("x\u{200D}\u{1b}"), vec!["x\u{200D}", "\u{1b}"]);
        assert_eq!(clusters("a\u{202E}b"), vec!["a", "\u{202E}", "b"]);
        assert_eq!(cluster_width("\u{1b}"), 0);
        assert_eq!(width("a\u{7f}\u{9b}b"), 2);
        // Ordinary combining sequences are unchanged.
        assert_eq!(clusters("e\u{301}x"), vec!["e\u{301}", "x"]);
    }

    #[test]
    fn truncates_and_fits() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate("日本語", 5), "日本…");
        assert_eq!(fit("ab", 5, Alignment::Right), "   ab");
        assert_eq!(fit("ab", 5, Alignment::Center), " ab  ");
        assert_eq!(fit("abcdef", 3, Alignment::Left), "ab…");
        assert_eq!(prefix("日本", 3), "日");
    }

    #[test]
    fn wraps_words_and_breaks_long_ones() {
        assert_eq!(
            wrap("the quick brown fox", 9),
            vec!["the quick", "brown fox"]
        );
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 4), vec!["a", "", "b"]);
    }
}
