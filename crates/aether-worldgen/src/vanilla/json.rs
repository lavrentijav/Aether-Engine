//! A minimal JSON reader, just enough for the game's worldgen data files.
//!
//! The worldgen graph is *content*: it lives in the game's own `data/` tree and
//! is read at run time (see [`super`]). Reading it needs a parser, but pulling
//! a serialization framework into the generator for a handful of files that are
//! read once at start-up is not a trade worth making — so this is a small,
//! self-contained reader with no dependencies.
//!
//! It is deliberately strict about the subset it supports and forgiving about
//! nothing: any malformed input is an error rather than a silent default,
//! because a silently-wrong spline is exactly the kind of bug that would show
//! up a thousand blocks away as "terrain looks a bit off".

use std::collections::BTreeMap;
use std::fmt;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// Any JSON number, always kept as `f64` — worldgen reads them as doubles.
    Num(f64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object. Ordered by key; worldgen never depends on member order.
    Obj(BTreeMap<String, Json>),
}

/// What went wrong while reading a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset at which parsing gave up.
    pub at: usize,
    /// What was expected there.
    pub msg: String,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid JSON at byte {}: {}", self.at, self.msg)
    }
}

impl std::error::Error for JsonError {}

impl Json {
    /// Parse a whole document. Trailing content other than whitespace is an
    /// error.
    pub fn parse(src: &str) -> Result<Json, JsonError> {
        let mut p = Parser {
            b: src.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(p.err("trailing content after the top-level value"));
        }
        Ok(v)
    }

    /// The object member `key`, if this is an object that has one.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.get(key),
            _ => None,
        }
    }

    /// This value as a number, if it is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// This value as a string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// This value as an array, if it is one.
    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a.as_slice()),
            _ => None,
        }
    }

    /// This value as a boolean, if it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// This value as an object, if it is one.
    pub fn as_obj(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Json::Obj(m) => Some(m),
            _ => None,
        }
    }

    /// Member `key` as a number, or `default`.
    pub fn f64_or(&self, key: &str, default: f64) -> f64 {
        self.get(key).and_then(Json::as_f64).unwrap_or(default)
    }

    /// Member `key` as an integer, or `default`.
    pub fn i32_or(&self, key: &str, default: i32) -> i32 {
        self.get(key).and_then(Json::as_f64).map(|v| v as i32).unwrap_or(default)
    }

    /// Member `key` as a boolean, or `default`.
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Json::as_bool).unwrap_or(default)
    }

    /// Member `key` as a string.
    pub fn str_of(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> JsonError {
        JsonError {
            at: self.i,
            msg: msg.to_string(),
        }
    }

    fn ws(&mut self) {
        while let Some(c) = self.b.get(self.i) {
            if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), JsonError> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected `{}`", c as char)))
        }
    }

    fn lit(&mut self, word: &str, v: Json) -> Result<Json, JsonError> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err(&format!("expected `{word}`")))
        }
    }

    fn value(&mut self) -> Result<Json, JsonError> {
        match self.b.get(self.i) {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(_) => self.number(),
        }
    }

    fn object(&mut self) -> Result<Json, JsonError> {
        self.eat(b'{')?;
        let mut m = BTreeMap::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Obj(m));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let v = self.value()?;
            m.insert(k, v);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(m));
                }
                _ => return Err(self.err("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self) -> Result<Json, JsonError> {
        self.eat(b'[')?;
        let mut a = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(a));
        }
        loop {
            self.ws();
            a.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(a));
                }
                _ => return Err(self.err("expected `,` or `]`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.eat(b'"')?;
        let mut s = String::new();
        loop {
            let c = *self.b.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
            self.i += 1;
            match c {
                b'"' => return Ok(s),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or_else(|| self.err("unterminated escape"))?;
                    self.i += 1;
                    match e {
                        b'"' => s.push('"'),
                        b'\\' => s.push('\\'),
                        b'/' => s.push('/'),
                        b'b' => s.push('\u{8}'),
                        b'f' => s.push('\u{c}'),
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            // Surrogate pairs: the low half follows as a second
                            // \u escape. Worldgen data is ASCII in practice, so
                            // this is here for correctness, not for use.
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.b.get(self.i) == Some(&b'\\')
                                    && self.b.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    let combined =
                                        0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    s.push(
                                        char::from_u32(combined)
                                            .ok_or_else(|| self.err("bad surrogate pair"))?,
                                    );
                                } else {
                                    return Err(self.err("lone high surrogate"));
                                }
                            } else {
                                s.push(
                                    char::from_u32(cp)
                                        .ok_or_else(|| self.err("bad \\u escape"))?,
                                );
                            }
                        }
                        _ => return Err(self.err("unknown escape")),
                    }
                }
                // Multi-byte UTF-8 passes through verbatim.
                _ => {
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    self.i = start + len;
                    let bytes = self
                        .b
                        .get(start..self.i)
                        .ok_or_else(|| self.err("truncated UTF-8"))?;
                    s.push_str(
                        std::str::from_utf8(bytes).map_err(|_| self.err("invalid UTF-8"))?,
                    );
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let bytes = self
            .b
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.err("truncated \\u escape"))?;
        let s = std::str::from_utf8(bytes).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(s, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while let Some(c) = self.b.get(self.i) {
            if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-') {
                self.i += 1;
            } else {
                break;
            }
        }
        if start == self.i {
            return Err(self.err("expected a value"));
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| self.err("bad number"))?;
        s.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| JsonError {
                at: start,
                msg: format!("`{s}` is not a number"),
            })
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shapes_worldgen_uses() {
        let v = Json::parse(
            r#"{"type":"minecraft:add","argument1":0.5,"argument2":[-1.0,2,3e2],"f":false,"n":null}"#,
        )
        .unwrap();
        assert_eq!(v.get("type").unwrap().as_str(), Some("minecraft:add"));
        assert_eq!(v.get("argument1").unwrap().as_f64(), Some(0.5));
        let a = v.get("argument2").unwrap().as_arr().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].as_f64(), Some(-1.0));
        assert_eq!(a[2].as_f64(), Some(300.0));
        assert_eq!(v.get("f"), Some(&Json::Bool(false)));
        assert_eq!(v.get("n"), Some(&Json::Null));
    }

    #[test]
    fn a_bare_number_is_a_document() {
        // `zero.json` really is just `0.0`.
        assert_eq!(Json::parse("0.0").unwrap().as_f64(), Some(0.0));
    }

    #[test]
    fn escapes_and_unicode_round_trip() {
        let v = Json::parse(r#"{"k":"a\"b\\c\néA"}"#).unwrap();
        assert_eq!(v.get("k").unwrap().as_str(), Some("a\"b\\c\néA"));
    }

    #[test]
    fn malformed_input_is_an_error_not_a_default() {
        for bad in [
            "{",
            "{\"a\"}",
            "[1,]",
            "tru",
            "{\"a\":1} junk",
            "\"unterminated",
        ] {
            assert!(Json::parse(bad).is_err(), "accepted malformed input {bad:?}");
        }
    }
}
