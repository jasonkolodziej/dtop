//! Docker's log stream format and newline-delimited JSON streams.

use std::collections::BTreeMap;

use crate::error::{DockerError, Result};
use crate::timestamp::{self, Timestamp};

/// Splits a byte stream into newline-delimited records (`/events`).
#[derive(Debug, Default, Clone)]
pub struct JsonLineSplitter {
    buffer: Vec<u8>,
}

impl JsonLineSplitter {
    pub const MAX_LINE_LENGTH: usize = 8 * 1024 * 1024;

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.buffer.extend_from_slice(bytes);
        let mut lines = Vec::new();
        let mut start = 0;
        for index in 0..self.buffer.len() {
            if self.buffer[index] == b'\n' {
                let line = &self.buffer[start..index];
                if line.iter().any(|&c| c != b'\r' && c != b' ') {
                    lines.push(line.to_vec());
                }
                start = index + 1;
            }
        }
        self.buffer.drain(..start);
        if self.buffer.len() > Self::MAX_LINE_LENGTH {
            return Err(DockerError::MalformedResponse(format!(
                "streamed record exceeds {} bytes",
                Self::MAX_LINE_LENGTH
            )));
        }
        Ok(lines)
    }

    /// Remaining bytes when the stream ends without a trailing newline.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let rest = std::mem::take(&mut self.buffer);
        rest.iter()
            .any(|&c| !matches!(c, b'\r' | b' ' | b'\n'))
            .then_some(rest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogStreamKind {
    Stdin = 0,
    Stdout = 1,
    Stderr = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub stream: LogStreamKind,
    pub text: String,
    /// When the daemon wrote the line, exact to the nanosecond, for `timestamps=1` streams.
    pub timestamp: Option<Timestamp>,
}

impl LogLine {
    pub fn new(stream: LogStreamKind, text: &str) -> Self {
        LogLine {
            stream,
            text: text.into(),
            timestamp: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderMode {
    Automatic,
    Multiplexed,
    Raw,
}

/// The unterminated end of one stream: bytes up to `MAX_LINE_BYTES`, and whether more were
/// dropped.
#[derive(Debug, Clone, Default)]
struct PartialLine {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Decodes Docker's log/attach stream. Containers without a TTY multiplex stdout/stderr with
/// 8-byte frame headers (`[stream, 0, 0, 0, size(be32)]`); TTY containers send raw bytes.
/// Incremental, so it serves both buffered and followed logs. Memory is bounded whatever the
/// container writes: frame payloads are decoded as they arrive, and a line longer than
/// `MAX_LINE_BYTES` keeps its start and is marked with `TRUNCATION_MARK`.
#[derive(Debug, Clone)]
pub struct LogStreamDecoder {
    mode: DecoderMode,
    buffer: Vec<u8>,
    /// The stream and remaining payload bytes of the frame being read (multiplexed mode).
    frame: Option<(LogStreamKind, usize)>,
    partial: BTreeMap<LogStreamKind, PartialLine>,
    timestamps: bool,
}

impl LogStreamDecoder {
    /// Longest line kept, timestamp included; the rest of a longer line is dropped.
    pub const MAX_LINE_BYTES: usize = 64 * 1024;
    /// Appended to lines cut at `MAX_LINE_BYTES`.
    pub const TRUNCATION_MARK: &'static str = " … [line truncated]";

    pub fn new(mode: DecoderMode, timestamps: bool) -> Self {
        LogStreamDecoder {
            mode,
            buffer: Vec::new(),
            frame: None,
            partial: BTreeMap::new(),
            timestamps,
        }
    }

    fn make_line(&self, line: &PartialLine, stream: LogStreamKind) -> LogLine {
        let bytes = line.bytes.as_slice();
        let finish = |text: String| {
            if line.truncated {
                text + Self::TRUNCATION_MARK
            } else {
                text
            }
        };
        if self.timestamps {
            if let Some(space) = bytes.iter().position(|&c| c == b' ').filter(|&s| s >= 20) {
                if let Some(time) =
                    timestamp::parse_exact(&String::from_utf8_lossy(&bytes[..space]))
                {
                    return LogLine {
                        stream,
                        text: finish(clean(&bytes[space + 1..])),
                        timestamp: Some(time),
                    };
                }
            }
        }
        LogLine {
            stream,
            text: finish(clean(bytes)),
            timestamp: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<LogLine> {
        self.buffer.extend_from_slice(bytes);
        if self.mode == DecoderMode::Automatic {
            match detect_mode(&self.buffer) {
                Some(mode) => self.mode = mode,
                None => return Vec::new(),
            }
        }
        let mut lines = Vec::new();
        match self.mode {
            DecoderMode::Raw | DecoderMode::Automatic => {
                let data = std::mem::take(&mut self.buffer);
                self.append_text(&data, LogStreamKind::Stdout, &mut lines);
            }
            DecoderMode::Multiplexed => {
                let mut offset = 0;
                loop {
                    if let Some((stream, remaining)) = self.frame {
                        let available = remaining.min(self.buffer.len() - offset);
                        if available == 0 && remaining > 0 {
                            break;
                        }
                        let payload = self.buffer[offset..offset + available].to_vec();
                        offset += available;
                        self.append_text(&payload, stream, &mut lines);
                        self.frame =
                            (remaining > available).then_some((stream, remaining - available));
                        continue;
                    }
                    let header = &self.buffer[offset..];
                    if header.len() < 8 {
                        break;
                    }
                    let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
                    let stream = match header[0] {
                        0 => LogStreamKind::Stdin,
                        2 => LogStreamKind::Stderr,
                        _ => LogStreamKind::Stdout,
                    };
                    offset += 8;
                    self.frame = (size > 0).then_some((stream, size as usize));
                }
                self.buffer.drain(..offset);
            }
        }
        lines
    }

    /// Emits trailing text that did not end with a newline.
    pub fn finish(&mut self) -> Vec<LogLine> {
        if self.mode == DecoderMode::Automatic && !self.buffer.is_empty() {
            self.mode = DecoderMode::Raw;
            let mut lines = self.feed(&[]);
            lines.extend(self.finish());
            return lines;
        }
        let partial = std::mem::take(&mut self.partial);
        partial
            .into_iter()
            .filter(|(_, line)| !line.bytes.is_empty())
            .map(|(stream, line)| self.make_line(&line, stream))
            .collect()
    }

    fn append_text(&mut self, bytes: &[u8], stream: LogStreamKind, lines: &mut Vec<LogLine>) {
        let mut pending = self.partial.remove(&stream).unwrap_or_default();
        for segment in bytes.split_inclusive(|&b| b == b'\n') {
            let (text, ends_line) = match segment.split_last() {
                Some((b'\n', text)) => (text, true),
                _ => (segment, false),
            };
            let room = Self::MAX_LINE_BYTES.saturating_sub(pending.bytes.len());
            if pending.truncated {
                // Past the limit: the rest of this line is dropped.
            } else if text.len() > room {
                pending.bytes.extend_from_slice(&text[..room]);
                trim_partial_utf8(&mut pending.bytes);
                pending.truncated = true;
            } else {
                pending.bytes.extend_from_slice(text);
            }
            if ends_line {
                lines.push(self.make_line(&pending, stream));
                pending = PartialLine::default();
            }
        }
        if !pending.bytes.is_empty() {
            self.partial.insert(stream, pending);
        }
    }
}

/// Drops an incomplete UTF-8 sequence left at the end by a cut.
fn trim_partial_utf8(bytes: &mut Vec<u8>) {
    let tail = bytes.len().saturating_sub(4);
    if let Some(lead) = (tail..bytes.len()).rev().find(|&i| bytes[i] & 0xC0 != 0x80) {
        let expected = match bytes[lead] {
            b if b >= 0xF0 => 4,
            b if b >= 0xE0 => 3,
            b if b >= 0xC0 => 2,
            _ => 1,
        };
        if lead + expected > bytes.len() {
            bytes.truncate(lead);
        }
    }
}

/// A multiplexed stream starts with a stream byte of 0-2 followed by three zero bytes.
pub fn detect_mode(bytes: &[u8]) -> Option<DecoderMode> {
    let first = *bytes.first()?;
    if first > 2 {
        return Some(DecoderMode::Raw);
    }
    if bytes.len() < 4 {
        return if bytes[1..].iter().all(|&c| c == 0) {
            None
        } else {
            Some(DecoderMode::Raw)
        };
    }
    Some(if bytes[1] == 0 && bytes[2] == 0 && bytes[3] == 0 {
        DecoderMode::Multiplexed
    } else {
        DecoderMode::Raw
    })
}

/// Strips carriage returns and terminal escape sequences that would corrupt dtop's drawing,
/// and expands tabs.
pub fn clean(bytes: &[u8]) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == 0x1B {
            i += 1;
            if bytes.get(i) == Some(&b'[') {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
            } else if bytes.get(i) == Some(&b']') {
                // OSC: terminated by BEL or ESC \
                while i < bytes.len() && bytes[i] != 0x07 {
                    if bytes[i] == 0x1B && bytes.get(i + 1) == Some(&b'\\') {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        if byte == b'\t' {
            out.extend_from_slice(b"    ");
        } else if byte >= 0x20 && byte != 0x7F {
            out.push(byte);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(stream: u8, text: &str) -> Vec<u8> {
        let mut bytes = vec![stream, 0, 0, 0];
        bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
        bytes.extend_from_slice(text.as_bytes());
        bytes
    }

    #[test]
    fn demultiplexes_frames_split_anywhere() {
        let mut data = frame(1, "out\npart");
        data.extend(frame(2, "err\n"));
        data.extend(frame(1, "ial\n"));
        for split in 0..=data.len() {
            let mut d = LogStreamDecoder::new(DecoderMode::Automatic, false);
            let mut lines = d.feed(&data[..split]);
            lines.extend(d.feed(&data[split..]));
            lines.extend(d.finish());
            let texts: Vec<_> = lines.iter().map(|l| (l.stream, l.text.as_str())).collect();
            assert_eq!(
                texts,
                vec![
                    (LogStreamKind::Stdout, "out"),
                    (LogStreamKind::Stderr, "err"),
                    (LogStreamKind::Stdout, "partial")
                ],
                "split {split}"
            );
        }
    }

    #[test]
    fn raw_tty_streams_and_trailing_text() {
        let mut d = LogStreamDecoder::new(DecoderMode::Automatic, false);
        let mut lines = d.feed(b"hello\nwor");
        lines.extend(d.finish());
        assert_eq!(
            lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["hello", "wor"]
        );
    }

    #[test]
    fn splits_timestamps() {
        let mut d = LogStreamDecoder::new(DecoderMode::Raw, true);
        let lines = d.feed(b"2024-05-01T12:34:56.5Z hi there\n");
        assert_eq!(lines[0].text, "hi there");
        assert!(lines[0].timestamp.is_some());
    }

    #[test]
    fn long_lines_are_cut_and_memory_stays_bounded() {
        let max = LogStreamDecoder::MAX_LINE_BYTES;
        for mode in [DecoderMode::Raw, DecoderMode::Multiplexed] {
            let mut d = LogStreamDecoder::new(mode, false);
            let chunk = vec![b'x'; 4096];
            let mut lines = Vec::new();
            // 1 MiB without a newline, in pieces.
            for _ in 0..256 {
                let bytes = if mode == DecoderMode::Raw {
                    chunk.clone()
                } else {
                    frame(1, std::str::from_utf8(&chunk).unwrap())
                };
                lines.extend(d.feed(&bytes));
                let held: usize = d.partial.values().map(|p| p.bytes.len()).sum();
                assert!(held <= max && d.buffer.len() <= 8 + 4096);
            }
            assert!(lines.is_empty());
            let tail = if mode == DecoderMode::Raw {
                b"end\nnext\n".to_vec()
            } else {
                frame(1, "end\nnext\n")
            };
            lines.extend(d.feed(&tail));
            assert_eq!(lines.len(), 2, "{mode:?}");
            assert!(lines[0].text.ends_with(LogStreamDecoder::TRUNCATION_MARK));
            assert_eq!(
                lines[0].text.len(),
                max + LogStreamDecoder::TRUNCATION_MARK.len()
            );
            assert_eq!(lines[1].text, "next");
        }
    }

    #[test]
    fn large_frames_are_decoded_as_they_arrive() {
        // A header announcing 1 GiB must not make the decoder wait for (or buffer) all of it.
        let mut d = LogStreamDecoder::new(DecoderMode::Multiplexed, false);
        let mut data = vec![2, 0, 0, 0];
        data.extend_from_slice(&(1u32 << 30).to_be_bytes());
        data.extend_from_slice(b"first\nsec");
        let lines = d.feed(&data);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].stream, LogStreamKind::Stderr);
        assert!(d.buffer.is_empty());
        let lines = d.feed(b"ond\n");
        assert_eq!(lines[0].text, "second");
        assert_eq!(lines[0].stream, LogStreamKind::Stderr);
    }

    #[test]
    fn cuts_never_split_a_character() {
        let max = LogStreamDecoder::MAX_LINE_BYTES;
        let mut text = "a".repeat(max - 1).into_bytes();
        text.extend_from_slice("é\n".as_bytes());
        let mut d = LogStreamDecoder::new(DecoderMode::Raw, false);
        let lines = d.feed(&text);
        assert!(!lines[0].text.contains('\u{FFFD}'));
        assert_eq!(
            lines[0].text,
            "a".repeat(max - 1) + LogStreamDecoder::TRUNCATION_MARK
        );
    }

    #[test]
    fn cleans_escape_sequences() {
        assert_eq!(
            clean(b"\x1b[31mred\x1b[0m\r\tx\x1b]0;title\x07!"),
            "red    x!"
        );
    }

    #[test]
    fn splits_json_lines() {
        let mut s = JsonLineSplitter::default();
        assert_eq!(
            s.feed(b"{\"a\":1}\n \r\n{\"b\"").unwrap(),
            vec![b"{\"a\":1}".to_vec()]
        );
        assert_eq!(s.feed(b":2}\n").unwrap(), vec![b"{\"b\":2}".to_vec()]);
        assert_eq!(s.finish(), None);
    }
}
