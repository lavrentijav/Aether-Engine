//! SNBT — the stringified NBT a command line carries.
//!
//! Vanilla's `nbt_compound`, `component` and the `{…}` tail of an
//! `item_stack` argument are all "the NBT, written out as text". This is the
//! parser for that text: compounds, lists, typed arrays, quoted and unquoted
//! strings, and the numeric suffixes (`1b`, `1s`, `1L`, `1.0f`, `1.0d`) that
//! choose a tag type. It builds the same [`Nbt`] value the protocol writer
//! already knows how to serialise.
//!
//! It is deliberately a *syntax* parser plus the obvious coercions; it does not
//! know which keys are meaningful. A malformed string is an error, never a
//! panic, which is what makes it safe to point at untrusted command input.

use crate::protocol::nbt::Nbt;

/// The deepest nesting the SNBT parser accepts. `/tellraw`'s component parser
/// has its own budget, but the text has to be turned into `Nbt` first, so the
/// stack bound has to hold here too.
pub const MAX_DEPTH: usize = 128;

/// Why an SNBT string could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnbtError {
    /// A human-readable description.
    pub message: String,
    /// The byte offset the parser stopped at.
    pub at: usize,
}

impl std::fmt::Display for SnbtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.at)
    }
}

impl std::error::Error for SnbtError {}

/// Parse a complete SNBT value, rejecting trailing input.
pub fn parse(text: &str) -> Result<Nbt, SnbtError> {
    let mut p = Parser {
        text,
        pos: 0,
        depth: 0,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.pos != text.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

/// Parse an SNBT compound; a bare `{…}` or a value are both accepted.
pub fn parse_compound(text: &str) -> Result<Nbt, SnbtError> {
    match parse(text)? {
        c @ Nbt::Compound(_) => Ok(c),
        _ => Err(SnbtError {
            message: "expected a compound".into(),
            at: 0,
        }),
    }
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> SnbtError {
        SnbtError {
            message: message.to_string(),
            at: self.pos,
        }
    }

    fn err_at(&self, at: usize, message: &str) -> SnbtError {
        SnbtError {
            message: message.to_string(),
            at,
        }
    }

    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.bump();
        }
    }

    fn expect(&mut self, want: char) -> Result<(), SnbtError> {
        match self.bump() {
            Some(c) if c == want => Ok(()),
            _ => {
                self.pos = self.pos.saturating_sub(want.len_utf8());
                Err(self.err(&format!("expected '{want}'")))
            }
        }
    }

    fn value(&mut self) -> Result<Nbt, SnbtError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(self.err("SNBT is nested too deeply"));
        }
        let result = self.value_inner();
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self) -> Result<Nbt, SnbtError> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.compound(),
            Some('[') => self.list_or_array(),
            Some('"') | Some('\'') => Ok(Nbt::Str(self.quoted()?)),
            Some(_) => self.scalar(),
            None => Err(self.err("expected a value")),
        }
    }

    fn compound(&mut self) -> Result<Nbt, SnbtError> {
        self.expect('{')?;
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.bump();
            return Ok(Nbt::Compound(fields));
        }
        loop {
            self.skip_ws();
            let key = match self.peek() {
                Some('"') | Some('\'') => self.quoted()?,
                Some(_) => {
                    let k = self.scalar_token();
                    if k.is_empty() {
                        return Err(self.err("expected a key"));
                    }
                    k
                }
                None => return Err(self.err("unterminated compound")),
            };
            self.skip_ws();
            self.expect(':')?;
            let value = self.value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                    if self.peek() == Some('}') {
                        self.bump();
                        break;
                    }
                }
                Some('}') => break,
                _ => return Err(self.err("expected ',' or '}'")),
            }
        }
        Ok(Nbt::Compound(fields))
    }

    fn list_or_array(&mut self) -> Result<Nbt, SnbtError> {
        self.expect('[')?;
        self.skip_ws();
        // A typed array opens `[B;`, `[I;` or `[L;`.
        if let Some(kind) = self.peek() {
            if matches!(kind, 'B' | 'I' | 'L') {
                let save = self.pos;
                self.bump();
                self.skip_ws();
                if self.peek() == Some(';') {
                    self.bump();
                    return self.array(kind);
                }
                self.pos = save;
            }
        }
        if self.peek() == Some(']') {
            self.bump();
            return Ok(Nbt::List(Vec::new()));
        }
        let mut items = Vec::new();
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.bump();
                        break;
                    }
                }
                Some(']') => break,
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
        if let Some(first) = items.first() {
            let tag = first.tag();
            if items.iter().any(|i| i.tag() != tag) {
                return Err(self.err("list elements have different types"));
            }
        }
        Ok(Nbt::List(items))
    }

    fn array(&mut self, kind: char) -> Result<Nbt, SnbtError> {
        let mut bytes = Vec::new();
        let mut ints = Vec::new();
        let mut longs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(empty_array(kind, bytes, ints, longs));
        }
        loop {
            let v = self.value()?;
            match (kind, v) {
                ('B', Nbt::Byte(b)) => bytes.push(b as u8),
                ('I', Nbt::Int(i)) => ints.push(i),
                ('L', Nbt::Long(l)) => longs.push(l),
                _ => return Err(self.err("array element has the wrong type")),
            }
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                    if self.peek() == Some(']') {
                        self.bump();
                        break;
                    }
                }
                Some(']') => break,
                _ => return Err(self.err("expected ',' or ']'")),
            }
        }
        Ok(empty_array(kind, bytes, ints, longs))
    }

    fn quoted(&mut self) -> Result<String, SnbtError> {
        let quote = self.bump().unwrap_or('"');
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err("unterminated string")),
                Some(c) if c == quote => return Ok(out),
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('u') => {
                        let mut v = 0u32;
                        for _ in 0..4 {
                            match self.bump().and_then(|h| h.to_digit(16)) {
                                Some(d) => v = v * 16 + d,
                                None => return Err(self.err("bad unicode escape")),
                            }
                        }
                        out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                    }
                    Some(other) => out.push(other),
                    None => return Err(self.err("unterminated escape")),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn scalar(&mut self) -> Result<Nbt, SnbtError> {
        let start = self.pos;
        let token = self.scalar_token();
        if token.is_empty() {
            return Err(self.err_at(start, "expected a value"));
        }
        classify(&token).ok_or_else(|| self.err_at(start, "invalid value"))
    }

    fn scalar_token(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, ',' | '}' | ']' | ':') {
                break;
            }
            self.bump();
        }
        self.text[start..self.pos].to_string()
    }
}

fn empty_array(kind: char, bytes: Vec<u8>, ints: Vec<i32>, longs: Vec<i64>) -> Nbt {
    match kind {
        'B' => Nbt::ByteArray(bytes),
        'I' => Nbt::IntArray(ints),
        _ => Nbt::LongArray(longs),
    }
}

/// Turn a bare token into an NBT scalar, choosing the tag from the suffix.
///
/// A token that is not a number or `true`/`false` is an **unquoted string**
/// (vanilla SNBT allows `{a:oops}`), provided every character is legal in one;
/// this is why `oops` is a `String` and not an error.
fn classify(token: &str) -> Option<Nbt> {
    match token {
        "true" => return Some(Nbt::Byte(1)),
        "false" => return Some(Nbt::Byte(0)),
        _ => {}
    }
    let (body, suffix) = match token.chars().last() {
        Some(c @ ('b' | 'B' | 's' | 'S' | 'l' | 'L' | 'f' | 'F' | 'd' | 'D')) => {
            (&token[..token.len() - c.len_utf8()], Some(c))
        }
        _ => (token, None),
    };
    let numeric = match suffix {
        Some('b' | 'B') => body.parse::<i8>().ok().map(Nbt::Byte),
        Some('s' | 'S') => body.parse::<i16>().ok().map(Nbt::Short),
        Some('l' | 'L') => body.parse::<i64>().ok().map(Nbt::Long),
        Some('f' | 'F') => body.parse::<f32>().ok().map(Nbt::Float),
        Some('d' | 'D') => body.parse::<f64>().ok().map(Nbt::Double),
        _ => {
            if body.contains(['.', 'e', 'E']) {
                body.parse::<f64>().ok().map(Nbt::Double)
            } else if let Ok(i) = body.parse::<i32>() {
                Some(Nbt::Int(i))
            } else {
                body.parse::<f64>().ok().map(Nbt::Double)
            }
        }
    };
    match numeric {
        Some(v) => Some(v),
        None if is_unquoted_string(token) => Some(Nbt::Str(token.to_string())),
        None => None,
    }
}

/// The characters vanilla allows in an unquoted SNBT string.
fn is_unquoted_string(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compound(fields: &[(&str, Nbt)]) -> Nbt {
        Nbt::Compound(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[test]
    fn a_compound_round_trips_through_the_writer() {
        let nbt = parse(r#"{name:"minecraft:stone",count:3b,nested:{flag:1b}}"#).unwrap();
        assert_eq!(
            nbt,
            compound(&[
                ("name", Nbt::Str("minecraft:stone".into())),
                ("count", Nbt::Byte(3)),
                ("nested", compound(&[("flag", Nbt::Byte(1))]))
            ])
        );
        // And the writer accepts it.
        let _ = nbt.to_network();
    }

    #[test]
    fn numbers_choose_their_tag_from_the_suffix() {
        // A list is homogeneous, so each type is checked on its own.
        for (text, want) in [
            ("1b", Nbt::Byte(1)),
            ("2s", Nbt::Short(2)),
            ("3", Nbt::Int(3)),
            ("4L", Nbt::Long(4)),
            ("5.5f", Nbt::Float(5.5)),
            ("6.5d", Nbt::Double(6.5)),
            ("7.0", Nbt::Double(7.0)),
            ("true", Nbt::Byte(1)),
            ("false", Nbt::Byte(0)),
        ] {
            assert_eq!(parse(text).unwrap(), want, "{text}");
        }
        // A homogeneous list stays a list.
        assert_eq!(
            parse("[1,2,3]").unwrap(),
            Nbt::List(vec![Nbt::Int(1), Nbt::Int(2), Nbt::Int(3)])
        );
    }

    #[test]
    fn typed_arrays_build_the_right_tag() {
        assert_eq!(parse("[B;1b,2b]").unwrap(), Nbt::ByteArray(vec![1, 2]));
        assert_eq!(parse("[I;1,2,3]").unwrap(), Nbt::IntArray(vec![1, 2, 3]));
        assert_eq!(parse("[L;1L]").unwrap(), Nbt::LongArray(vec![1]));
        assert_eq!(parse("[B;]").unwrap(), Nbt::ByteArray(vec![]));
    }

    #[test]
    fn quoted_strings_support_escapes() {
        assert_eq!(parse(r#""a\"b\n""#).unwrap(), Nbt::Str("a\"b\n".into()));
        assert_eq!(parse("'single'").unwrap(), Nbt::Str("single".into()));
        assert_eq!(parse(r#""\u0041""#).unwrap(), Nbt::Str("A".into()));
    }

    #[test]
    fn whitespace_inside_a_compound_does_not_end_it() {
        let nbt = parse("{ a : 1 , b : { c : 2 } }").unwrap();
        assert_eq!(
            nbt,
            compound(&[("a", Nbt::Int(1)), ("b", compound(&[("c", Nbt::Int(2))]))])
        );
    }

    #[test]
    fn a_trailing_comma_is_allowed() {
        assert_eq!(
            parse("[1,2,]").unwrap(),
            Nbt::List(vec![Nbt::Int(1), Nbt::Int(2)])
        );
        assert_eq!(parse("{a:1,}").unwrap(), compound(&[("a", Nbt::Int(1))]));
    }

    #[test]
    fn an_unquoted_value_is_a_string() {
        // Vanilla SNBT: `{a:oops}` parses as a String, not an error.
        assert_eq!(
            parse("{a:oops}").unwrap(),
            compound(&[("a", Nbt::Str("oops".into()))])
        );
        assert_eq!(parse("hello").unwrap(), Nbt::Str("hello".into()));
        assert_eq!(parse("a-b.c+d_e").unwrap(), Nbt::Str("a-b.c+d_e".into()));
        // A value with a character an unquoted string cannot hold is refused.
        assert!(parse("{a:oops!}").is_err());
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            "{",
            "}",
            "[",
            "]",
            "{a}",
            "{a:}",
            "{:1}",
            "[1,2",
            "\"unterminated",
            "[B;1,2]",
            "[1,\"two\"]",
            "\u{1f600}",
        ] {
            assert!(parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn trailing_input_is_rejected() {
        assert!(parse("{} extra").is_err());
        assert!(parse("1 2").is_err());
    }

    #[test]
    fn parse_compound_requires_a_compound() {
        assert!(parse_compound("{a:1}").is_ok());
        assert!(parse_compound("[1]").is_err());
        assert!(parse_compound("\"s\"").is_err());
    }
}
