//! A small Rust tokenizer for the module check: enough to find `mod` items, attributes and `crate::` and `super::`
//! paths. It drops comments and keeps the text of string literals (for `#[path = "..."]`).

/// One token with the line it starts on (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    /// An identifier or a keyword (`r#` is removed).
    Ident(String),
    /// A string literal, with its text between the quotes as written.
    Str(String),
    /// `::`.
    Path,
    /// Any other punctuation character.
    Punct(char),
}

/// Splits `src` into tokens. Numbers, lifetimes and character literals are dropped.
pub(crate) fn tokens(src: &str) -> Vec<(Tok, usize)> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    let at = |i: usize| c.get(i).copied().unwrap_or('\0');
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && at(i + 1) == '/' {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && at(i + 1) == '*' {
            let mut depth = 0usize;
            while i < c.len() {
                if c[i] == '/' && at(i + 1) == '*' {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && at(i + 1) == '/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(c[i] == '\n');
                    i += 1;
                }
            }
        } else if let Some((text, end, lines)) = string_at(&c, i) {
            out.push((Tok::Str(text), line));
            line += lines;
            i = end;
        } else if ch == '\'' {
            i = skip_char_or_lifetime(&c, i);
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let mut s = i;
            if ch == 'r' && at(i + 1) == '#' && (at(i + 2).is_alphabetic() || at(i + 2) == '_') {
                s = i + 2;
            }
            let mut e = s;
            while e < c.len() && (c[e].is_alphanumeric() || c[e] == '_') {
                e += 1;
            }
            out.push((Tok::Ident(c[s..e].iter().collect()), line));
            i = e;
        } else if ch == ':' && at(i + 1) == ':' {
            out.push((Tok::Path, line));
            i += 2;
        } else {
            out.push((Tok::Punct(ch), line));
            i += 1;
        }
    }
    out
}

/// A string literal at `i` (`"..."`, `b"..."`, `c"..."`, `r#"..."#`, `br"..."`): its text, the end index and the
/// count of newlines inside.
fn string_at(c: &[char], i: usize) -> Option<(String, usize, usize)> {
    let at = |k: usize| c.get(k).copied().unwrap_or('\0');
    // Only a prefix that starts a token: `b"` in `ab"` is not a literal start.
    if i > 0 && (c[i - 1].is_alphanumeric() || c[i - 1] == '_') {
        return None;
    }
    let mut k = i;
    if matches!(at(k), 'b' | 'c') {
        k += 1;
    }
    let raw = at(k) == 'r' && matches!(at(k + 1), '"' | '#');
    if raw {
        k += 1;
        let mut hashes = 0;
        while at(k) == '#' {
            hashes += 1;
            k += 1;
        }
        if at(k) != '"' {
            return None;
        }
        let start = k + 1;
        let mut e = start;
        while e < c.len() {
            if c[e] == '"' && (1..=hashes).all(|h| at(e + h) == '#') {
                let text: String = c[start..e].iter().collect();
                let lines = text.matches('\n').count();
                return Some((text, e + 1 + hashes, lines));
            }
            e += 1;
        }
        return None;
    }
    if at(k) != '"' {
        return None;
    }
    let start = k + 1;
    let mut e = start;
    while e < c.len() && c[e] != '"' {
        e += if c[e] == '\\' { 2 } else { 1 };
    }
    let text: String = c[start..e.min(c.len())].iter().collect();
    let lines = text.matches('\n').count();
    Some((text, (e + 1).min(c.len()), lines))
}

/// Skips a character literal (`'x'`, `'\n'`, `'\u{1F600}'`) or a lifetime (`'a`) at `i`.
fn skip_char_or_lifetime(c: &[char], i: usize) -> usize {
    let at = |k: usize| c.get(k).copied().unwrap_or('\0');
    if at(i + 1) == '\\' {
        let mut e = i + 2;
        while e < c.len() && c[e] != '\'' {
            e += 1;
        }
        return e + 1;
    }
    if at(i + 2) == '\'' {
        return i + 3;
    }
    let mut e = i + 1;
    while e < c.len() && (c[e].is_alphanumeric() || c[e] == '_') {
        e += 1;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(src: &str) -> Vec<String> {
        tokens(src)
            .into_iter()
            .filter_map(|(t, _)| match t {
                Tok::Ident(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn drops_comments_chars_and_lifetimes() {
        let src = "// crate::a\n/* crate::b /* nested */ */ fn f<'a>(x: &'a u8) { let c = '{'; let d = '\\''; }";
        assert_eq!(idents(src), ["fn", "f", "x", "u8", "let", "c", "let", "d"]);
        let braces = tokens(src)
            .iter()
            .filter(|(t, _)| *t == Tok::Punct('{'))
            .count();
        assert_eq!(
            braces, 1,
            "the brace in the character literal is not a token"
        );
    }

    #[test]
    fn keeps_strings_and_lines() {
        let t = tokens("#[path = \"a/b.rs\"]\nmod x;\nlet s = r#\"crate::q\"#;");
        assert!(t.contains(&(Tok::Str("a/b.rs".into()), 1)));
        assert!(t.contains(&(Tok::Ident("mod".into()), 2)));
        assert!(t.contains(&(Tok::Str("crate::q".into()), 3)));
        assert!(!idents("let s = \"crate::q\";").contains(&"crate".to_string()));
    }
}
