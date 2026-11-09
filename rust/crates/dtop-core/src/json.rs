//! A small JSON parser and value type. ADR 0001 rules out dependencies, so this replaces
//! `serde_json`. Docker omits fields, sends `null` and sends numbers as strings depending on the
//! API version, so the accessors are lenient: a missing or mistyped field reads as `None`.

use std::fmt;

/// A JSON number, keeping integer precision (memory limits exceed `f64`'s 53-bit mantissa).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    Int(i64),
    UInt(u64),
    Float(f64),
}

impl Number {
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Number::Int(v) => Some(v),
            Number::UInt(v) => Some(i64::try_from(v).unwrap_or(i64::MAX)),
            Number::Float(v) if v.is_finite() => Some(v as i64),
            Number::Float(_) => None,
        }
    }

    pub fn as_u64(self) -> Option<u64> {
        match self {
            Number::Int(v) => u64::try_from(v).ok(),
            Number::UInt(v) => Some(v),
            Number::Float(v) if v.is_finite() && v >= 0.0 => Some(v as u64),
            Number::Float(_) => None,
        }
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Number::Int(v) => v as f64,
            Number::UInt(v) => v as f64,
            Number::Float(v) => v,
        }
    }
}

/// A parsed JSON value. Objects keep their key order.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

static NULL: Value = Value::Null;

impl Value {
    /// The member `key` of an object, or `Null`.
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Object(members) => members
                .iter()
                .find(|(k, _)| k == key)
                .map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(members) => Some(members),
            _ => None,
        }
    }

    /// A string, or `default` when missing or not a string.
    pub fn str_or(&self, key: &str, default: &str) -> String {
        self.get(key).as_str().unwrap_or(default).to_string()
    }

    pub fn opt_str(&self, key: &str) -> Option<String> {
        self.get(key).as_str().map(str::to_string)
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).as_bool().unwrap_or(default)
    }

    /// Integers that some daemons send as floats or strings.
    pub fn flexible_int(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    /// Non-negative integers (floats accepted; strings are not, as in the Swift decoder).
    pub fn flexible_uint(&self, key: &str) -> Option<u64> {
        match self.get(key) {
            Value::Number(n) => n.as_u64(),
            _ => None,
        }
    }

    /// An array of strings; the whole array is rejected if any element is not a string.
    pub fn string_array(&self, key: &str) -> Vec<String> {
        self.get(key).as_string_list().unwrap_or_default()
    }

    pub fn as_string_list(&self) -> Option<Vec<String>> {
        self.as_array()?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect()
    }

    /// A string-to-string map; rejected entirely if any value is not a string.
    pub fn string_map(&self, key: &str) -> Vec<(String, String)> {
        let Some(members) = self.get(key).as_object() else {
            return Vec::new();
        };
        members
            .iter()
            .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default()
    }

    /// Serializes compactly.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        write_value(self, &mut out);
        out
    }
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(Number::Int(v)) => out.push_str(&v.to_string()),
        Value::Number(Number::UInt(v)) => out.push_str(&v.to_string()),
        Value::Number(Number::Float(v)) => {
            if v.is_finite() {
                out.push_str(&v.to_string());
            } else {
                out.push_str("null");
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (i, (k, v)) in members.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_value(v, out);
            }
            out.push('}');
        }
    }
}

/// Writes `s` as a JSON string literal.
pub fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Why a document failed to parse, with the byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Parses a complete JSON document.
pub fn parse(bytes: &[u8]) -> Result<Value, ParseError> {
    let mut parser = Parser {
        bytes,
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != bytes.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(value)
}

pub fn parse_str(text: &str) -> Result<Value, ParseError> {
    parse(text.as_bytes())
}

const MAX_DEPTH: usize = 256;

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> ParseError {
        ParseError {
            offset: self.pos,
            message: message.to_string(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect_literal(&mut self, literal: &[u8], value: Value) -> Result<Value, ParseError> {
        if self.bytes[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'n') => self.expect_literal(b"null", Value::Null),
            Some(b't') => self.expect_literal(b"true", Value::Bool(true)),
            Some(b'f') => self.expect_literal(b"false", Value::Bool(false)),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        Ok(())
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        self.enter()?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
        self.depth -= 1;
        Ok(Value::Array(items))
    }

    fn object(&mut self) -> Result<Value, ParseError> {
        self.enter()?;
        self.pos += 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value()?;
            // Later duplicates win, like most decoders.
            if let Some(existing) = members.iter_mut().find(|(k, _)| *k == key) {
                existing.1 = value;
            } else {
                members.push((key, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
        self.depth -= 1;
        Ok(Value::Object(members))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| self.error("short \\u escape"))?;
        let text = std::str::from_utf8(slice).map_err(|_| self.error("invalid \\u escape"))?;
        let code = u32::from_str_radix(text, 16).map_err(|_| self.error("invalid \\u escape"))?;
        self.pos += 4;
        Ok(code)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            self.pos += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let Some(escape) = self.peek() else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.pos += 1;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code)
                                && self.bytes[self.pos..].starts_with(b"\\u")
                            {
                                self.pos += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    push_char(&mut out, char::REPLACEMENT_CHARACTER);
                                    code = low;
                                }
                            }
                            push_char(
                                &mut out,
                                char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER),
                            );
                        }
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                byte if byte < 0x20 => return Err(self.error("control character in string")),
                byte => out.push(byte),
            }
        }
        Ok(String::from_utf8(out)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let mut is_float = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => {}
                b'.' | b'e' | b'E' | b'+' | b'-' => is_float = true,
                _ => break,
            }
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.error("invalid number"))?;
        if !is_float {
            if let Ok(v) = text.parse::<i64>() {
                return Ok(Value::Number(Number::Int(v)));
            }
            if let Ok(v) = text.parse::<u64>() {
                return Ok(Value::Number(Number::UInt(v)));
            }
        }
        text.parse::<f64>()
            .map(|v| Value::Number(Number::Float(v)))
            .map_err(|_| ParseError {
                offset: start,
                message: "invalid number".to_string(),
            })
    }
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_documents() {
        let v = parse_str(r#"{"a":[1,-2,3.5,"x",true,null],"b":{"c":"d"}}"#).unwrap();
        assert_eq!(v.get("a").as_array().unwrap().len(), 6);
        assert_eq!(v.get("b").get("c").as_str(), Some("d"));
    }

    #[test]
    fn keeps_u64_precision() {
        let v = parse_str(r#"{"limit":18446744073709551615}"#).unwrap();
        assert_eq!(v.flexible_uint("limit"), Some(u64::MAX));
    }

    #[test]
    fn flexible_int_accepts_strings_and_floats() {
        let v = parse_str(r#"{"a":"42","b":7.0,"c":null}"#).unwrap();
        assert_eq!(v.flexible_int("a"), Some(42));
        assert_eq!(v.flexible_int("b"), Some(7));
        assert_eq!(v.flexible_int("c"), None);
    }

    #[test]
    fn decodes_escapes_and_surrogates() {
        let v = parse_str(r#""a\né😀""#).unwrap();
        assert_eq!(v.as_str(), Some("a\né😀"));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse_str("{} x").is_err());
        assert!(parse_str("[1,]").is_err());
    }

    #[test]
    fn round_trips() {
        let text = r#"{"a":[1,"x\"y"],"b":null}"#;
        assert_eq!(parse_str(text).unwrap().to_json(), text);
    }
}
