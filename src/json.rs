//! Minimal JSON handling for relayed messages.
//!
//! NVDA serializes with Python's `json.dumps` defaults, which emit lone UTF-16
//! surrogate escapes (`"\ud83d"`) and `NaN` / `Infinity` / `-Infinity`; serde_json
//! rejects both. So a line is only validated here, against the grammar Python's
//! `json.loads` accepts, and split into its top-level members; values are relayed
//! byte for byte, never re-encoded, so the relayed line is at most a few bytes
//! longer than the received one. Anything accepted here is accepted by NVDA's
//! `json.loads` (which matters: a parse error kills NVDA's receive thread).

use std::borrow::Cow;
use std::fmt::Write;
use std::sync::Arc;

/// Deeper nesting is rejected (NVDA messages nest 4 levels; Python's limit is ~1000).
const MAX_DEPTH: usize = 128;
/// Python refuses to convert integers with more digits (sys.int_info).
const MAX_INT_DIGITS: usize = 4300;

/// One top-level member of an object.
pub struct Field<'a> {
    /// Decoded name; None if it holds a lone surrogate (so equals no known name).
    pub name: Option<Cow<'a, str>>,
    /// The name as received, with its quotes.
    pub raw_name: &'a str,
    /// The value as received.
    pub value: &'a str,
}

/// Most top-level members a relayed message may have (NVDA uses about 15): each
/// costs a small allocation, so many tiny members must not multiply memory.
pub const MAX_MEMBERS: usize = 64;

/// Why a line is not a relayable object.
#[derive(Debug, PartialEq)]
pub enum Bad {
    /// Not valid (Python-flavoured) JSON, or not an object.
    Invalid,
    /// More than [`MAX_MEMBERS`] top-level members.
    TooManyMembers,
}

/// Splits a line holding one JSON object into its members.
pub fn object(line: &[u8]) -> Result<Vec<Field<'_>>, Bad> {
    let line = std::str::from_utf8(line).map_err(|_| Bad::Invalid)?;
    let mut p = Parser {
        s: line,
        pos: 0,
        too_many: false,
    };
    let mut fields = Vec::new();
    match p.members(&mut fields) {
        Some(()) => Ok(fields),
        None if p.too_many => Err(Bad::TooManyMembers),
        None => Err(Bad::Invalid),
    }
}

fn decode_name(raw: &str) -> Option<Cow<'_, str>> {
    if raw.contains('\\') {
        serde_json::from_str::<String>(raw).ok().map(Cow::Owned)
    } else {
        Some(Cow::Borrowed(&raw[1..raw.len() - 1]))
    }
}

/// The raw value of the last member named `key` (Python keeps the last duplicate).
pub fn get<'a>(fields: &[Field<'a>], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .rev()
        .find(|f| f.name.as_deref() == Some(key))
        .map(|f| f.value)
}

/// Decodes a raw value that is a string without lone surrogates.
pub fn string(raw: &str) -> Option<String> {
    serde_json::from_str(raw).ok()
}

/// Re-assembles a message for relaying, values unchanged. With an origin (protocol
/// version 2) any client `origin` is replaced by it; without (version 1) the
/// `origin`, `clients` and `client` fields are left out, as NVDA's server does.
pub fn relay_line(fields: &[Field], origin: Option<u64>) -> Arc<str> {
    let size: usize = fields
        .iter()
        .map(|f| f.raw_name.len() + f.value.len() + 2)
        .sum();
    let mut out = String::with_capacity(size + 32);
    out.push('{');
    if let Some(id) = origin {
        let _ = write!(out, "\"origin\":{id}");
    }
    let dropped: &[&str] = match origin {
        Some(_) => &["origin"],
        None => &["origin", "clients", "client"],
    };
    for f in fields {
        if f.name.as_deref().is_some_and(|n| dropped.contains(&n)) {
            continue;
        }
        if out.len() > 1 {
            out.push(',');
        }
        out.push_str(f.raw_name);
        out.push(':');
        out.push_str(f.value);
    }
    out.push_str("}\n");
    out.into()
}

struct Parser<'a> {
    s: &'a str,
    pos: usize,
    too_many: bool,
}

impl<'a> Parser<'a> {
    fn members(&mut self, fields: &mut Vec<Field<'a>>) -> Option<()> {
        let line = self.s;
        self.ws();
        self.eat(b'{')?;
        self.ws();
        if self.eat(b'}').is_none() {
            loop {
                if fields.len() == MAX_MEMBERS {
                    self.too_many = true;
                    return None;
                }
                self.ws();
                let start = self.pos;
                self.string()?;
                let raw_name = &line[start..self.pos];
                self.ws();
                self.eat(b':')?;
                self.ws();
                let start = self.pos;
                self.value(1)?;
                fields.push(Field {
                    name: decode_name(raw_name),
                    raw_name,
                    value: &line[start..self.pos],
                });
                self.ws();
                if self.eat(b',').is_none() {
                    self.eat(b'}')?;
                    break;
                }
            }
        }
        self.ws();
        (self.pos == line.len()).then_some(())
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek() == Some(b)).then(|| self.pos += 1)
    }

    fn word(&mut self, w: &str) -> Option<()> {
        self.s[self.pos..]
            .starts_with(w)
            .then(|| self.pos += w.len())
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        self.pos - start
    }

    /// A string; escapes are checked for form only, so lone surrogates pass.
    fn string(&mut self) -> Option<()> {
        self.eat(b'"')?;
        loop {
            match self.next()? {
                b'"' => return Some(()),
                b'\\' => match self.next()? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                    b'u' => {
                        for _ in 0..4 {
                            self.next().filter(u8::is_ascii_hexdigit)?;
                        }
                    }
                    _ => return None,
                },
                c if c < 0x20 => return None,
                _ => {}
            }
        }
    }

    /// A number as Python reads it, plus NaN, Infinity and -Infinity.
    fn number(&mut self) -> Option<()> {
        let negative = self.eat(b'-').is_some();
        if self.word("Infinity").is_some() {
            return Some(());
        }
        if !negative && self.word("NaN").is_some() {
            return Some(());
        }
        let int = if self.eat(b'0').is_some() {
            1
        } else {
            match self.digits() {
                0 => return None,
                n => n,
            }
        };
        let mut float = false;
        if self.eat(b'.').is_some() {
            float = true;
            (self.digits() > 0).then_some(())?;
        }
        if let Some(b'e' | b'E') = self.peek() {
            float = true;
            self.pos += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.pos += 1;
            }
            (self.digits() > 0).then_some(())?;
        }
        (float || int <= MAX_INT_DIGITS).then_some(())
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => {
                self.pos += 1;
                self.ws();
                if self.eat(b'}').is_some() {
                    return Some(());
                }
                loop {
                    self.ws();
                    self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    self.value(depth + 1)?;
                    self.ws();
                    if self.eat(b',').is_none() {
                        return self.eat(b'}');
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                self.ws();
                if self.eat(b']').is_some() {
                    return Some(());
                }
                loop {
                    self.ws();
                    self.value(depth + 1)?;
                    self.ws();
                    if self.eat(b',').is_none() {
                        return self.eat(b']');
                    }
                }
            }
            b'"' => self.string(),
            b't' => self.word("true"),
            b'f' => self.word("false"),
            b'n' => self.word("null"),
            _ => self.number(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay(line: &str, origin: Option<u64>) -> Option<String> {
        object(line.as_bytes())
            .ok()
            .map(|f| relay_line(&f, origin).to_string())
    }

    #[test]
    fn relays_nvda_surrogates_and_non_finite_numbers_verbatim() {
        // Exact bytes Python's json.dumps produces for these NVDA payloads.
        let line = r#"{"sequence": ["a\ud83d", "\ude00b", "\ud83d\ude00"], "rate": NaN, "x": [Infinity, -Infinity, 1E15, 1.5e-3], "type": "speak"}"#;
        assert_eq!(
            relay(line, Some(7)).unwrap(),
            r#"{"origin":7,"sequence":["a\ud83d", "\ude00b", "\ud83d\ude00"],"rate":NaN,"x":[Infinity, -Infinity, 1E15, 1.5e-3],"type":"speak"}"#
                .to_owned()
                + "\n"
        );
    }

    #[test]
    fn replaces_origin_and_strips_v1_fields() {
        let line = r#"{"type":"x","origin":5,"orig\u0069n":6,"client":{},"clients":[]}"#;
        assert_eq!(
            relay(line, Some(1)).unwrap(),
            "{\"origin\":1,\"type\":\"x\",\"client\":{},\"clients\":[]}\n"
        );
        assert_eq!(relay(line, None).unwrap(), "{\"type\":\"x\"}\n");
        assert_eq!(relay("{}", Some(3)).unwrap(), "{\"origin\":3}\n");
        assert_eq!(relay(" { } ", None).unwrap(), "{}\n");
    }

    #[test]
    fn relayed_line_is_never_much_longer_than_the_input() {
        let line = format!(r#"{{"type":"speak","n":[{}]}}"#, ["1E15"; 1000].join(","));
        let out = relay(&line, Some(u64::MAX)).unwrap();
        assert!(
            out.len() <= line.len() + 32,
            "{} > {}",
            out.len(),
            line.len()
        );
    }

    #[test]
    fn decodes_names_and_keeps_the_last_duplicate() {
        let f = object(br#"{"type":"a","t\u0079pe":"b","\ud800":1}"#).unwrap();
        assert_eq!(get(&f, "type"), Some("\"b\""));
        assert!(f[2].name.is_none());
        assert_eq!(string("\"\\ud83d\\ude00\"").as_deref(), Some("\u{1F600}"));
        assert_eq!(string("\"\\ud83d\""), None);
    }

    #[test]
    fn accepts_what_python_accepts() {
        for ok in [
            r#"{"a":[],"b":{},"c":[1,-0,0.5,-1.25E+2,true,false,null,"\"\\\/\b\f\n\r\t\u00e9é"]}"#,
            "\t{ \"a\" : [ 1 , { } ] }\r",
            r#"{"deep":[[[[[[[[[[1]]]]]]]]]]}"#,
        ] {
            assert!(object(ok.as_bytes()).is_ok(), "{ok}");
        }
        let big = format!("{{\"n\":{}}}", "9".repeat(MAX_INT_DIGITS));
        assert!(object(big.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_what_python_rejects() {
        let deep = format!(
            "{{\"a\":{}{}}}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        let huge_int = format!("{{\"n\":{}}}", "9".repeat(MAX_INT_DIGITS + 1));
        for bad in [
            "",
            "[]",
            "\"x\"",
            "{",
            "{\"a\"}",
            "{\"a\":1,}",
            "{\"a\":1}x",
            "{\"a\":1}{}",
            "{'a':1}",
            "{\"a\":01}",
            "{\"a\":1.}",
            "{\"a\":.5}",
            "{\"a\":+1}",
            "{\"a\":-NaN}",
            "{\"a\":nan}",
            "{\"a\":\"\\x\"}",
            "{\"a\":\"\\u12g4\"}",
            "{\"a\":\"tab\there\"}",
            "{\"a\":[1,]}",
            "{\"a\":tru}",
            "\u{feff}{}",
            "{\"a\":1}\u{a0}",
            &deep,
            &huge_int,
        ] {
            assert_eq!(object(bad.as_bytes()).err(), Some(Bad::Invalid), "{bad:?}");
        }
        assert_eq!(object(b"{\"a\":\xff}").err(), Some(Bad::Invalid));
    }

    #[test]
    fn caps_top_level_members() {
        let line = |n: usize| {
            let m: Vec<String> = (0..n).map(|i| format!("\"k{i}\":0")).collect();
            format!("{{{}}}", m.join(","))
        };
        assert_eq!(
            object(line(MAX_MEMBERS).as_bytes()).unwrap().len(),
            MAX_MEMBERS
        );
        assert_eq!(
            object(line(MAX_MEMBERS + 1).as_bytes()).err(),
            Some(Bad::TooManyMembers)
        );
        // Nested members do not count.
        let nested = format!("{{\"type\":\"x\",\"o\":{}}}", line(1000));
        assert!(object(nested.as_bytes()).is_ok());
    }
}
