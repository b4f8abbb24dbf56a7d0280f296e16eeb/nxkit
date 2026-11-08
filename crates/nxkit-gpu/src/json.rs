//! A small JSON reader for the `_b.json` reflection of the shader step.
//!
//! The workspace has no JSON dependency in this layer. The reader covers what the reflection uses: objects, arrays,
//! strings with the usual escapes, numbers, booleans and null. Numbers are kept as `f64`, which holds every integer
//! of the reflection exactly.

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 => Some(*n as i64),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(crate) fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

const MAX_DEPTH: u32 = 64;

/// Parses a document. Returns `None` for anything malformed.
pub(crate) fn parse(text: &str) -> Option<Json> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.value()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.s.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn word(&mut self, w: &str, v: Json) -> Option<Json> {
        let end = self.i.checked_add(w.len())?;
        (self.s.get(self.i..end)? == w.as_bytes()).then(|| {
            self.i = end;
            v
        })
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.s.get(self.i)? {
            b'{' => self.nested(Self::object),
            b'[' => self.nested(Self::array),
            b'"' => self.string().map(Json::Str),
            b't' => self.word("true", Json::Bool(true)),
            b'f' => self.word("false", Json::Bool(false)),
            b'n' => self.word("null", Json::Null),
            _ => self.number(),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Option<Json>) -> Option<Json> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let v = f(self);
        self.depth -= 1;
        v
    }

    fn object(&mut self) -> Option<Json> {
        self.eat(b'{')?;
        let mut m = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(Json::Obj(m));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.eat(b':')?;
            m.push((k, self.value()?));
            self.ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Json::Obj(m));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.eat(b'[')?;
        let mut a = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Some(Json::Arr(a));
        }
        loop {
            a.push(self.value()?);
            self.ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Json::Arr(a));
                }
                _ => return None,
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.i..self.i.checked_add(4)?)?;
        let v = u32::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        if *self.s.get(self.i)? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.s.get(self.i..self.i + 2)? != b"\\u" {
                                    return None;
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                0x10000 + ((hi - 0xD800) << 10) + lo.checked_sub(0xDC00)?
                            } else {
                                hi
                            };
                            char::from_u32(cp)?
                        }
                        _ => return None,
                    };
                    let mut b = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while matches!(
            self.s.get(self.i),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.i += 1;
        }
        let t = std::str::from_utf8(self.s.get(start..self.i)?).ok()?;
        t.parse::<f64>().ok().map(Json::Num)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_shapes_of_a_reflection() {
        let j = parse(
            r#"{"key":"06ec","entry":"main0","pull":{},"bindings":[{"arg_id":2,"kind":"tex","reg":null,"rw":true}],"n":-3,"s":"a\u00e9\n"}"#,
        );
        let j = j.as_ref();
        assert_eq!(
            j.and_then(|j| j.get("entry")).and_then(Json::as_str),
            Some("main0")
        );
        assert_eq!(j.and_then(|j| j.get("n")).and_then(Json::as_i64), Some(-3));
        assert_eq!(
            j.and_then(|j| j.get("s")).and_then(Json::as_str),
            Some("a\u{e9}\n")
        );
        let b = j.and_then(|j| j.get("bindings")).map(Json::as_arr);
        assert_eq!(b.map(<[Json]>::len), Some(1));
        assert_eq!(
            b.and_then(|b| b.first()).and_then(|b| b.get("reg")),
            Some(&Json::Null)
        );
    }

    #[test]
    fn refuses_malformed_input() {
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "\"x", "{} x", "tru"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
    }
}
