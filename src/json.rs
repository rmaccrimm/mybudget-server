//! A strict JSON parser (RFC 8259) for request bodies.
//!
//! serde_json can't be used for requests: once parsed, it can't tell `1` from
//! `1.0` or `-0` from `-0.0` (SPEC 3.7 needs numbers as written), and with
//! the `raw_value` feature axum turns on, it treats some object keys
//! specially, while SPEC 3.13 lets an ignored field hold any JSON value.

use std::collections::BTreeMap;

pub type Object = BTreeMap<String, Json>;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number exactly as written in the body.
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Object),
}

/// Deepest nesting of arrays and objects accepted. SPEC 3.13 requires 64.
const MAX_DEPTH: usize = 128;

/// Parses a whole body. Fails on invalid UTF-8, any syntax error, an escaped
/// lone surrogate (SPEC 3.1), nesting deeper than [`MAX_DEPTH`], or a number
/// too large for a 64-bit float (SPEC 3.13).
pub fn parse(bytes: &[u8]) -> Result<Json, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the body isn't valid UTF-8".to_string())?;
    let mut p = Parser { text, bytes: text.as_bytes(), pos: 0 };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.error("unexpected characters after the JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, message: &str) -> String {
        format!("invalid JSON: {message} at byte {}", self.pos)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        let found = self.peek() == Some(b);
        if found {
            self.pos += 1;
        }
        found
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn skip_digits(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.pos += 1;
        let mut map = Object::new();
        self.skip_ws();
        if self.eat(b'}') {
            return Ok(Json::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a string key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(b':') {
                return Err(self.error("expected ':'"));
            }
            self.skip_ws();
            let value = self.value(depth)?;
            // A repeated key keeps its last value.
            map.insert(key, value);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Ok(Json::Object(map));
            }
            return Err(self.error("expected ',' or '}'"));
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']') {
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth)?);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(Json::Array(items));
            }
            return Err(self.error("expected ',' or ']'"));
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            // Both ends sit next to ASCII bytes, so they are char boundaries.
            out.push_str(&self.text[start..self.pos]);
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let c = self.escape()?;
                    out.push(c);
                }
                Some(_) => return Err(self.error("control character in string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let Some(b) = self.peek() else {
            return Err(self.error("unterminated string"));
        };
        self.pos += 1;
        Ok(match b {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let unit = self.hex4()?;
                match unit {
                    0xD800..=0xDBFF => {
                        if !(self.eat(b'\\') && self.eat(b'u')) {
                            return Err(self.error("lone surrogate in string"));
                        }
                        let low = self.hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return Err(self.error("lone surrogate in string"));
                        }
                        let scalar = 0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00);
                        char::from_u32(scalar).expect("a surrogate pair is a scalar value")
                    }
                    0xDC00..=0xDFFF => return Err(self.error("lone surrogate in string")),
                    _ => char::from_u32(unit).expect("a non-surrogate BMP code point is a scalar value"),
                }
            }
            _ => return Err(self.error("invalid escape")),
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(digits) = self.bytes.get(self.pos..self.pos + 4) else {
            return Err(self.error("invalid \\u escape"));
        };
        let mut value = 0;
        for &d in digits {
            let Some(h) = (d as char).to_digit(16) else {
                return Err(self.error("invalid \\u escape"));
            };
            value = value * 16 + h;
        }
        self.pos += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        self.eat(b'-');
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.skip_digits(),
            _ => return Err(self.error("invalid number")),
        }
        if self.eat(b'.') {
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error("invalid number"));
            }
            self.skip_digits();
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error("invalid number"));
            }
            self.skip_digits();
        }
        let raw = &self.text[start..self.pos];
        match raw.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Json::Number(raw.to_owned())),
            _ => Err(self.error("number out of range")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> Json {
        parse(s.as_bytes()).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn valid_documents() {
        assert_eq!(ok(" null "), Json::Null);
        assert_eq!(ok("-0"), Json::Number("-0".into()));
        assert_eq!(ok("1.5e3"), Json::Number("1.5e3".into()));
        assert_eq!(ok(r#""a\u00e9\ud83d\ude00\n\/""#), Json::String("aé😀\n/".into()));
        assert_eq!(
            ok("[1, [], {}]"),
            Json::Array(vec![Json::Number("1".into()), Json::Array(vec![]), Json::Object(Object::new())])
        );
        let Json::Object(o) = ok(r#"{"a": 1, "a": 2, "$serde_json::private::RawValue": "x"}"#) else { panic!() };
        assert_eq!(o["a"], Json::Number("2".into()));
        ok(&format!("{}{}", "[".repeat(128), "]".repeat(128)));
    }

    #[test]
    fn invalid_documents() {
        for s in [
            "",
            " ",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "01",
            "-",
            "1.",
            ".5",
            "1e",
            "+1",
            "tru",
            "nul",
            "\"\\x\"",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"\\ud800\\u0041\"",
            "\"a\u{1}\"",
            "1e400",
            "-1e400",
            "{} x",
            "\u{feff}{}",
            "{'a': 1}",
            "[1 2]",
            "{\"a\" 1}",
            "NaN",
            "Infinity",
        ] {
            assert!(parse(s.as_bytes()).is_err(), "{s:?} should fail");
        }
        assert!(parse(b"\"\xff\"").is_err());
        assert!(parse(format!("{}{}", "[".repeat(129), "]".repeat(129)).as_bytes()).is_err());
    }
}
