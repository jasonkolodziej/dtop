//! Docker's log stream format and newline-delimited JSON streams.

use std::collections::BTreeMap;

use crate::error::{DockerError, Result};
use crate::timestamp;

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
    /// When the daemon wrote the line (seconds since the epoch), for `timestamps=1` streams.
    pub timestamp: Option<f64>,
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

/// Decodes Docker's log/attach stream. Containers without a TTY multiplex stdout/stderr with
/// 8-byte frame headers (`[stream, 0, 0, 0, size(be32)]`); TTY containers send raw bytes.
/// Incremental, so it serves both buffered and followed logs.
#[derive(Debug, Clone)]
pub struct LogStreamDecoder {
    mode: DecoderMode,
    buffer: Vec<u8>,
    partial: BTreeMap<LogStreamKind, Vec<u8>>,
    timestamps: bool,
}

impl LogStreamDecoder {
    pub fn new(mode: DecoderMode, timestamps: bool) -> Self {
        LogStreamDecoder {
            mode,
            buffer: Vec::new(),
            partial: BTreeMap::new(),
            timestamps,
        }
    }

    fn make_line(&self, bytes: &[u8], stream: LogStreamKind) -> LogLine {
        if self.timestamps {
            if let Some(space) = bytes.iter().position(|&c| c == b' ').filter(|&s| s >= 20) {
                if let Some(time) = timestamp::parse(&String::from_utf8_lossy(&bytes[..space])) {
                    return LogLine {
                        stream,
                        text: clean(&bytes[space + 1..]),
                        timestamp: Some(time),
                    };
                }
            }
        }
        LogLine {
            stream,
            text: clean(bytes),
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
                while self.buffer.len() >= 8 {
                    let size = u32::from_be_bytes([
                        self.buffer[4],
                        self.buffer[5],
                        self.buffer[6],
                        self.buffer[7],
                    ]) as usize;
                    if self.buffer.len() < 8 + size {
                        break;
                    }
                    let stream = match self.buffer[0] {
                        0 => LogStreamKind::Stdin,
                        2 => LogStreamKind::Stderr,
                        _ => LogStreamKind::Stdout,
                    };
                    let payload: Vec<u8> = self.buffer[8..8 + size].to_vec();
                    self.buffer.drain(..8 + size);
                    self.append_text(&payload, stream, &mut lines);
                }
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
            .filter(|(_, bytes)| !bytes.is_empty())
            .map(|(stream, bytes)| self.make_line(&bytes, stream))
            .collect()
    }

    fn append_text(&mut self, bytes: &[u8], stream: LogStreamKind, lines: &mut Vec<LogLine>) {
        let mut pending = self.partial.remove(&stream).unwrap_or_default();
        for &byte in bytes {
            if byte == b'\n' {
                lines.push(self.make_line(&pending, stream));
                pending.clear();
            } else {
                pending.push(byte);
            }
        }
        self.partial.insert(stream, pending);
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
