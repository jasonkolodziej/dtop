//! Raw terminal input → events: VT/xterm keys with modifiers, CSI u, SGR and X10 mouse reports,
//! bracketed paste and focus. Incremental: partial sequences wait for the next `feed`, and
//! `flush` resolves a lone ESC once the caller knows no more bytes are coming.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: Modifiers = Modifiers(1);
    pub const ALT: Modifiers = Modifiers(2);
    pub const CTRL: Modifiers = Modifiers(4);

    /// xterm encodes modifiers as `1 + bitmask(shift=1, alt=2, ctrl=4)`.
    pub fn from_xterm(parameter: i64) -> Modifiers {
        Modifiers(((parameter - 1).max(0) & 7) as u8)
    }

    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    pub fn new(key: Key) -> Self {
        KeyEvent {
            key,
            modifiers: Modifiers::NONE,
        }
    }

    pub fn with(key: Key, modifiers: Modifiers) -> Self {
        KeyEvent { key, modifiers }
    }

    pub fn char(c: char) -> Self {
        KeyEvent::new(Key::Char(c))
    }

    pub fn ctrl(c: char) -> Self {
        KeyEvent::with(Key::Char(c), Modifiers::CTRL)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseAction {
    Press,
    Release,
    Drag,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub action: MouseAction,
    /// Zero-based column and row.
    pub x: i32,
    pub y: i32,
    pub modifiers: Modifiers,
}

impl MouseEvent {
    pub fn new(button: MouseButton, action: MouseAction, x: i32, y: i32) -> Self {
        MouseEvent {
            button,
            action,
            x,
            y,
            modifiers: Modifiers::NONE,
        }
    }

    pub fn is_wheel(&self) -> bool {
        matches!(
            self.button,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Focus(bool),
}

enum Parse {
    Event(Option<InputEvent>, usize),
    Incomplete,
    PasteBegins(usize),
}

const ESC: u8 = 0x1b;
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Default)]
pub struct InputParser {
    buffer: Vec<u8>,
    paste: Option<Vec<u8>>,
}

impl InputParser {
    pub fn new() -> Self {
        InputParser::default()
    }

    /// True when bytes wait for the rest of a sequence.
    pub fn has_pending_input(&self) -> bool {
        !self.buffer.is_empty() || self.paste.is_some()
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while !self.buffer.is_empty() {
            if self.paste.is_some() {
                if !self.consume_paste(&mut events) {
                    break;
                }
                continue;
            }
            match self.parse_next() {
                Parse::Event(event, length) => {
                    self.buffer.drain(..length);
                    events.extend(event);
                }
                Parse::Incomplete => return events,
                Parse::PasteBegins(length) => {
                    self.buffer.drain(..length);
                    self.paste = Some(Vec::new());
                }
            }
        }
        events
    }

    /// True while a CSI/SS3 sequence (e.g. a mouse report) or a paste has started but not
    /// finished. Over SSH a report can arrive in two reads; resolving it early would turn its
    /// tail into keystrokes.
    pub fn has_incomplete_sequence(&self) -> bool {
        if self.paste.is_some() {
            return true;
        }
        self.buffer.len() >= 2
            && self.buffer[0] == ESC
            && (self.buffer[1] == b'[' || self.buffer[1] == b'O')
    }

    /// Suggested wait (seconds) before calling `flush` for the pending input.
    pub fn flush_delay(&self) -> f64 {
        if self.has_incomplete_sequence() {
            0.5
        } else {
            0.03
        }
    }

    /// Resolves pending input: a lone ESC becomes Escape, a dangling `ESC x` becomes Alt+x, an
    /// unterminated paste is delivered as-is. An unfinished CSI/SS3 sequence longer than its
    /// introducer is dropped (its parameter bytes would otherwise trigger commands).
    pub fn flush(&mut self) -> Vec<InputEvent> {
        let mut events = Vec::new();
        if self.paste.is_none() && self.buffer.len() > 2 && self.has_incomplete_sequence() {
            self.buffer.clear();
            return events;
        }
        if let Some(mut paste) = self.paste.take() {
            paste.extend(self.buffer.drain(..));
            events.push(InputEvent::Paste(
                String::from_utf8_lossy(&paste).into_owned(),
            ));
            return events;
        }
        while !self.buffer.is_empty() {
            if self.buffer[0] == ESC {
                self.buffer.remove(0);
                if self.buffer.is_empty() {
                    events.push(InputEvent::Key(KeyEvent::new(Key::Escape)));
                } else if let Parse::Event(Some(InputEvent::Key(mut key)), length) =
                    self.parse_next()
                {
                    self.buffer.drain(..length);
                    key.modifiers.insert(Modifiers::ALT);
                    events.push(InputEvent::Key(key));
                } else {
                    events.push(InputEvent::Key(KeyEvent::new(Key::Escape)));
                }
                continue;
            }
            match self.parse_next() {
                Parse::Event(event, length) => {
                    self.buffer.drain(..length);
                    events.extend(event);
                }
                // Invalid trailing bytes (e.g. truncated UTF-8): drop them.
                Parse::Incomplete | Parse::PasteBegins(_) => self.buffer.clear(),
            }
        }
        events
    }

    fn consume_paste(&mut self, events: &mut Vec<InputEvent>) -> bool {
        let Some(end) = self
            .buffer
            .windows(PASTE_END.len())
            .position(|w| w == PASTE_END)
        else {
            // Keep a tail that might be the start of the terminator.
            let keep = self.buffer.len().min(PASTE_END.len() - 1);
            let take = self.buffer.len() - keep;
            let head: Vec<u8> = self.buffer.drain(..take).collect();
            if let Some(paste) = self.paste.as_mut() {
                paste.extend(head);
            }
            return false;
        };
        let head: Vec<u8> = self.buffer.drain(..end).collect();
        self.buffer.drain(..PASTE_END.len());
        let mut paste = self.paste.take().unwrap_or_default();
        paste.extend(head);
        events.push(InputEvent::Paste(
            String::from_utf8_lossy(&paste).into_owned(),
        ));
        true
    }

    fn parse_next(&self) -> Parse {
        let byte = self.buffer[0];
        if byte == ESC {
            return self.parse_escape();
        }
        if let Some(control) = control_key(byte) {
            return Parse::Event(Some(InputEvent::Key(control)), 1);
        }
        match self.parse_utf8(0) {
            Some((c, length)) => Parse::Event(Some(InputEvent::Key(KeyEvent::char(c))), length),
            None => Parse::Incomplete,
        }
    }

    /// None when incomplete; invalid bytes decode to U+FFFD.
    fn parse_utf8(&self, index: usize) -> Option<(char, usize)> {
        let lead = self.buffer[index];
        let length = match lead {
            0x00..=0x7F => 1,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return Some(('\u{FFFD}', 1)),
        };
        if self.buffer.len() - index < length {
            return None;
        }
        let bytes = &self.buffer[index..index + length];
        if bytes[1..].iter().any(|b| b & 0xC0 != 0x80) {
            return Some(('\u{FFFD}', 1));
        }
        let c = std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('\u{FFFD}');
        Some((c, length))
    }

    fn parse_escape(&self) -> Parse {
        if self.buffer.len() < 2 {
            return Parse::Incomplete;
        }
        let next = self.buffer[1];
        match next {
            b'[' => self.parse_csi(),
            b'O' => {
                if self.buffer.len() < 3 {
                    return Parse::Incomplete;
                }
                Parse::Event(ss3_key(self.buffer[2]).map(InputEvent::Key), 3)
            }
            ESC => Parse::Event(Some(InputEvent::Key(KeyEvent::new(Key::Escape))), 1),
            _ => {
                // Alt+key: ESC followed by a normal key.
                if let Some(mut control) = control_key(next) {
                    control.modifiers.insert(Modifiers::ALT);
                    return Parse::Event(Some(InputEvent::Key(control)), 2);
                }
                match self.parse_utf8(1) {
                    Some((c, length)) => Parse::Event(
                        Some(InputEvent::Key(KeyEvent::with(
                            Key::Char(c),
                            Modifiers::ALT,
                        ))),
                        1 + length,
                    ),
                    None => Parse::Incomplete,
                }
            }
        }
    }

    fn parse_csi(&self) -> Parse {
        let b = &self.buffer;
        // X10 mouse: ESC [ M Cb Cx Cy
        if b.len() >= 3 && b[2] == b'M' {
            if b.len() < 6 {
                return Parse::Incomplete;
            }
            let (code, x, y) = (
                i64::from(b[3]) - 32,
                i64::from(b[4]) - 33,
                i64::from(b[5]) - 33,
            );
            return Parse::Event(
                mouse_event(code, x, y, false, true).map(InputEvent::Mouse),
                6,
            );
        }
        let mut index = 2;
        while index < b.len() && (0x30..=0x3F).contains(&b[index]) {
            index += 1;
        }
        while index < b.len() && (0x20..=0x2F).contains(&b[index]) {
            index += 1;
        }
        if index >= b.len() {
            return if b.len() > 64 {
                Parse::Event(None, b.len())
            } else {
                Parse::Incomplete
            };
        }
        let final_byte = b[index];
        let length = index + 1;
        if !(0x40..=0x7E).contains(&final_byte) {
            // Malformed: drop the introducer so parsing can resynchronize.
            return Parse::Event(None, 2);
        }
        let parameters = String::from_utf8_lossy(&b[2..index]).into_owned();
        if let Some(sgr) = parameters.strip_prefix('<') {
            if final_byte == b'M' || final_byte == b'm' {
                let numbers: Vec<Option<i64>> = sgr.split(';').map(|n| n.parse().ok()).collect();
                let event = match numbers[..] {
                    [Some(code), Some(x), Some(y)] => {
                        mouse_event(code, x - 1, y - 1, final_byte == b'm', false)
                    }
                    _ => None,
                };
                return Parse::Event(event.map(InputEvent::Mouse), length);
            }
        }
        let values: Vec<Option<i64>> = parameters.split(';').map(|n| n.parse().ok()).collect();
        let first = values.first().copied().flatten();
        let modifiers = if values.len() > 1 {
            Modifiers::from_xterm(values[1].unwrap_or(1))
        } else {
            Modifiers::NONE
        };
        let key =
            |k: Key| Parse::Event(Some(InputEvent::Key(KeyEvent::with(k, modifiers))), length);
        match final_byte {
            b'A' => key(Key::Up),
            b'B' => key(Key::Down),
            b'C' => key(Key::Right),
            b'D' => key(Key::Left),
            b'H' => key(Key::Home),
            b'F' => key(Key::End),
            b'Z' => Parse::Event(
                Some(InputEvent::Key(KeyEvent::with(
                    Key::BackTab,
                    Modifiers::SHIFT,
                ))),
                length,
            ),
            b'P' => key(Key::F(1)),
            b'Q' => key(Key::F(2)),
            b'R' => key(Key::F(3)),
            b'S' => key(Key::F(4)),
            b'I' => Parse::Event(Some(InputEvent::Focus(true)), length),
            b'O' => Parse::Event(Some(InputEvent::Focus(false)), length),
            b'u' => {
                // CSI codepoint ; modifiers u (fixterms / kitty keyboard protocol)
                let Some(c) = first
                    .and_then(|cp| u32::try_from(cp).ok())
                    .and_then(char::from_u32)
                else {
                    return Parse::Event(None, length);
                };
                match c as u32 {
                    13 => key(Key::Enter),
                    9 => key(Key::Tab),
                    27 => key(Key::Escape),
                    127 => key(Key::Backspace),
                    _ => key(Key::Char(c)),
                }
            }
            b'~' => match first.unwrap_or(0) {
                200 => Parse::PasteBegins(length),
                1 | 7 => key(Key::Home),
                2 => key(Key::Insert),
                3 => key(Key::Delete),
                4 | 8 => key(Key::End),
                5 => key(Key::PageUp),
                6 => key(Key::PageDown),
                n @ 11..=15 => key(Key::F((n - 10) as u8)),
                n @ 17..=21 => key(Key::F((n - 11) as u8)),
                n @ 23..=24 => key(Key::F((n - 12) as u8)),
                _ => Parse::Event(None, length),
            },
            _ => Parse::Event(None, length),
        }
    }
}

fn control_key(byte: u8) -> Option<KeyEvent> {
    Some(match byte {
        0x0D | 0x0A => KeyEvent::new(Key::Enter),
        0x09 => KeyEvent::new(Key::Tab),
        0x7F | 0x08 => KeyEvent::new(Key::Backspace),
        0x00 => KeyEvent::ctrl(' '),
        0x01..=0x1A => KeyEvent::ctrl(char::from(byte + 0x60)),
        0x1C => KeyEvent::ctrl('\\'),
        0x1D => KeyEvent::ctrl(']'),
        0x1E => KeyEvent::ctrl('^'),
        0x1F => KeyEvent::ctrl('_'),
        _ => return None,
    })
}

fn ss3_key(byte: u8) -> Option<KeyEvent> {
    Some(KeyEvent::new(match byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'M' => Key::Enter,
        b'P' => Key::F(1),
        b'Q' => Key::F(2),
        b'R' => Key::F(3),
        b'S' => Key::F(4),
        _ => return None,
    }))
}

fn mouse_event(code: i64, x: i64, y: i64, is_release: bool, legacy: bool) -> Option<MouseEvent> {
    if code < 0 || x < 0 || y < 0 {
        return None;
    }
    let mut modifiers = Modifiers::NONE;
    if code & 4 != 0 {
        modifiers.insert(Modifiers::SHIFT);
    }
    if code & 8 != 0 {
        modifiers.insert(Modifiers::ALT);
    }
    if code & 16 != 0 {
        modifiers.insert(Modifiers::CTRL);
    }
    let low = code & 3;
    let (x, y) = (x as i32, y as i32);
    if code & 64 != 0 {
        let button = [
            MouseButton::WheelUp,
            MouseButton::WheelDown,
            MouseButton::WheelLeft,
            MouseButton::WheelRight,
        ][low as usize];
        return Some(MouseEvent {
            button,
            action: MouseAction::Press,
            x,
            y,
            modifiers,
        });
    }
    let button = match low {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::None,
    };
    let action = if code & 32 != 0 {
        if button == MouseButton::None {
            MouseAction::Move
        } else {
            MouseAction::Drag
        }
    } else if is_release || (legacy && low == 3) {
        MouseAction::Release
    } else {
        MouseAction::Press
    };
    Some(MouseEvent {
        button,
        action,
        x,
        y,
        modifiers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> Vec<InputEvent> {
        let mut p = InputParser::new();
        let mut events = p.feed(bytes);
        events.extend(p.flush());
        events
    }

    fn key(k: Key, m: Modifiers) -> InputEvent {
        InputEvent::Key(KeyEvent::with(k, m))
    }

    #[test]
    fn parses_keys_and_modifiers() {
        assert_eq!(
            parse_all(b"a\r\x7f\x03"),
            vec![
                key(Key::Char('a'), Modifiers::NONE),
                key(Key::Enter, Modifiers::NONE),
                key(Key::Backspace, Modifiers::NONE),
                key(Key::Char('c'), Modifiers::CTRL),
            ]
        );
        assert_eq!(parse_all(b"\x1b[1;5A"), vec![key(Key::Up, Modifiers::CTRL)]);
        assert_eq!(
            parse_all(b"\x1b[15~\x1b[6~\x1bOP"),
            vec![
                key(Key::F(5), Modifiers::NONE),
                key(Key::PageDown, Modifiers::NONE),
                key(Key::F(1), Modifiers::NONE)
            ]
        );
        assert_eq!(
            parse_all(b"\x1b[97;5u"),
            vec![key(Key::Char('a'), Modifiers::CTRL)]
        );
        assert_eq!(
            parse_all(b"\x1b[Z"),
            vec![key(Key::BackTab, Modifiers::SHIFT)]
        );
        assert_eq!(
            parse_all("é日".as_bytes()),
            vec![
                key(Key::Char('é'), Modifiers::NONE),
                key(Key::Char('日'), Modifiers::NONE)
            ]
        );
    }

    #[test]
    fn resolves_escape_and_alt() {
        assert_eq!(parse_all(b"\x1b"), vec![key(Key::Escape, Modifiers::NONE)]);
        assert_eq!(
            parse_all(b"\x1bx"),
            vec![key(Key::Char('x'), Modifiers::ALT)]
        );
    }

    #[test]
    fn parses_mouse_reports() {
        assert_eq!(
            parse_all(b"\x1b[<0;10;5M"),
            vec![InputEvent::Mouse(MouseEvent::new(
                MouseButton::Left,
                MouseAction::Press,
                9,
                4
            ))]
        );
        assert_eq!(
            parse_all(b"\x1b[<0;10;5m"),
            vec![InputEvent::Mouse(MouseEvent::new(
                MouseButton::Left,
                MouseAction::Release,
                9,
                4
            ))]
        );
        assert_eq!(
            parse_all(b"\x1b[<65;1;1M"),
            vec![InputEvent::Mouse(MouseEvent::new(
                MouseButton::WheelDown,
                MouseAction::Press,
                0,
                0
            ))]
        );
        assert_eq!(
            parse_all(b"\x1b[<32;3;3M"),
            vec![InputEvent::Mouse(MouseEvent::new(
                MouseButton::Left,
                MouseAction::Drag,
                2,
                2
            ))]
        );
        assert_eq!(
            parse_all(&[0x1b, b'[', b'M', 32, 33 + 4, 33 + 2]),
            vec![InputEvent::Mouse(MouseEvent::new(
                MouseButton::Left,
                MouseAction::Press,
                4,
                2
            ))]
        );
    }

    #[test]
    fn every_split_point_gives_the_same_events() {
        let input = b"a\x1b[<0;10;5M\x1b[200~pa\x1b[201ste\x1b[201~\x1b[1;2Bz";
        let expected = parse_all(input);
        assert_eq!(expected.len(), 5);
        assert_eq!(expected[2], InputEvent::Paste("pa\x1b[201ste".into()));
        for split in 0..=input.len() {
            let mut p = InputParser::new();
            let mut events = p.feed(&input[..split]);
            events.extend(p.feed(&input[split..]));
            events.extend(p.flush());
            assert_eq!(events, expected, "split {split}");
        }
    }

    #[test]
    fn incomplete_csi_is_dropped_not_typed() {
        let mut p = InputParser::new();
        assert!(p.feed(b"\x1b[<0;1").is_empty());
        assert!(p.has_incomplete_sequence());
        assert!((p.flush_delay() - 0.5).abs() < 1e-9);
        assert!(p.flush().is_empty());
    }
}
