//! A small JSON writer for the manifest. The reader of the crate is `crate::json`.

/// A JSON value to write.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum J {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub(crate) fn str(s: impl Into<String>) -> J {
        J::Str(s.into())
    }

    pub(crate) fn uint(n: impl TryInto<i64>) -> J {
        n.try_into().map_or(J::Null, J::Int)
    }

    /// A float. NaN and infinity have no JSON form and become null.
    pub(crate) fn num(x: f64) -> J {
        if x.is_finite() { J::Num(x) } else { J::Null }
    }

    /// A single precision float, written with the digits of the single precision value (0.1 stays 0.1).
    pub(crate) fn num32(x: f32) -> J {
        format!("{x}").parse::<f64>().map_or(J::Null, J::num)
    }

    pub(crate) fn opt_str(s: Option<impl Into<String>>) -> J {
        s.map_or(J::Null, |s| J::Str(s.into()))
    }

    pub(crate) fn strs<S: AsRef<str>>(list: &[S]) -> J {
        J::Arr(list.iter().map(|s| J::str(s.as_ref())).collect())
    }

    /// An object from pairs in the order given.
    pub(crate) fn obj<const N: usize>(pairs: [(&str, J); N]) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    }

    /// The text with two spaces of indent. Arrays of numbers and of strings stay on one line.
    pub(crate) fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn simple(&self) -> bool {
        !matches!(self, J::Arr(_) | J::Obj(_))
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => out.push_str(&n.to_string()),
            J::Num(x) => out.push_str(&x.to_string()),
            J::Str(s) => quote(out, s),
            J::Arr(a) if a.is_empty() => out.push_str("[]"),
            J::Arr(a) if a.iter().all(J::simple) && a.len() <= 8 => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out, depth);
                }
                out.push(']');
            }
            J::Arr(a) => {
                out.push_str("[\n");
                for (i, v) in a.iter().enumerate() {
                    indent(out, depth + 1);
                    v.write(out, depth + 1);
                    out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push(']');
            }
            J::Obj(o) if o.is_empty() => out.push_str("{}"),
            J::Obj(o) => {
                out.push_str("{\n");
                for (i, (k, v)) in o.iter().enumerate() {
                    indent(out, depth + 1);
                    quote(out, k);
                    out.push_str(": ");
                    v.write(out, depth + 1);
                    out.push_str(if i + 1 < o.len() { ",\n" } else { "\n" });
                }
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{Json, parse};

    #[test]
    fn the_output_parses_with_the_reader_of_the_crate() {
        let j = J::obj([
            ("name", J::str("a \"quoted\"\nline\\")),
            ("n", J::uint(7u32)),
            ("x", J::num32(0.1)),
            ("nan", J::num(f64::NAN)),
            ("list", J::Arr(vec![J::Bool(true), J::Null, J::Int(-3)])),
            (
                "deep",
                J::Arr(vec![J::obj([("k", J::strs(&["a", "b"]))]), J::Arr(vec![])]),
            ),
            ("empty", J::Obj(vec![])),
        ]);
        let text = j.pretty();
        let back = parse(&text).expect("valid json");
        assert_eq!(
            back.get("name").and_then(Json::as_str),
            Some("a \"quoted\"\nline\\")
        );
        assert_eq!(back.get("n").and_then(Json::as_i64), Some(7));
        assert_eq!(back.get("nan"), Some(&Json::Null));
        assert_eq!(back.get("x"), Some(&Json::Num(0.1)));
        assert!(text.contains("[true, null, -3]"));
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut s = String::new();
        quote(&mut s, "a\u{1}b\t");
        assert_eq!(s, "\"a\\u0001b\\t\"");
    }
}
