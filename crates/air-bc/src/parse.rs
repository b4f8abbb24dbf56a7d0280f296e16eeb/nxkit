//! Parser of the text IR subset air-emit writes. Anything outside the subset is an error, so a
//! caller can fall back to Apple's text assembler for that shader.

use crate::ast::*;

/// A parse error with the line.
pub type R<T> = Result<T, String>;

use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
enum Tok<'a> {
    Local(&'a str),
    Global(&'a str),
    Meta(&'a str),
    MetaNum(u32),
    Attr(u32),
    Word(Cow<'a, str>),
    Int(i128),
    Str(Cow<'a, [u8]>),
    P(char),
}

fn unescape(s: &[u8]) -> Cow<'_, [u8]> {
    if !s.contains(&b'\\') {
        return Cow::Borrowed(s);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 2 < s.len() + 1 && i + 2 <= s.len() - 1 + 1 {
            if let Ok(v) = u8::from_str_radix(
                std::str::from_utf8(&s[i + 1..(i + 3).min(s.len())]).unwrap_or(""),
                16,
            ) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    Cow::Owned(out)
}

fn lex(line: &str) -> R<Vec<Tok<'_>>> {
    let b = line.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let ident = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'$' | b'-');
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        if c == b';' {
            break;
        }
        if c == b'"' {
            let st = i + 1;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            out.push(Tok::Str(unescape(&b[st..i])));
            i += 1;
            continue;
        }
        if matches!(c, b'%' | b'@' | b'!' | b'#') {
            i += 1;
            if c == b'!' && i < b.len() && b[i] == b'"' {
                let st = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                let s = unescape(&b[st..i]);
                i += 1;
                out.push(Tok::P('!'));
                out.push(Tok::Str(s));
                continue;
            }
            if i < b.len() && b[i] == b'"' {
                let st = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                let s = line.get(st..i).ok_or("bad utf-8 in name")?;
                i += 1;
                out.push(if c == b'%' {
                    Tok::Local(s)
                } else {
                    Tok::Global(s)
                });
                continue;
            }
            let st = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            let s = line.get(st..i).ok_or("bad utf-8 in name")?;
            out.push(match c {
                b'%' => Tok::Local(s),
                b'@' => Tok::Global(s),
                b'#' => Tok::Attr(s.parse().map_err(|_| format!("attr ref {s}"))?),
                _ => {
                    if s.is_empty() {
                        Tok::P('!')
                    } else if let Ok(n) = s.parse() {
                        Tok::MetaNum(n)
                    } else {
                        Tok::Meta(s)
                    }
                }
            });
            continue;
        }
        if c == b'-' || c.is_ascii_digit() {
            let st = i;
            i += 1;
            if c == b'0' && i < b.len() && b[i] == b'x' {
                i += 1;
                let hs = i;
                let half = i < b.len() && b[i] == b'H';
                if half {
                    i += 1;
                }
                let ds = i;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let v = u64::from_str_radix(std::str::from_utf8(&b[ds..i]).unwrap_or("0"), 16)
                    .map_err(|e| e.to_string())?;
                let _ = hs;
                out.push(Tok::Word(Cow::Owned(format!(
                    "{}{}",
                    if half { "0xH" } else { "0x" },
                    v
                ))));
                continue;
            }
            while i < b.len()
                && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'e' || b[i] == b'+')
            {
                i += 1;
            }
            let t = std::str::from_utf8(&b[st..i]).unwrap_or("");
            if t.contains('.') || t.contains('e') {
                let f: f64 = t.parse().map_err(|_| format!("float {t}"))?;
                out.push(Tok::Word(Cow::Owned(format!("0x{}", f.to_bits()))));
            } else {
                out.push(Tok::Int(t.parse().map_err(|_| format!("int {t}"))?));
            }
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let st = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            // `c"..."` is not emitted by air-emit
            out.push(Tok::Word(Cow::Borrowed(
                line.get(st..i).ok_or("bad utf-8 in word")?,
            )));
            continue;
        }
        out.push(Tok::P(c as char));
        i += 1;
    }
    Ok(out)
}

struct P<'a> {
    t: &'a [Tok<'a>],
    i: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<&Tok<'a>> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> R<Tok<'a>> {
        let t = self.t.get(self.i).cloned().ok_or("unexpected end")?;
        self.i += 1;
        Ok(t)
    }
    fn is(&self, c: char) -> bool {
        self.peek() == Some(&Tok::P(c))
    }
    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }
    fn eat(&mut self, c: char) -> bool {
        if self.is(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, c: char) -> R<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected {c} at {:?}", self.peek()))
        }
    }
    fn word(&mut self) -> R<String> {
        match self.next()? {
            Tok::Word(w) => Ok(w.into_owned()),
            t => Err(format!("expected word, got {t:?}")),
        }
    }
    fn int(&mut self) -> R<i128> {
        match self.next()? {
            Tok::Int(v) => Ok(v),
            t => Err(format!("expected int, got {t:?}")),
        }
    }
    fn local(&mut self) -> R<String> {
        match self.next()? {
            Tok::Local(s) => Ok(s.to_string()),
            t => Err(format!("expected local, got {t:?}")),
        }
    }

    fn base_type(&mut self) -> R<Ty> {
        let t = self.next()?;
        Ok(match t {
            Tok::Word(w) => match w.as_ref() {
                "void" => Ty::Void,
                "half" => Ty::Half,
                "float" => Ty::Float,
                "double" => Ty::Double,
                "label" => Ty::Label,
                "metadata" => Ty::Metadata,
                w if w.starts_with('i') && w[1..].parse::<u32>().is_ok() => {
                    Ty::Int(w[1..].parse().unwrap_or(32))
                }
                w => return Err(format!("type word {w}")),
            },
            Tok::Local(n) => Ty::Named(n.to_string()),
            Tok::P('<') => {
                if self.eat('{') {
                    let e = self.struct_body('}')?;
                    self.expect('>')?;
                    Ty::Struct(true, e)
                } else {
                    let n = self.int()? as u32;
                    if self.word()? != "x" {
                        return Err("vector x".into());
                    }
                    let e = self.ty()?;
                    self.expect('>')?;
                    Ty::Vector(n, Box::new(e))
                }
            }
            Tok::P('[') => {
                let n = self.int()? as u64;
                if self.word()? != "x" {
                    return Err("array x".into());
                }
                let e = self.ty()?;
                self.expect(']')?;
                Ty::Array(n, Box::new(e))
            }
            Tok::P('{') => Ty::Struct(false, self.struct_body('}')?),
            t => return Err(format!("type token {t:?}")),
        })
    }

    fn struct_body(&mut self, close: char) -> R<Vec<Ty>> {
        let mut v = Vec::new();
        if self.eat(close) {
            return Ok(v);
        }
        loop {
            v.push(self.ty()?);
            if self.eat(close) {
                return Ok(v);
            }
            self.expect(',')?;
        }
    }

    fn ty(&mut self) -> R<Ty> {
        let mut t = self.base_type()?;
        loop {
            if self.is_word("addrspace") {
                self.i += 1;
                self.expect('(')?;
                let n = self.int()? as u32;
                self.expect(')')?;
                self.expect('*')?;
                t = Ty::Ptr(Box::new(t), n);
            } else if self.eat('*') {
                t = Ty::Ptr(Box::new(t), 0);
            } else if self.is('(') {
                self.i += 1;
                let ps = self.struct_body(')')?;
                t = Ty::Func(Box::new(t), ps);
            } else {
                return Ok(t);
            }
        }
    }

    fn value(&mut self) -> R<Val> {
        let t = self.next()?;
        Ok(match t {
            Tok::Local(s) => Val::Local(s.to_string()),
            Tok::Global(s) => Val::Global(s.to_string()),
            Tok::Int(v) => Val::Int(v),
            Tok::Word(w) => match w.as_ref() {
                "true" => Val::Int(1),
                "false" => Val::Int(0),
                "null" => Val::Null,
                "undef" => Val::Undef,
                "poison" => Val::Poison,
                "zeroinitializer" => Val::Zero,
                "bitcast" | "addrspacecast" => {
                    self.expect('(')?;
                    let ty = self.ty()?;
                    let v = self.value()?;
                    if self.word()? != "to" {
                        return Err("cast to".into());
                    }
                    let to = self.ty()?;
                    self.expect(')')?;
                    Val::Cast(w.into_owned(), Box::new((ty, v)), to)
                }
                w if w.starts_with("0xH") => Val::Float(w[3..].parse().map_err(|_| "half")?),
                w if w.starts_with("0x") => Val::Float(w[2..].parse().map_err(|_| "float")?),
                w => return Err(format!("value word {w}")),
            },
            Tok::P('<') => {
                let packed = self.eat('{');
                let close = if packed { '}' } else { '>' };
                let items = self.typed_list(close)?;
                if packed {
                    self.expect('>')?;
                }
                Val::Aggregate(items)
            }
            Tok::P('[') => Val::Aggregate(self.typed_list(']')?),
            Tok::P('{') => Val::Aggregate(self.typed_list('}')?),
            t => return Err(format!("value token {t:?}")),
        })
    }

    fn typed_list(&mut self, close: char) -> R<Vec<(Ty, Val)>> {
        let mut v = Vec::new();
        if self.eat(close) {
            return Ok(v);
        }
        loop {
            let t = self.ty()?;
            let x = self.value()?;
            v.push((t, x));
            if self.eat(close) {
                return Ok(v);
            }
            self.expect(',')?;
        }
    }

    fn typed(&mut self) -> R<(Ty, Val)> {
        let t = self.ty()?;
        Ok((t, self.value()?))
    }

    /// Flags words (nsw, nnan, ...) before a type.
    fn flags(&mut self) -> Vec<String> {
        let mut v = Vec::new();
        while let Some(Tok::Word(w)) = self.peek() {
            if matches!(
                w.as_ref(),
                "nsw"
                    | "nuw"
                    | "nnan"
                    | "ninf"
                    | "nsz"
                    | "arcp"
                    | "afn"
                    | "contract"
                    | "reassoc"
                    | "fast"
                    | "exact"
                    | "inbounds"
            ) {
                v.push(w.to_string());
                self.i += 1;
            } else {
                break;
            }
        }
        v
    }

    fn param_attrs(&mut self) -> R<Vec<String>> {
        let mut v = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::Word(w))
                    if matches!(
                        w.as_ref(),
                        "noundef" | "nocapture" | "readonly" | "zeroext" | "immarg" | "writeonly"
                    ) =>
                {
                    v.push(w.to_string());
                    self.i += 1;
                }
                Some(Tok::Word(w)) if w == "align" => {
                    self.i += 1;
                    let n = self.int()?;
                    v.push(format!("align {n}"));
                }
                Some(Tok::Word(w)) if w.starts_with("dereferenceable") => {
                    self.i += 1;
                    self.expect('(')?;
                    let n = self.int()?;
                    self.expect(')')?;
                    v.push(format!("dereferenceable {n}"));
                }
                Some(Tok::Str(s)) => {
                    v.push(format!("\"{}", String::from_utf8_lossy(s)));
                    self.i += 1;
                }
                _ => return Ok(v),
            }
        }
    }

    fn md_attachments(&mut self) -> R<Vec<(String, u32)>> {
        let mut v = Vec::new();
        while self.eat(',') {
            let k = match self.next()? {
                Tok::Meta(k) => k,
                Tok::Word(w) if w == "align" => return Err("align after metadata".into()),
                t => return Err(format!("attachment {t:?}")),
            };
            let n = match self.next()? {
                Tok::MetaNum(n) => n,
                t => return Err(format!("attachment node {t:?}")),
            };
            v.push((k.to_string(), n));
        }
        Ok(v)
    }

    fn inst(&mut self) -> R<Inst> {
        let mut res = None;
        if let Some(Tok::Local(n)) = self.peek().cloned() {
            if self.t.get(self.i + 1) == Some(&Tok::P('=')) {
                res = Some(n.to_string());
                self.i += 2;
            }
        }
        let opw = self.word()?;
        let op = match opw.as_ref() {
            "alloca" => {
                let t = self.ty()?;
                self.expect(',')?;
                if self.word()? != "align" {
                    return Err("alloca align".into());
                }
                Op::Alloca(t, self.int()? as u32)
            }
            "load" => {
                let volatile = self.eat_word("volatile");
                let t = self.ty()?;
                self.expect(',')?;
                let p = self.typed()?;
                self.expect(',')?;
                if self.word()? != "align" {
                    return Err("load align".into());
                }
                let a = self.int()? as u32;
                let md = self.md_attachments()?;
                return Ok(Inst {
                    res,
                    op: Op::Load(t, p, a, volatile),
                    md,
                });
            }
            "store" => {
                let v = self.typed()?;
                self.expect(',')?;
                let p = self.typed()?;
                self.expect(',')?;
                if self.word()? != "align" {
                    return Err("store align".into());
                }
                let a = self.int()? as u32;
                let md = self.md_attachments()?;
                return Ok(Inst {
                    res,
                    op: Op::Store(v, p, a),
                    md,
                });
            }
            "getelementptr" => {
                let ib = self.eat_word("inbounds");
                let t = self.ty()?;
                self.expect(',')?;
                let p = self.typed()?;
                let mut idx = Vec::new();
                while self.eat(',') {
                    idx.push(self.typed()?);
                }
                Op::Gep(ib, t, p, idx)
            }
            "bitcast" | "zext" | "sext" | "trunc" | "fpext" | "fptrunc" | "addrspacecast"
            | "inttoptr" => {
                let v = self.typed()?;
                if self.word()? != "to" {
                    return Err("cast to".into());
                }
                Op::Cast(opw.clone(), v, self.ty()?)
            }
            "add" | "sub" | "mul" | "sdiv" | "udiv" | "srem" | "urem" | "and" | "or" | "xor"
            | "shl" | "lshr" | "ashr" | "fadd" | "fsub" | "fmul" | "fdiv" | "frem" => {
                let f = self.flags();
                let t = self.ty()?;
                let a = self.value()?;
                self.expect(',')?;
                let b = self.value()?;
                Op::Bin(opw.clone(), f, t, a, b)
            }
            "freeze" => {
                let t = self.ty()?;
                Op::Freeze(t, self.value()?)
            }
            "fneg" => {
                let f = self.flags();
                let t = self.ty()?;
                Op::FNeg(f, t, self.value()?)
            }
            "icmp" | "fcmp" => {
                let f = self.flags();
                let pred = self.word()?;
                let t = self.ty()?;
                let a = self.value()?;
                self.expect(',')?;
                let b = self.value()?;
                Op::Cmp(opw == "fcmp", f, pred, t, a, b)
            }
            "select" => {
                let f = self.flags();
                let c = self.typed()?;
                self.expect(',')?;
                let a = self.typed()?;
                self.expect(',')?;
                let b = self.typed()?;
                Op::Select(f, c, a, b)
            }
            "phi" => {
                let f = self.flags();
                let t = self.ty()?;
                let mut v = Vec::new();
                loop {
                    self.expect('[')?;
                    let x = self.value()?;
                    self.expect(',')?;
                    let l = self.local()?;
                    self.expect(']')?;
                    v.push((x, l));
                    if !self.eat(',') {
                        break;
                    }
                }
                Op::Phi(f, t, v)
            }
            "br" => {
                if self.eat_word("label") {
                    let l = self.local()?;
                    let md = self.md_attachments()?;
                    return Ok(Inst {
                        res,
                        op: Op::Br(l),
                        md,
                    });
                }
                let _ = self.ty()?;
                let c = self.value()?;
                self.expect(',')?;
                self.eat_word("label");
                let a = self.local()?;
                self.expect(',')?;
                self.eat_word("label");
                let b = self.local()?;
                let md = self.md_attachments()?;
                return Ok(Inst {
                    res,
                    op: Op::CondBr(c, a, b),
                    md,
                });
            }
            "switch" => {
                let t = self.ty()?;
                let v = self.value()?;
                self.expect(',')?;
                self.eat_word("label");
                let d = self.local()?;
                self.expect('[')?;
                let mut cases = Vec::new();
                while !self.eat(']') {
                    let _ = self.ty()?;
                    let k = self.value()?;
                    self.expect(',')?;
                    self.eat_word("label");
                    cases.push((k, self.local()?));
                }
                Op::Switch(t, v, d, cases)
            }
            "ret" => {
                if self.eat_word("void") {
                    Op::Ret(None)
                } else {
                    Op::Ret(Some(self.typed()?))
                }
            }
            "unreachable" => Op::Unreachable,
            "call" => {
                let f = self.flags();
                let ra = self.param_attrs()?;
                let rt = self.ty()?;
                let callee = match self.next()? {
                    Tok::Global(g) => g,
                    t => return Err(format!("callee {t:?}")),
                };
                self.expect('(')?;
                let mut args = Vec::new();
                if !self.eat(')') {
                    loop {
                        let t = self.ty()?;
                        let a = self.param_attrs()?;
                        let v = self.value()?;
                        args.push((t, a, v));
                        if self.eat(')') {
                            break;
                        }
                        self.expect(',')?;
                    }
                }
                let g = match self.peek() {
                    Some(Tok::Attr(n)) => {
                        let n = *n;
                        self.i += 1;
                        Some(n)
                    }
                    _ => None,
                };
                Op::Call(f, ra, rt, callee.to_string(), args, g)
            }
            "extractelement" => {
                let v = self.typed()?;
                self.expect(',')?;
                Op::ExtractElt(v, self.typed()?)
            }
            "insertelement" => {
                let v = self.typed()?;
                self.expect(',')?;
                let e = self.typed()?;
                self.expect(',')?;
                Op::InsertElt(v, e, self.typed()?)
            }
            "shufflevector" => {
                let a = self.typed()?;
                self.expect(',')?;
                let b = self.typed()?;
                self.expect(',')?;
                Op::Shuffle(a, b, self.typed()?)
            }
            "extractvalue" | "insertvalue" => {
                let v = self.typed()?;
                let e = if opw == "insertvalue" {
                    self.expect(',')?;
                    Some(self.typed()?)
                } else {
                    None
                };
                let mut idx = Vec::new();
                while self.eat(',') {
                    idx.push(self.int()? as u32);
                }
                match e {
                    Some(e) => Op::InsertVal(v, e, idx),
                    None => Op::ExtractVal(v, idx),
                }
            }
            w => return Err(format!("instruction {w}")),
        };
        let md = self.md_attachments()?;
        Ok(Inst { res, op, md })
    }
}

fn parse_params(p: &mut P, defs: bool) -> R<Vec<ParamDef>> {
    p.expect('(')?;
    let mut v = Vec::new();
    if p.eat(')') {
        return Ok(v);
    }
    loop {
        let ty = p.ty()?;
        let attrs = p.param_attrs()?;
        let name = if defs { Some(p.local()?) } else { None };
        v.push(ParamDef { ty, attrs, name });
        if p.eat(')') {
            return Ok(v);
        }
        p.expect(',')?;
    }
}

/// Parses a whole module.
pub fn parse(text: &str) -> R<Module> {
    let mut m = Module::default();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((ln, line)) = lines.next() {
        let err = |e: String| {
            format!(
                "line {}: {e}: {}",
                ln + 1,
                line.chars().take(120).collect::<String>()
            )
        };
        let t = lex(line).map_err(err)?;
        if t.is_empty() {
            continue;
        }
        let mut p = P { t: &t, i: 0 };
        let r: R<()> = (|| {
            match p.peek().cloned() {
                Some(Tok::Word(w)) if w == "source_filename" => {
                    p.i += 2;
                    if let Tok::Str(s) = p.next()? {
                        m.source = String::from_utf8_lossy(&s).to_string();
                    }
                }
                Some(Tok::Word(w)) if w == "target" => {
                    p.i += 1;
                    let k = p.word()?;
                    p.expect('=')?;
                    if let Tok::Str(s) = p.next()? {
                        let s = String::from_utf8_lossy(&s).to_string();
                        if k == "datalayout" {
                            m.layout = s;
                        } else {
                            m.triple = s;
                        }
                    }
                }
                Some(Tok::Local(n)) => {
                    p.i += 1;
                    p.expect('=')?;
                    if p.word()? != "type" {
                        return Err("type def".into());
                    }
                    if p.eat_word("opaque") {
                        m.types.push((n.to_string(), None));
                    } else {
                        let packed = p.eat('<');
                        p.expect('{')?;
                        let e = p.struct_body('}')?;
                        if packed {
                            p.expect('>')?;
                        }
                        m.types.push((n.to_string(), Some((packed, e))));
                    }
                }
                Some(Tok::Global(n)) => {
                    p.i += 1;
                    p.expect('=')?;
                    let mut linkage = String::new();
                    let mut hidden = false;
                    let mut space = 0;
                    let mut ext_init = false;
                    loop {
                        if p.eat_word("addrspace") {
                            p.expect('(')?;
                            space = p.int()? as u32;
                            p.expect(')')?;
                        } else if p.eat_word("hidden") {
                            hidden = true;
                        } else if p.eat_word("externally_initialized") {
                            ext_init = true;
                        } else if p.is_word("global") || p.is_word("constant") {
                            break;
                        } else {
                            linkage = p.word()?;
                        }
                    }
                    let constant = p.word()? == "constant";
                    let ty = p.ty()?;
                    let init = p.value()?;
                    let mut section = None;
                    let mut align = 0;
                    while p.eat(',') {
                        let w = p.word()?;
                        if w == "section" {
                            if let Tok::Str(s) = p.next()? {
                                section = Some(String::from_utf8_lossy(&s).to_string());
                            }
                        } else if w == "align" {
                            align = p.int()? as u32;
                        }
                    }
                    m.globals.push(Global {
                        name: n.to_string(),
                        linkage,
                        hidden,
                        space,
                        ext_init,
                        constant,
                        ty,
                        init,
                        section,
                        align,
                    });
                }
                Some(Tok::Word(w)) if w == "declare" || w == "define" => {
                    p.i += 1;
                    let mut linkage = String::new();
                    if p.is_word("internal") || p.is_word("linkonce_odr") {
                        linkage = p.word()?;
                    }
                    let ret_attrs = p.param_attrs()?;
                    let ret = p.ty()?;
                    let name = match p.next()? {
                        Tok::Global(g) => g,
                        t => return Err(format!("function name {t:?}")),
                    };
                    let params = parse_params(&mut p, w == "define")?;
                    let mut group = None;
                    let mut section = None;
                    loop {
                        match p.peek().cloned() {
                            Some(Tok::Attr(n)) => {
                                group = Some(n);
                                p.i += 1;
                            }
                            Some(Tok::Word(s)) if s == "section" => {
                                p.i += 1;
                                if let Tok::Str(s) = p.next()? {
                                    section = Some(String::from_utf8_lossy(&s).to_string());
                                }
                            }
                            _ => break,
                        }
                    }
                    let mut f = Function {
                        name: name.to_string(),
                        linkage,
                        ret_attrs,
                        ret,
                        params,
                        group,
                        section,
                        blocks: Vec::new(),
                    };
                    if w == "define" {
                        p.expect('{')?;
                        let mut cur: Option<(String, Vec<Inst>)> = None;
                        for (ln2, l2) in lines.by_ref() {
                            let lt = l2.trim();
                            if lt == "}" {
                                break;
                            }
                            if lt.is_empty() || lt.starts_with(';') {
                                continue;
                            }
                            if let Some(lbl) = lt.strip_suffix(':') {
                                if let Some(c) = cur.take() {
                                    f.blocks.push(c);
                                }
                                cur = Some((lbl.to_string(), Vec::new()));
                                continue;
                            }
                            let toks = lex(lt).map_err(|e| format!("line {}: {e}", ln2 + 1))?;
                            let mut q = P { t: &toks, i: 0 };
                            let ins = q.inst().map_err(|e| {
                                format!(
                                    "line {}: {e}: {}",
                                    ln2 + 1,
                                    lt.chars().take(140).collect::<String>()
                                )
                            })?;
                            match &mut cur {
                                Some((_, v)) => v.push(ins),
                                None => cur = Some(("entry".into(), vec![ins])),
                            }
                        }
                        if let Some(c) = cur.take() {
                            f.blocks.push(c);
                        }
                    }
                    m.functions.push(f);
                }
                Some(Tok::Word(w)) if w == "attributes" => {
                    p.i += 1;
                    let n = match p.next()? {
                        Tok::Attr(n) => n,
                        t => return Err(format!("attributes {t:?}")),
                    };
                    p.expect('=')?;
                    p.expect('{')?;
                    let mut words = Vec::new();
                    while !p.eat('}') {
                        match p.next()? {
                            Tok::Word(w) => words.push(w.into_owned()),
                            Tok::Str(k) => {
                                let k = String::from_utf8_lossy(&k).to_string();
                                if p.eat('=') {
                                    if let Tok::Str(v) = p.next()? {
                                        words
                                            .push(format!("\"{k}={}", String::from_utf8_lossy(&v)));
                                    }
                                } else {
                                    words.push(format!("\"{k}"));
                                }
                            }
                            t => return Err(format!("attribute token {t:?}")),
                        }
                    }
                    m.attr_groups.push((n, words));
                }
                Some(Tok::Meta(name)) => {
                    p.i += 1;
                    p.expect('=')?;
                    p.expect('!')?;
                    p.expect('{')?;
                    let mut v = Vec::new();
                    while !p.eat('}') {
                        if let Tok::MetaNum(n) = p.next()? {
                            v.push(n);
                        }
                        p.eat(',');
                    }
                    m.named_md.push((name.to_string(), v));
                }
                Some(Tok::MetaNum(n)) => {
                    p.i += 1;
                    p.expect('=')?;
                    let distinct = p.eat_word("distinct");
                    p.expect('!')?;
                    p.expect('{')?;
                    let mut ops = Vec::new();
                    while !p.eat('}') {
                        match p.peek().cloned() {
                            Some(Tok::P('!')) => {
                                p.i += 1;
                                if let Tok::Str(s) = p.next()? {
                                    ops.push(MdOp::Str(s.into_owned()));
                                }
                            }
                            Some(Tok::MetaNum(k)) => {
                                p.i += 1;
                                ops.push(MdOp::Node(k));
                            }
                            _ => {
                                let (t, v) = p.typed()?;
                                ops.push(MdOp::Value(t, v));
                            }
                        }
                        p.eat(',');
                    }
                    m.md.push((n, distinct, ops));
                }
                t => return Err(format!("top level {t:?}")),
            }
            Ok(())
        })();
        r.map_err(err)?;
    }
    Ok(m)
}
