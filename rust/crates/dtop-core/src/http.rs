//! A minimal blocking HTTP/1.1 client: one request per connection, an incremental response
//! parser (Content-Length, chunked, read-until-close) and streaming with interruption.

use std::sync::Arc;

use crate::error::{DockerError, Result};
use crate::transport::{Connection, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Head,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
        }
    }
}

/// Ordered, case-insensitive header list.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    pub entries: Vec<(String, String)>,
}

impl PartialEq for Headers {
    /// Names compare case-insensitively (RFC 9110 §5.1); values and order must match.
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|(a, b)| a.0.eq_ignore_ascii_case(&b.0) && a.1 == b.1)
    }
}

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn values(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn add(&mut self, name: &str, value: &str) {
        self.entries.push((name.to_string(), value.to_string()));
    }

    /// Replaces every header with this name.
    pub fn set(&mut self, name: &str, value: &str) {
        self.entries.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.add(name, value);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    /// Path including the query string, e.g. `/v1.43/containers/json?all=1`.
    pub target: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: Method, target: impl Into<String>) -> Self {
        Request {
            method,
            target: target.into(),
            headers: Headers::default(),
            body: Vec::new(),
        }
    }

    /// Builds a target from a path and ordered query items; `None` values are omitted.
    pub fn target(path: &str, query: &[(&str, Option<String>)]) -> String {
        let items: Vec<String> = query
            .iter()
            .filter_map(|(k, v)| {
                v.as_ref()
                    .map(|v| format!("{}={}", percent_encode(k), percent_encode(v)))
            })
            .collect();
        if items.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{}", items.join("&"))
        }
    }

    /// Serializes to wire format. Always asks the daemon to close the connection: one request
    /// per connection, and EOF terminates until-close bodies.
    pub fn serialized(&self, host: &str) -> Vec<u8> {
        let mut headers = self.headers.clone();
        if headers.get("Host").is_none() {
            headers.set("Host", host);
        }
        if headers.get("User-Agent").is_none() {
            headers.set("User-Agent", "dtop");
        }
        if headers.get("Connection").is_none() {
            headers.set("Connection", "close");
        }
        if !self.body.is_empty() || matches!(self.method, Method::Post | Method::Put) {
            headers.set("Content-Length", &self.body.len().to_string());
        }
        let mut head = format!("{} {} HTTP/1.1\r\n", self.method.as_str(), self.target);
        for (name, value) in &headers.entries {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

/// RFC 3986 percent-encoding that leaves only unreserved characters untouched.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseHead {
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
}

impl ResponseHead {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub head: ResponseHead,
    pub body: Vec<u8>,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.head.status
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParserEvent {
    Head(ResponseHead),
    Body(Vec<u8>),
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Head,
    Fixed(usize),
    ChunkSize,
    ChunkData(usize),
    ChunkDataCrlf,
    Trailers,
    UntilClose,
    Done,
}

/// Incremental response parser: feed bytes as they arrive; it emits the head once, then body
/// fragments (already de-chunked), then `End`.
#[derive(Debug, Clone)]
pub struct ResponseParser {
    state: State,
    buffer: Vec<u8>,
    is_head_request: bool,
}

impl ResponseParser {
    /// Upper bound on header size, so a misbehaving peer cannot make us buffer forever.
    pub const MAX_HEAD_SIZE: usize = 64 * 1024;

    pub fn new(is_head_request: bool) -> Self {
        ResponseParser {
            state: State::Head,
            buffer: Vec::new(),
            is_head_request,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.state == State::Done
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ParserEvent>> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while self.step(&mut events)? {}
        Ok(events)
    }

    /// Call when the connection reports EOF.
    pub fn finish(&mut self) -> Result<Vec<ParserEvent>> {
        match self.state {
            State::Done => Ok(Vec::new()),
            State::UntilClose => {
                let mut events = Vec::new();
                if !self.buffer.is_empty() {
                    events.push(ParserEvent::Body(std::mem::take(&mut self.buffer)));
                }
                self.state = State::Done;
                events.push(ParserEvent::End);
                Ok(events)
            }
            State::Head => Err(DockerError::MalformedResponse(if self.buffer.is_empty() {
                "connection closed before any response was received".into()
            } else {
                "connection closed in the middle of the response headers".into()
            })),
            _ => Err(DockerError::MalformedResponse(
                "connection closed before the response body was complete".into(),
            )),
        }
    }

    fn step(&mut self, events: &mut Vec<ParserEvent>) -> Result<bool> {
        match self.state {
            State::Head => {
                let Some(end) = find(b"\r\n\r\n", &self.buffer) else {
                    if self.buffer.len() > Self::MAX_HEAD_SIZE {
                        return Err(DockerError::MalformedResponse(format!(
                            "response headers exceed {} bytes",
                            Self::MAX_HEAD_SIZE
                        )));
                    }
                    return Ok(false);
                };
                let head = parse_head(&self.buffer[..end])?;
                self.buffer.drain(..end + 4);
                self.state = self.body_state(&head)?;
                events.push(ParserEvent::Head(head));
                if self.state == State::Done {
                    events.push(ParserEvent::End);
                }
                Ok(true)
            }
            State::Fixed(remaining) => {
                if self.buffer.is_empty() {
                    return Ok(false);
                }
                let take = remaining.min(self.buffer.len());
                events.push(ParserEvent::Body(self.buffer.drain(..take).collect()));
                if remaining == take {
                    self.state = State::Done;
                    events.push(ParserEvent::End);
                } else {
                    self.state = State::Fixed(remaining - take);
                }
                Ok(true)
            }
            State::ChunkSize => {
                let Some(line_end) = find(b"\r\n", &self.buffer) else {
                    if self.buffer.len() > 1024 {
                        return Err(DockerError::MalformedResponse(
                            "chunk size line too long".into(),
                        ));
                    }
                    return Ok(false);
                };
                let line = String::from_utf8_lossy(&self.buffer[..line_end]).into_owned();
                self.buffer.drain(..line_end + 2);
                // Chunk extensions (";name=value") are legal and ignored.
                let size_text = line.split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(size_text, 16).map_err(|_| {
                    DockerError::MalformedResponse(format!("invalid chunk size '{line}'"))
                })?;
                self.state = if size == 0 {
                    State::Trailers
                } else {
                    State::ChunkData(size)
                };
                Ok(true)
            }
            State::ChunkData(remaining) => {
                if self.buffer.is_empty() {
                    return Ok(false);
                }
                let take = remaining.min(self.buffer.len());
                events.push(ParserEvent::Body(self.buffer.drain(..take).collect()));
                self.state = if remaining == take {
                    State::ChunkDataCrlf
                } else {
                    State::ChunkData(remaining - take)
                };
                Ok(true)
            }
            State::ChunkDataCrlf => {
                if self.buffer.len() < 2 {
                    return Ok(false);
                }
                if &self.buffer[..2] != b"\r\n" {
                    return Err(DockerError::MalformedResponse(
                        "missing CRLF after chunk data".into(),
                    ));
                }
                self.buffer.drain(..2);
                self.state = State::ChunkSize;
                Ok(true)
            }
            State::Trailers => {
                let Some(line_end) = find(b"\r\n", &self.buffer) else {
                    return Ok(false);
                };
                self.buffer.drain(..line_end + 2);
                if line_end == 0 {
                    self.state = State::Done;
                    events.push(ParserEvent::End);
                }
                Ok(true)
            }
            State::UntilClose => {
                if self.buffer.is_empty() {
                    return Ok(false);
                }
                events.push(ParserEvent::Body(std::mem::take(&mut self.buffer)));
                Ok(true)
            }
            State::Done => Ok(false),
        }
    }

    fn body_state(&self, head: &ResponseHead) -> Result<State> {
        if self.is_head_request
            || head.status == 204
            || head.status == 304
            || (100..200).contains(&head.status)
        {
            return Ok(State::Done);
        }
        if head
            .headers
            .get("Transfer-Encoding")
            .is_some_and(|e| e.to_lowercase().contains("chunked"))
        {
            return Ok(State::ChunkSize);
        }
        if let Some(text) = head.headers.get("Content-Length") {
            let length: usize = text.trim().parse().map_err(|_| {
                DockerError::MalformedResponse(format!("invalid Content-Length '{text}'"))
            })?;
            return Ok(if length == 0 {
                State::Done
            } else {
                State::Fixed(length)
            });
        }
        Ok(State::UntilClose)
    }
}

pub(crate) fn find(needle: &[u8], haystack: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Splits on CRLF, tolerating bare LF.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn parse_head(bytes: &[u8]) -> Result<ResponseHead> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let mut lines = split_lines(&text).into_iter();
    let status_line = lines
        .next()
        .ok_or_else(|| DockerError::MalformedResponse("empty status line".into()))?;
    let parts: Vec<&str> = status_line
        .splitn(3, ' ')
        .filter(|p| !p.is_empty())
        .collect();
    let invalid = || DockerError::MalformedResponse(format!("invalid status line '{status_line}'"));
    if parts.len() < 2 || !parts[0].starts_with("HTTP/") {
        return Err(invalid());
    }
    let status: u16 = parts[1].parse().map_err(|_| invalid())?;
    if !(100..=999).contains(&status) {
        return Err(invalid());
    }
    let reason = parts.get(2).map(|r| r.to_string()).unwrap_or_default();
    let mut headers = Headers::default();
    for line in lines.filter(|l| !l.is_empty()) {
        let colon = line.find(':').ok_or_else(|| {
            DockerError::MalformedResponse(format!("invalid header line '{line}'"))
        })?;
        let name = line[..colon].trim();
        if name.is_empty() {
            return Err(DockerError::MalformedResponse("empty header name".into()));
        }
        headers.add(name, line[colon + 1..].trim());
    }
    Ok(ResponseHead {
        status,
        reason,
        headers,
    })
}

/// Blocking HTTP client over a transport. One request per connection.
#[derive(Clone)]
pub struct HttpClient {
    pub transport: Arc<dyn Transport>,
    pub read_chunk_size: usize,
    /// Hard cap on buffered (non-streaming) response bodies.
    pub max_body_size: usize,
}

impl HttpClient {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        HttpClient {
            transport,
            read_chunk_size: 64 * 1024,
            max_body_size: 256 * 1024 * 1024,
        }
    }

    /// Sends a request and buffers the whole response.
    pub fn send(&self, request: &Request) -> Result<Response> {
        let mut head = None;
        let mut body = Vec::new();
        let max = self.max_body_size;
        self.stream(
            request,
            |_| {},
            |h| {
                head = Some(h.clone());
                Ok(())
            },
            |chunk| {
                body.extend_from_slice(chunk);
                if body.len() > max {
                    return Err(DockerError::MalformedResponse(format!(
                        "response body exceeds {max} bytes"
                    )));
                }
                Ok(true)
            },
        )?;
        let head = head.ok_or_else(|| DockerError::MalformedResponse("no response head".into()))?;
        Ok(Response { head, body })
    }

    /// Sends a request and delivers body fragments as they arrive. Return `Ok(false)` from
    /// `on_body` to stop early. `on_connect` hands out the live connection so another thread
    /// can interrupt it.
    pub fn stream(
        &self,
        request: &Request,
        mut on_connect: impl FnMut(Arc<dyn Connection>),
        mut on_head: impl FnMut(&ResponseHead) -> Result<()>,
        mut on_body: impl FnMut(&[u8]) -> Result<bool>,
    ) -> Result<()> {
        let connection = self.transport.connect()?;
        let _guard = CloseOnDrop(Arc::clone(&connection));
        on_connect(Arc::clone(&connection));
        connection.write_all(&request.serialized(&self.transport.endpoint().host_header()))?;
        let mut parser = ResponseParser::new(request.method == Method::Head);
        loop {
            let bytes = connection.read(self.read_chunk_size)?;
            let events = if bytes.is_empty() {
                parser.finish()?
            } else {
                parser.feed(&bytes)?
            };
            for event in events {
                match event {
                    ParserEvent::Head(head) => on_head(&head)?,
                    ParserEvent::Body(chunk) => {
                        if !on_body(&chunk)? {
                            return Ok(());
                        }
                    }
                    ParserEvent::End => return Ok(()),
                }
            }
            if bytes.is_empty() {
                return Ok(());
            }
        }
    }
}

struct CloseOnDrop(Arc<dyn Connection>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_split(response: &[u8], split: usize) -> (Vec<u8>, ResponseHead) {
        let mut parser = ResponseParser::new(false);
        let mut events = parser.feed(&response[..split]).unwrap();
        events.extend(parser.feed(&response[split..]).unwrap());
        events.extend(parser.finish().unwrap());
        let mut body = Vec::new();
        let mut head = None;
        let mut ended = 0;
        for event in events {
            match event {
                ParserEvent::Head(h) => head = Some(h),
                ParserEvent::Body(b) => body.extend(b),
                ParserEvent::End => ended += 1,
            }
        }
        assert_eq!(ended, 1, "exactly one End at split {split}");
        (body, head.unwrap())
    }

    #[test]
    fn parses_content_length_at_every_split_point() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX: y\r\n\r\nhello";
        for split in 0..=response.len() {
            let (body, head) = feed_split(response, split);
            assert_eq!(body, b"hello");
            assert_eq!(head.status, 200);
            assert_eq!(head.headers.get("x"), Some("y"));
        }
    }

    #[test]
    fn parses_chunked_at_every_split_point() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3;ext=1\r\nabc\r\n2\r\nde\r\n0\r\nT: x\r\n\r\n";
        for split in 0..=response.len() {
            assert_eq!(feed_split(response, split).0, b"abcde");
        }
    }

    #[test]
    fn reads_until_close() {
        let response = b"HTTP/1.0 200 OK\r\n\r\nstream";
        for split in 0..=response.len() {
            assert_eq!(feed_split(response, split).0, b"stream");
        }
    }

    #[test]
    fn no_body_for_204_and_head() {
        let mut parser = ResponseParser::new(false);
        let events = parser.feed(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!(events.last(), Some(&ParserEvent::End));
        let mut parser = ResponseParser::new(true);
        let events = parser
            .feed(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
            .unwrap();
        assert_eq!(events.last(), Some(&ParserEvent::End));
    }

    #[test]
    fn rejects_truncated_bodies() {
        let mut parser = ResponseParser::new(false);
        parser
            .feed(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
            .unwrap();
        assert!(parser.finish().is_err());
        let mut parser = ResponseParser::new(false);
        assert!(parser.finish().is_err());
    }

    #[test]
    fn rejects_bad_status_lines() {
        let mut parser = ResponseParser::new(false);
        assert!(parser.feed(b"FTP 200 OK\r\n\r\n").is_err());
    }

    #[test]
    fn serializes_with_default_headers() {
        let mut request = Request::new(Method::Post, "/x");
        request.headers.set("Content-Type", "application/json");
        let text = String::from_utf8(request.serialized("docker")).unwrap();
        assert!(text.starts_with("POST /x HTTP/1.1\r\n"));
        assert!(text.contains("Host: docker\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("Content-Length: 0\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    #[test]
    fn builds_targets_with_encoding() {
        let target = Request::target(
            "/p",
            &[
                ("a", Some("1 2".into())),
                ("b", None),
                ("c", Some("{\"x\"}".into())),
            ],
        );
        assert_eq!(target, "/p?a=1%202&c=%7B%22x%22%7D");
    }
}
