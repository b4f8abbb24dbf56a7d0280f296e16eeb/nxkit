//! Tokenizer for the Metal Shading Language subset that the reference build engine generates
//! (spirv-cross output plus the engine's injected helpers).
//!
//! This is a faithful port of the phase 1 prototype `msl_lex.py`. Every token carries its
//! exact byte span in the source, its text and its 1-based line number. The token stream is
//! identical to the prototype's for its covered syntax, checked by the `lexdump` comparison in
//! `tools/compare_tokens.sh` on the corpus. Binary literal tokens extend that syntax.
//!
//! Rules (all taken over from the prototype, including its corner cases):
//! * A line whose first non-blank character (spaces and tabs only) is `#` is one `Pp` token:
//!   the whole line, continued over lines that end in a backslash, with whitespace stripped
//!   at both ends.
//! * Whitespace, newlines, `//` and `/* */` comments produce no tokens. An unterminated
//!   `/*` is not a comment: it lexes as the punctuators `/` and `*`.
//! * `[[` is always one `AttrOpen` token (the C++ rule). `]]` stays two `]` tokens.
//! * Numbers use the prototype's rules plus binary literals (see [`lex_number`]).
//! * Any other character is a [`LexError`].
//!
//! Input text must already have universal newlines applied (`\r\n` and `\r` turned into
//! `\n`), as Python's text mode does. [`normalize_newlines`] does that.

use std::fmt;

/// The kind of a token. The names returned by [`TokKind::name`] are the prototype's spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokKind {
    /// Identifier or keyword: `[A-Za-z_]\w*`.
    Id,
    /// Numeric literal.
    Num,
    /// Punctuator (one to three characters).
    Punct,
    /// A whole preprocessor line.
    Pp,
    /// The attribute opener `[[`.
    AttrOpen,
    /// A string literal including its quotes.
    Str,
    /// End of input. The lexer itself never produces it: the parser appends one.
    Eof,
}

impl TokKind {
    /// The kind name as the Python prototype spells it.
    pub fn name(self) -> &'static str {
        match self {
            TokKind::Id => "id",
            TokKind::Num => "num",
            TokKind::Punct => "punct",
            TokKind::Pp => "pp",
            TokKind::AttrOpen => "attr_open",
            TokKind::Str => "str",
            TokKind::Eof => "eof",
        }
    }
}

/// A byte range in the source text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span {
            start: start as u32,
            end: end as u32,
        }
    }
    /// The smallest span that covers both.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// One token. `text` is the exact source slice at `span` (for `Pp`, the stripped line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokKind,
    pub text: &'a str,
    pub line: u32,
    pub span: Span,
}

/// A character the tokenizer does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub line: u32,
    pub offset: usize,
    /// The prototype's message: `line N: unexpected '...'` with up to 20 characters.
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LexError {}

/// Python's `str.isspace()`: Unicode White_Space plus the ASCII separators 0x1c to 0x1f.
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's regex `\w` on `str` patterns. Exact for ASCII. For other characters this uses
/// Rust's Alphanumeric property, which differs from Python's `str.isalnum()` only in rare
/// numeric categories. The engine's output is pure ASCII.
fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Python's regex `\d` on `str` patterns (Unicode decimal digits). Exact for ASCII. For other
/// characters it uses `char::is_numeric`, a superset of Python's Nd category.
fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric())
}

/// Universal newlines as Python's text mode applies them: `\r\n` and lone `\r` become `\n`.
pub fn normalize_newlines(src: String) -> String {
    if !src.contains('\r') {
        return src;
    }
    let mut out = String::with_capacity(src.len());
    let mut it = src.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Three- and two-character punctuators in the prototype's alternation order. With these
/// lengths and contents the first match is also the longest match.
const PUNCT3: [&str; 3] = [">>=", "<<=", "..."];
const PUNCT2: [&str; 20] = [
    "::", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=", "/=",
    "%=", "&=", "|=", "^=",
];
const PUNCT1: &str = "[]{}();,.<>+-*/%&|^!~?:=#";

/// Byte at `i`, or 0 past the end (0 never matches any class below).
#[inline]
fn byte(b: &[u8], i: usize) -> u8 {
    if i < b.len() {
        b[i]
    } else {
        0
    }
}

/// The character at byte offset `i` (which must be a char boundary).
#[inline]
fn char_at(src: &str, i: usize) -> Option<char> {
    src[i..].chars().next()
}

/// Length of a run of `\d` characters starting at `i`.
fn digits(src: &str, i: usize) -> usize {
    let mut j = i;
    while let Some(c) = char_at(src, j) {
        if !is_digit(c) {
            break;
        }
        j += c.len_utf8();
    }
    j - i
}

/// Matches the prototype's number pattern at `i` and returns the end offset. The pattern is
///
/// ```text
/// 0[xX][0-9a-fA-F]+[uUlL]*
/// | (?:\d+\.\d*|\.\d+)(?:[eE][+-]?\d+)?[fFhH]?
/// | \d+[eE][+-]?\d+[fFhH]?
/// | \d+[uUlLfFhH]*
/// ```
///
/// This also recognizes binary integer tokens. Keep their invalid suffixes
/// together so the parser can reject them rather than admit two literals.
///
/// Python's `re` takes the first alternative that matches, not the longest. Each alternative
/// is deterministic here: no backtracking inside it can produce a different match.
pub fn lex_number(src: &str, i: usize) -> Option<usize> {
    let b = src.as_bytes();
    // 0[xX][0-9a-fA-F]+[uUlL]*
    if byte(b, i) == b'0'
        && matches!(byte(b, i + 1), b'x' | b'X')
        && byte(b, i + 2).is_ascii_hexdigit()
    {
        let mut j = i + 2;
        while byte(b, j).is_ascii_hexdigit() {
            j += 1;
        }
        while matches!(byte(b, j), b'u' | b'U' | b'l' | b'L') {
            j += 1;
        }
        return Some(j);
    }
    if byte(b, i) == b'0'
        && matches!(byte(b, i + 1), b'b' | b'B')
        && matches!(byte(b, i + 2), b'0' | b'1' | b'\'')
    {
        let mut j = i + 2;
        while byte(b, j).is_ascii_alphanumeric() || matches!(byte(b, j), b'_' | b'.' | b'\'') {
            j += 1;
        }
        return Some(j);
    }
    // (?:\d+\.\d*|\.\d+)(?:[eE][+-]?\d+)?[fFhH]?
    let lead = digits(src, i);
    let mantissa_end = if lead > 0 && byte(b, i + lead) == b'.' {
        let j = i + lead + 1;
        Some(j + digits(src, j))
    } else if lead == 0 && byte(b, i) == b'.' && digits(src, i + 1) > 0 {
        Some(i + 1 + digits(src, i + 1))
    } else {
        None
    };
    if let Some(mut j) = mantissa_end {
        j = exponent(src, j).unwrap_or(j);
        if matches!(byte(b, j), b'f' | b'F' | b'h' | b'H') {
            j += 1;
        }
        return Some(j);
    }
    if lead == 0 {
        return None;
    }
    // \d+[eE][+-]?\d+[fFhH]?
    if let Some(mut j) = exponent(src, i + lead) {
        if matches!(byte(b, j), b'f' | b'F' | b'h' | b'H') {
            j += 1;
        }
        return Some(j);
    }
    // \d+[uUlLfFhH]*
    let mut j = i + lead;
    while matches!(
        byte(b, j),
        b'u' | b'U' | b'l' | b'L' | b'f' | b'F' | b'h' | b'H'
    ) {
        j += 1;
    }
    Some(j)
}

/// `[eE][+-]?\d+` at `i`: the end offset, or None when it does not match.
fn exponent(src: &str, i: usize) -> Option<usize> {
    let b = src.as_bytes();
    if !matches!(byte(b, i), b'e' | b'E') {
        return None;
    }
    let mut j = i + 1;
    if matches!(byte(b, j), b'+' | b'-') {
        j += 1;
    }
    let n = digits(src, j);
    if n == 0 {
        None
    } else {
        Some(j + n)
    }
}

/// A string literal `"(?:[^"\\\n]|\\.)*"` (with DOTALL) at `i`: the end offset.
fn lex_string(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    loop {
        match *b.get(j)? {
            b'"' => return Some(j + 1),
            b'\n' => return None,
            // `\\.` with DOTALL takes any following character. A multi-byte UTF-8 character
            // continues with bytes >= 0x80, which the default arm consumes one by one.
            b'\\' => {
                b.get(j + 1)?;
                j += 2;
            }
            _ => j += 1,
        }
    }
}

/// Punctuator at `i`: its length.
fn lex_punct(b: &[u8], i: usize) -> Option<usize> {
    let rest = &b[i..];
    for p in PUNCT3 {
        if rest.starts_with(p.as_bytes()) {
            return Some(3);
        }
    }
    for p in PUNCT2 {
        if rest.starts_with(p.as_bytes()) {
            return Some(2);
        }
    }
    if !rest.is_empty() && PUNCT1.as_bytes().contains(&rest[0]) {
        return Some(1);
    }
    None
}

/// The span of `src[start..end]` with Python `str.strip()` applied.
fn strip_span(src: &str, start: usize, end: usize) -> (usize, usize) {
    let s = &src[start..end];
    let trimmed_start = s.trim_start_matches(py_isspace);
    let a = start + (s.len() - trimmed_start.len());
    let trimmed = trimmed_start.trim_end_matches(py_isspace);
    (a, a + trimmed.len())
}

fn count_newlines(b: &[u8]) -> u32 {
    b.iter().filter(|&&c| c == b'\n').count() as u32
}

/// Python's `repr()` of a string, used for error messages.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c if (0x80..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Tokenizes `src` (universal newlines already applied).
pub fn tokenize(src: &str) -> Result<Vec<Token<'_>>, LexError> {
    let b = src.as_bytes();
    let n = b.len();
    // Shaders average about one token per four bytes.
    let mut out = Vec::with_capacity(n / 4 + 16);
    let mut line: u32 = 1;
    let mut i = 0usize;
    let mut bol = true;
    while i < n {
        if bol {
            let mut j = i;
            while j < n && (b[j] == b' ' || b[j] == b'\t') {
                j += 1;
            }
            if j < n && b[j] == b'#' {
                let mut e = memchr(b'\n', b, j).unwrap_or(n);
                // A continued line: the prototype tests `src[e - 1] == '\\' and e < n`.
                while b[e - 1] == b'\\' && e < n {
                    e = memchr(b'\n', b, e + 1).unwrap_or(n);
                }
                let (a, z) = strip_span(src, j, e);
                out.push(Token {
                    kind: TokKind::Pp,
                    text: &src[a..z],
                    line,
                    span: Span::new(a, z),
                });
                line += count_newlines(&b[j..e]);
                i = e;
                // `bol` stays set: the newline after the directive is lexed next.
                continue;
            }
        }
        let c = b[i];
        // Alternatives in the prototype's order: ws, nl, lc, bc, num, id, str, punct.
        if matches!(c, b' ' | b'\t' | b'\r' | 0x0c | 0x0b) {
            let mut j = i + 1;
            while j < n && matches!(b[j], b' ' | b'\t' | b'\r' | 0x0c | 0x0b) {
                j += 1;
            }
            i = j;
            bol = false;
            continue;
        }
        if c == b'\n' {
            line += 1;
            i += 1;
            bol = true;
            continue;
        }
        if c == b'/' && byte(b, i + 1) == b'/' {
            i = memchr(b'\n', b, i).unwrap_or(n);
            bol = false;
            continue;
        }
        if c == b'/' && byte(b, i + 1) == b'*' {
            if let Some(e) = find(b, b"*/", i + 2) {
                line += count_newlines(&b[i..e + 2]);
                i = e + 2;
                bol = false;
                continue;
            }
            // Unterminated: falls through to the punctuator `/`.
        }
        bol = false;
        let start = i;
        let (kind, end) = if let Some(e) = lex_number(src, i) {
            (TokKind::Num, e)
        } else if c == b'_' || c.is_ascii_alphabetic() {
            let mut j = i + 1;
            while let Some(ch) = char_at(src, j) {
                if !is_word(ch) {
                    break;
                }
                j += ch.len_utf8();
            }
            (TokKind::Id, j)
        } else if let Some(e) = (c == b'"').then(|| lex_string(b, i)).flatten() {
            (TokKind::Str, e)
        } else if let Some(len) = lex_punct(b, i) {
            if c == b'[' && byte(b, i + 1) == b'[' {
                (TokKind::AttrOpen, i + 2)
            } else {
                (TokKind::Punct, i + len)
            }
        } else {
            let snippet: String = src[i..].chars().take(20).collect();
            return Err(LexError {
                line,
                offset: i,
                message: format!("line {line}: unexpected {}", py_repr(&snippet)),
            });
        };
        out.push(Token {
            kind,
            text: &src[start..end],
            line,
            span: Span::new(start, end),
        });
        i = end;
    }
    Ok(out)
}

fn memchr(needle: u8, hay: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .iter()
        .position(|&c| c == needle)
        .map(|p| p + from)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// The token dump format shared with `tools/lex_dump.py`: one line per token,
/// `kind<TAB>line<TAB>text` with backslash, newline and tab in the text escaped.
pub fn dump_line(tok: &Token<'_>, out: &mut String) {
    out.push_str(tok.kind.name());
    out.push('\t');
    out.push_str(&tok.line.to_string());
    out.push('\t');
    for c in tok.text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(&'static str, String, u32)> {
        tokenize(src)
            .unwrap()
            .iter()
            .map(|t| (t.kind.name(), t.text.to_string(), t.line))
            .collect()
    }

    fn texts(src: &str) -> Vec<String> {
        tokenize(src)
            .unwrap()
            .iter()
            .map(|t| t.text.to_string())
            .collect()
    }

    #[test]
    fn identifiers_and_punct() {
        assert_eq!(
            kinds("a_b1 = c->d;"),
            vec![
                ("id", "a_b1".into(), 1),
                ("punct", "=".into(), 1),
                ("id", "c".into(), 1),
                ("punct", "->".into(), 1),
                ("id", "d".into(), 1),
                ("punct", ";".into(), 1)
            ]
        );
    }

    #[test]
    fn longest_punct_wins() {
        assert_eq!(
            texts("a>>=b<<=c...d::e"),
            ["a", ">>=", "b", "<<=", "c", "...", "d", "::", "e"]
        );
        assert_eq!(
            texts("a<=b>=c==d!=e&&f||g"),
            ["a", "<=", "b", ">=", "c", "==", "d", "!=", "e", "&&", "f", "||", "g"]
        );
        assert_eq!(
            texts("++--+=-=*=/=%=&=|=^="),
            ["++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^="]
        );
        assert_eq!(texts("<<>>"), ["<<", ">>"]);
    }

    #[test]
    fn numbers() {
        assert_eq!(
            texts("0x1Fu 1.5f 1. .5 1e5 1.e5 2.5e-3h 7u 7ul 3h 1f"),
            ["0x1Fu", "1.5f", "1.", ".5", "1e5", "1.e5", "2.5e-3h", "7u", "7ul", "3h", "1f"]
        );
        // `0x` without hex digits: `0` then the identifier `x`.
        assert_eq!(
            kinds("0xg"),
            vec![("num", "0".into(), 1), ("id", "xg".into(), 1)]
        );
        // An exponent without digits is not part of the number.
        assert_eq!(texts("1.e"), ["1.", "e"]);
        assert_eq!(texts("1e+"), ["1", "e", "+"]);
        // A dot without digits around it is a punctuator.
        assert_eq!(
            kinds(". x"),
            vec![("punct", ".".into(), 1), ("id", "x".into(), 1)]
        );
        assert_eq!(texts("a.x"), ["a", ".", "x"]);
        assert_eq!(texts("1.5.x"), ["1.5", ".", "x"]);
        assert_eq!(texts("0x10UL"), ["0x10UL"]);
        assert_eq!(texts("0b101u 0B10U 0b"), ["0b101u", "0B10U", "0", "b"]);
        assert_eq!(
            texts("0b10'10u 0B'1U 0b1u'"),
            ["0b10'10u", "0B'1U", "0b1u'"]
        );
    }

    #[test]
    fn comments_and_lines() {
        let k = kinds("a // c\n/* x\n y */ b\n");
        assert_eq!(k, vec![("id", "a".into(), 1), ("id", "b".into(), 3)]);
        // Unterminated block comment: `/` and `*`.
        assert_eq!(texts("/* x"), ["/", "*", "x"]);
    }

    #[test]
    fn preprocessor_lines() {
        let k = kinds("#include <metal_stdlib>\n  #define A \\\n  B\nx #y\n");
        assert_eq!(
            k,
            vec![
                ("pp", "#include <metal_stdlib>".into(), 1),
                ("pp", "#define A \\\n  B".into(), 2),
                ("id", "x".into(), 4),
                ("punct", "#".into(), 4),
                ("id", "y".into(), 4)
            ]
        );
        // `#` after a comment on the same line is not a directive.
        assert_eq!(kinds("/**/#a")[0].0, "punct");
        // A directive at the very end without a newline.
        assert_eq!(kinds("#pragma x  ")[0].1, "#pragma x");
        // Continuation backslash at the very end of input.
        assert_eq!(kinds("#a \\")[0].1, "#a \\");
    }

    #[test]
    fn attributes() {
        assert_eq!(kinds("[[id(0)]]")[0], ("attr_open", "[[".into(), 1));
        assert_eq!(texts("[[id(0)]]"), ["[[", "id", "(", "0", ")", "]", "]"]);
        assert_eq!(texts("a[b[0]]"), ["a", "[", "b", "[", "0", "]", "]"]);
        assert_eq!(texts("[[["), ["[[", "["]);
    }

    #[test]
    fn strings() {
        assert_eq!(kinds(r#""a\"b" x"#)[0], ("str", r#""a\"b""#.into(), 1));
        assert!(tokenize("\"abc").is_err());
        assert!(tokenize("\"ab\nc\"").is_err());
    }

    #[test]
    fn errors() {
        let e = tokenize("a\n @b").unwrap_err();
        assert_eq!(e.message, "line 2: unexpected '@b'");
        assert_eq!(
            tokenize("'x'").unwrap_err().message,
            "line 1: unexpected \"'x'\""
        );
    }

    #[test]
    fn spans_are_exact() {
        let src = "  float4 x = 1.0;";
        for t in tokenize(src).unwrap() {
            assert_eq!(&src[t.span.range()], t.text);
        }
    }

    #[test]
    fn universal_newlines() {
        assert_eq!(normalize_newlines("a\r\nb\rc".into()), "a\nb\nc");
    }

    #[test]
    fn python_repr() {
        assert_eq!(py_repr("ab"), "'ab'");
        assert_eq!(py_repr("a'b"), "\"a'b\"");
        assert_eq!(py_repr("a'\"b"), "'a\\'\"b'");
        assert_eq!(py_repr("a\nb\\"), "'a\\nb\\\\'");
    }
}
