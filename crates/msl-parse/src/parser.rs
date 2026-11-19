//! The strict parser and type checker, a port of the prototype's `Parser` class.
//!
//! The structure follows `msl_parse.py` method by method, in the same order of evaluation, so
//! that every accept/reject decision, reason key, detail text and construct counter is the
//! same. Where the prototype has a quirk that decides a verdict, the quirk is kept and marked
//! with "prototype:" in a comment. The main ones:
//!
//! * The lvalue flag `_lv` (here [`Parser::lv`]) is parser state shared by all expression
//!   methods, not a property of the returned type. For example an index reads the flag left
//!   by the index expression, not by the indexed base.
//! * Parsing state survives a rejected item: declared functions, stale scopes and the flag.
//! * Python exceptions other than `Reject` (an `IndexError` past the end of the tokens, a
//!   `KeyError` on a bogus scalar name ...) abort the whole file. They are [`Fail::Crash`].
//! * Python's recursion limit: the prototype runs in a `multiprocessing` worker with the
//!   default limit of 1000 frames, and a deeper nesting ends in `RecursionError`, which
//!   `parse_file` reports as the reason `recursion`. [`Parser`] counts the Python frames the
//!   prototype would have on its stack and fails at the same depth (see `enter` and `leaf`).
//!   This also bounds the Rust stack.

use std::collections::HashSet;
use std::rc::Rc;

use msl_lex::{py_repr, Span, TokKind, Token};

use crate::ast::*;
use crate::prelude;
use crate::stats::{Counter, FxHashMap};
use crate::ty::*;
use crate::{Fail, Reject};

type PResult<T> = Result<T, Fail>;

/// Python frames below `Parser.run` in a `multiprocessing.Pool` worker: `parse_file` runs at
/// depth 8 (measured with Python 3.14, spawn start method).
pub const PY_BASE_DEPTH: u32 = 8;
/// `sys.getrecursionlimit()` in the worker.
pub const PY_RECURSION_LIMIT: u32 = 1000;

/// The pragma that turns off `a*b + c` contraction, at file scope or at the start of a block.
pub const FP_CONTRACT_OFF: &str = "#pragma clang fp contract(off)";

fn reject<T>(key: impl Into<String>) -> PResult<T> {
    Err(Fail::Reject(Box::new(Reject {
        key: key.into(),
        detail: String::new(),
    })))
}

fn reject_d<T>(key: impl Into<String>, detail: String) -> PResult<T> {
    Err(Fail::Reject(Box::new(Reject {
        key: key.into(),
        detail,
    })))
}

/// `Ok(e)`: expressions travel boxed so that results stay small.
#[inline]
fn bx(e: Expr) -> PResult<Box<Expr>> {
    Ok(Box::new(e))
}

fn crash<T>(e: PyExc) -> PResult<T> {
    Err(Fail::Crash(e))
}

/// Python's `int(s)` for a decimal string of ASCII digits: the value, saturated at u128::MAX.
fn py_int(s: &str) -> PResult<u128> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return crash(PyExc::ValueError);
    }
    Ok(s.bytes().fold(0u128, |acc, b| {
        acc.saturating_mul(10).saturating_add((b - b'0') as u128)
    }))
}

/// `int(n.rstrip('uU'))` as the canonical decimal text (no leading zeros).
fn py_int_text(n: &str) -> PResult<Rc<str>> {
    let digits = n.trim_end_matches(['u', 'U']);
    py_int(digits)?;
    let t = digits.trim_start_matches('0');
    Ok(if t.is_empty() { "0".into() } else { t.into() })
}

/// `re.fullmatch(r'_\d+', s)`
fn is_ssa_name(s: &str) -> bool {
    s.len() > 1 && s.starts_with('_') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

/// `re.fullmatch(r'-?\d+[uU]?', s)` (with `neg`) or `re.fullmatch(r'\d+[uU]?', s)`.
fn is_int_literal(s: &str, neg: bool) -> bool {
    let s = if neg {
        s.strip_prefix('-').unwrap_or(s)
    } else {
        s
    };
    let s = s.strip_suffix(['u', 'U']).unwrap_or(s);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Python's `str.split()` (whitespace runs).
fn py_split(s: &str) -> Vec<&str> {
    s.split(msl_lex::py_isspace)
        .filter(|p| !p.is_empty())
        .collect()
}

const ADDR: [&str; 4] = ["device", "constant", "threadgroup", "thread"];
const TEX_ACCEPTED: [&str; 9] = [
    "texture2d",
    "texture2d_array",
    "texture3d",
    "texturecube",
    "texturecube_array",
    "depth2d",
    "depth2d_array",
    "depthcube",
    "depthcube_array",
];
const FIELD_ATTRS: [&str; 16] = [
    "id",
    "user",
    "attribute",
    "position",
    "invariant",
    "point_size",
    "color",
    "flat",
    "function_constant",
    "center_no_perspective",
    "depth",
    "render_target_array_index",
    "center_perspective",
    "sample_mask",
    "clip_distance",
    "front_facing",
];
const PARAM_ATTRS: [&str; 16] = [
    "stage_in",
    "buffer",
    "position",
    "vertex_id",
    "instance_id",
    "base_instance",
    "color",
    "function_constant",
    "front_facing",
    "thread_position_in_grid",
    "threadgroup_position_in_grid",
    "thread_position_in_threadgroup",
    "point_coord",
    "sample_id",
    "thread_index_in_simdgroup",
    "thread_index_in_threadgroup",
];
const ASSIGN_OPS: [&str; 11] = [
    "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=",
];
const PREC: [&[&str]; 10] = [
    &["||"],
    &["&&"],
    &["|"],
    &["^"],
    &["&"],
    &["==", "!="],
    &["<", ">", "<=", ">="],
    &["<<", ">>"],
    &["+", "-"],
    &["*", "/", "%"],
];

const F1: [&str; 28] = [
    "abs",
    "sqrt",
    "rsqrt",
    "exp2",
    "log2",
    "floor",
    "ceil",
    "trunc",
    "rint",
    "round",
    "sin",
    "cos",
    "tan",
    "fract",
    "saturate",
    "sign",
    "dfdx",
    "dfdy",
    "fwidth",
    "exp",
    "log",
    "asin",
    "acos",
    "atan",
    "sinh",
    "cosh",
    "tanh",
    "normalize",
];
const F2: [&str; 13] = [
    "min",
    "max",
    "pow",
    "powr",
    "atan2",
    "fmod",
    "step",
    "precise::min",
    "precise::max",
    "fast::min",
    "fast::max",
    "fmin",
    "fmax",
];
const F3: [&str; 6] = [
    "fma",
    "clamp",
    "precise::clamp",
    "fast::clamp",
    "mix",
    "smoothstep",
];
const I1: [&str; 4] = ["popcount", "clz", "ctz", "reverse_bits"];

fn in_f1(name: &str) -> bool {
    F1.contains(&name)
}

/// `UNPACK`: result width of the unpack builtins.
fn unpack_width(name: &str) -> Option<u8> {
    Some(match name {
        "unpack_unorm10a2_to_float"
        | "unpack_unorm4x8_to_float"
        | "unpack_snorm4x8_to_float"
        | "unpack_unorm4x8_srgb_to_float" => 4,
        "unpack_unorm2x16_to_float" | "unpack_snorm2x16_to_float" | "unpack_half2x16" => 2,
        _ => return None,
    })
}

/// `OPTIONS`: sampling options and their argument counts.
fn option(name: &str) -> Option<(&'static str, usize)> {
    Some(match name {
        "level" => ("level", 1),
        "bias" => ("bias", 1),
        "min_lod_clamp" => ("min_lod_clamp", 1),
        "gradient2d" => ("gradient2d", 2),
        "gradient3d" => ("gradient3d", 2),
        "gradientcube" => ("gradientcube", 2),
        _ => return None,
    })
}

/// A struct's fields: (name, type). `None` for `spvUnsafeArray`, which the prototype
/// registers as a struct without a field list.
type Fields = Option<Rc<[(Rc<str>, Ty)]>>;

/// Result of checking one file.
#[derive(Debug, Default)]
pub struct Checked {
    pub module: Module,
    /// Reason key -> count, in first-occurrence order. Empty means accepted.
    pub reasons: Counter,
    /// The prototype's construct counters.
    pub stats: Counter,
    /// The detail of the last rejection (`last_detail`).
    pub last_detail: String,
    /// The deepest emulated Python frame depth reached.
    pub max_depth: u32,
}

pub struct Parser<'a> {
    toks: Vec<Token<'a>>,
    /// Per token: its level in `PREC` when it is a binary operator punctuator, else `NO_LEVEL`.
    prec: Vec<u8>,
    i: usize,
    structs: FxHashMap<Rc<str>, Fields>,
    funcs: FxHashMap<Rc<str>, Vec<Rc<Signature>>>,
    globals: FxHashMap<Rc<str>, Ty>,
    prelude: HashSet<&'static str>,
    /// A replacement for the built-in helper table (None: built-in).
    prelude_table: Option<Rc<prelude::Table>>,
    scopes: Vec<FxHashMap<Rc<str>, (Ty, bool)>>,
    cur: Option<Rc<Signature>>,
    stats: Counter,
    assigned: Counter,
    /// prototype: `_lv`, the "last primary was an lvalue" flag.
    lv: bool,
    /// prototype: `_lvname`, the variable the flag refers to.
    lvname: Option<Rc<str>>,
    /// Emulated Python frame depth (see the module documentation).
    depth: u32,
    max_depth: u32,
    limit: u32,
    /// Interned identifier texts.
    names: FxHashMap<&'a str, Rc<str>>,
    /// Reused buffer for counter keys.
    keybuf: String,
    /// The last parsed type had a `const` qualifier (kept for the IR metadata, not for verdicts).
    last_const: bool,
}

impl<'a> Parser<'a> {
    /// A parser over `toks` (as the lexer made them, without the end marker).
    pub fn new(mut toks: Vec<Token<'a>>, src_len: usize) -> Parser<'a> {
        toks.push(Token {
            kind: TokKind::Eof,
            text: "<eof>",
            line: 0,
            span: Span::new(src_len, src_len),
        });
        let prec = toks.iter().map(prec_level).collect();
        Parser {
            toks,
            prec,
            i: 0,
            structs: FxHashMap::default(),
            funcs: FxHashMap::default(),
            globals: FxHashMap::default(),
            prelude: HashSet::new(),
            prelude_table: None,
            scopes: Vec::new(),
            cur: None,
            stats: Counter::new(),
            assigned: Counter::new(),
            lv: false,
            lvname: None,
            depth: PY_BASE_DEPTH,
            max_depth: PY_BASE_DEPTH,
            limit: PY_RECURSION_LIMIT,
            names: FxHashMap::default(),
            keybuf: String::new(),
            last_const: false,
        }
    }

    /// Uses `table` instead of the built-in helper table.
    pub fn with_prelude_table(mut self, table: Rc<prelude::Table>) -> Self {
        self.prelude_table = Some(table);
        self
    }

    /// Overrides the emulated recursion limit (for tests).
    pub fn with_recursion_limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }

    // ------------------------------------------------------------ frames and counters

    /// Runs `f` as one Python method frame.
    #[inline]
    fn frame<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.max_depth = self.depth;
        }
        if self.depth > self.limit {
            self.depth -= 1;
            return crash(PyExc::RecursionError);
        }
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// A call to a non-recursive Python helper `extra` frames deep (token helpers, `re`
    /// functions, `Counter.__missing__`, `Ty()`).
    #[inline]
    fn leaf(&mut self, extra: u32) -> PResult<()> {
        let d = self.depth + extra;
        if d > self.max_depth {
            self.max_depth = d;
        }
        if d > self.limit {
            return crash(PyExc::RecursionError);
        }
        Ok(())
    }

    /// `self.stats[key] += 1`
    #[inline]
    fn stat(&mut self, key: &str) -> PResult<()> {
        if self.stats.inc(key) {
            self.leaf(1)?;
        }
        Ok(())
    }

    /// `self.stats[prefix + suffix] += 1`
    fn stat2(&mut self, prefix: &str, suffix: &str) -> PResult<()> {
        let mut k = std::mem::take(&mut self.keybuf);
        k.clear();
        k.push_str(prefix);
        k.push_str(suffix);
        let r = self.stat(&k);
        self.keybuf = k;
        r
    }

    fn intern(&mut self, s: &'a str) -> Rc<str> {
        self.names.entry(s).or_insert_with(|| Rc::from(s)).clone()
    }

    // ------------------------------------------------------------ token helpers

    #[inline]
    fn tok(&self, o: usize) -> PResult<&Token<'a>> {
        match self.toks.get(self.i + o) {
            Some(t) => Ok(t),
            None => crash(PyExc::IndexError),
        }
    }

    #[inline]
    fn peek(&mut self) -> PResult<&'a str> {
        self.leaf(1)?;
        Ok(self.tok(0)?.text)
    }

    #[inline]
    fn peek_at(&mut self, o: usize) -> PResult<&'a str> {
        self.leaf(1)?;
        Ok(self.tok(o)?.text)
    }

    #[inline]
    fn kind(&mut self) -> PResult<TokKind> {
        self.leaf(1)?;
        Ok(self.tok(0)?.kind)
    }

    #[inline]
    fn kind_at(&mut self, o: usize) -> PResult<TokKind> {
        self.leaf(1)?;
        Ok(self.tok(o)?.kind)
    }

    #[inline]
    fn next(&mut self) -> PResult<&'a str> {
        self.leaf(1)?;
        let t = self.tok(0)?.text;
        self.i += 1;
        Ok(t)
    }

    fn expect(&mut self, v: &str) -> PResult<()> {
        self.leaf(2)?;
        let got = self.tok(0)?;
        if got.text != v {
            let detail = format!(
                "expected {} got {} line {}",
                py_repr(v),
                py_repr(got.text),
                got.line
            );
            return reject_d("syntax", detail);
        }
        self.i += 1;
        Ok(())
    }

    #[inline]
    fn accept(&mut self, v: &str) -> PResult<bool> {
        self.leaf(2)?;
        if self.tok(0)?.text == v {
            self.i += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn ident(&mut self) -> PResult<&'a str> {
        self.leaf(2)?;
        let t = *self.tok(0)?;
        if t.kind != TokKind::Id {
            let detail = format!(
                "expected identifier got {} line {}",
                py_repr(t.text),
                t.line
            );
            return reject_d("syntax", detail);
        }
        self.i += 1;
        Ok(t.text)
    }

    /// The span from token `start` to the last consumed token.
    fn span_from(&self, start: usize) -> Span {
        let a = self.toks[start.min(self.toks.len() - 1)].span;
        if self.i > start {
            let b = self.toks[(self.i - 1).min(self.toks.len() - 1)].span;
            a.to(b)
        } else {
            Span {
                start: a.start,
                end: a.start,
            }
        }
    }

    // ------------------------------------------------------------ item boundaries

    /// `item_end(start)`: the token index after the top-level item that starts at `start`.
    fn item_end(&self, start: usize) -> usize {
        let mut d: i64 = 0;
        let mut j = start;
        while self.toks[j].kind != TokKind::Eof {
            let t = &self.toks[j];
            if t.kind == TokKind::Pp && j == start {
                return j + 1;
            }
            match t.text {
                "{" | "(" | "[" => d += 1,
                "[[" => d += 2,
                "}" | ")" | "]" => {
                    d -= 1;
                    if d == 0 && t.text == "}" {
                        if self.toks[j + 1].text == ";" {
                            j += 1;
                        }
                        return j + 1;
                    }
                }
                ";" if d == 0 => return j + 1,
                _ => {}
            }
            j += 1;
        }
        j
    }

    // ------------------------------------------------------------ top level

    /// Hashes of the template items in the token stream, as `--make-prelude` computes them.
    pub fn template_hashes(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while self.toks[i].kind != TokKind::Eof {
            let end = self.item_end(i);
            if self.toks[i].text == "template" {
                out.push(prelude::text_hash(&self.toks[i..end]));
            }
            i = end;
        }
        out
    }

    /// `run()`: checks the whole file with item-level recovery.
    pub fn run(mut self) -> Result<Checked, PyExc> {
        let mut module = Module::default();
        let mut reasons = Counter::new();
        let mut last_detail = String::new();
        let r = self.frame(|p| {
            while p.kind()? != TokKind::Eof {
                let start = p.i;
                let end = p.item_end(start);
                let res = p.top_item(end).and_then(|item| {
                    if p.i != end {
                        reject_d("syntax", format!("item ended at {} not {}", p.i, end))
                    } else {
                        Ok(item)
                    }
                });
                match res {
                    Ok(Some(item)) => module.items.push(item),
                    Ok(None) => {}
                    Err(Fail::Reject(rj)) => {
                        reasons.inc(&rj.key);
                        last_detail = rj.detail.clone();
                        p.i = end;
                        let span = p.span_from(start);
                        module.items.push(Item::Rejected {
                            reason: rj.key,
                            detail: rj.detail,
                            span,
                        });
                    }
                    Err(crash) => return Err(crash),
                }
            }
            Ok(())
        });
        match r {
            Ok(()) => {}
            Err(Fail::Crash(e)) => return Err(e),
            Err(Fail::Reject(_)) => unreachable!("rejections are caught per item"),
        }
        let mut stats = self.stats;
        let reassigned = self.assigned.iter().filter(|(k, _)| is_ssa_name(k)).count() as u64;
        stats.set("ssa-names-reassigned", reassigned);
        Ok(Checked {
            module,
            reasons,
            stats,
            last_detail,
            max_depth: self.max_depth,
        })
    }

    fn top_item(&mut self, end: usize) -> PResult<Option<Item>> {
        self.frame(|p| p.top_item_body(end))
    }

    fn top_item_body(&mut self, end: usize) -> PResult<Option<Item>> {
        let start = self.i;
        let k = self.kind()?;
        let v = self.peek()?;
        if k == TokKind::Pp {
            self.next()?;
            if v == "#include <metal_stdlib>"
                || v == "#include <simd/simd.h>"
                || v.starts_with("#pragma clang diagnostic ignored")
                || v == FP_CONTRACT_OFF
            {
                self.stat("pp")?;
                return Ok(Some(Item::Preprocessor {
                    text: v.to_string(),
                    span: self.span_from(start),
                }));
            }
            let parts = py_split(v);
            return reject(format!(
                "pp:{} {}",
                parts[0],
                parts.get(1).copied().unwrap_or("")
            ));
        }
        if v == "using" {
            self.next()?;
            self.expect("namespace")?;
            self.expect("metal")?;
            self.expect(";")?;
            return Ok(Some(Item::UsingNamespaceMetal {
                span: self.span_from(start),
            }));
        }
        if v == "template" {
            return self.template_item(end).map(Some);
        }
        if v == "struct" {
            return self.struct_item().map(Some);
        }
        if v == "constant" && self.peek_at(1)? != "_16" && self.is_global_const(end)? {
            return self.global_const().map(Some);
        }
        self.function().map(Some)
    }

    fn template_item(&mut self, end: usize) -> PResult<Item> {
        let start = self.i;
        let toks = &self.toks[self.i..end];
        let h = prelude::text_hash(toks);
        let table = self.prelude_table.clone();
        let name: Option<&str> = match &table {
            Some(t) => t.lookup(&h),
            None => prelude::lookup(&h),
        };
        if let Some(name) = name.and_then(prelude::allowed) {
            self.prelude.insert(name);
            self.i = end;
            self.stat2("prelude:", name)?;
            if name == "spvUnsafeArray" {
                self.structs.insert("spvUnsafeArray".into(), None);
            }
            return Ok(Item::Prelude {
                name,
                hash: h,
                span: self.span_from(start),
            });
        }
        if let Some(name) = name {
            return reject(format!("template:{name}"));
        }
        // prototype: the name of the first `id (` pair (j = 0 looks at the last token).
        let mut nm = "?";
        for j in 0..toks.len() {
            let prev = if j == 0 {
                &toks[toks.len() - 1]
            } else {
                &toks[j - 1]
            };
            if toks[j].text == "("
                && prev.kind == TokKind::Id
                && !matches!(
                    prev.text,
                    "__attribute__" | "always_inline" | "METAL_CONST_ARG"
                )
            {
                nm = prev.text;
                break;
            }
        }
        if toks.iter().take(12).any(|t| t.text == "struct") {
            let idx = toks.iter().position(|t| t.text == "struct").unwrap_or(0);
            match toks.get(idx + 1) {
                Some(t) => return reject(format!("template:struct {}", t.text)),
                None => return crash(PyExc::IndexError),
            }
        }
        reject(format!("template:{nm}"))
    }

    fn struct_item(&mut self) -> PResult<Item> {
        let start = self.i;
        self.next()?;
        let name = self.ident()?;
        let name = self.intern(name);
        if self.peek()? != "{" {
            return reject("struct-forward");
        }
        self.next()?;
        let mut fields: Vec<Field> = Vec::new();
        while !self.accept("}")? {
            let ty = self.type_()?;
            let is_const = self.last_const;
            loop {
                let fname = self.ident()?;
                let fname = self.intern(fname);
                let fty = self.array_suffix(ty.clone())?;
                let attrs = self.attrs()?;
                for a in &attrs {
                    if !FIELD_ATTRS.contains(&a.name.as_str()) {
                        return reject(format!("field-attr:{}", a.name));
                    }
                }
                if attrs.iter().any(|a| a.name == "depth" && a.args != ["any"]) {
                    // prototype: the detail lists the arguments of the *first* attribute.
                    return reject(format!("field-attr:depth({})", attrs[0].args.join(",")));
                }
                fields.push(Field {
                    name: fname,
                    ty: fty,
                    attrs,
                    is_const,
                });
                if !self.accept(",")? {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.expect(";")?;
        let table: Rc<[(Rc<str>, Ty)]> = fields
            .iter()
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect();
        self.structs.insert(name.clone(), Some(table));
        self.stat("struct")?;
        Ok(Item::Struct(StructDef {
            name,
            fields,
            span: self.span_from(start),
        }))
    }

    fn global_const(&mut self) -> PResult<Item> {
        let start = self.i;
        self.next()?;
        let ty = self.type_()?;
        let name = self.ident()?;
        let name = self.intern(name);
        if self.peek()? == "[" {
            let ty = self.array_suffix(ty)?;
            self.expect("=")?;
            let init = self.init_list()?;
            self.expect(";")?;
            self.globals
                .insert(name.clone(), Ty::Ref(Space::Constant, Rc::new(ty.clone())));
            self.stat("const-array")?;
            return Ok(Item::ConstArray {
                name,
                ty,
                init,
                span: self.span_from(start),
            });
        }
        let attrs = self.attrs()?;
        if attrs.first().is_some_and(|a| a.name == "function_constant") {
            self.expect(";")?;
            self.globals.insert(name.clone(), ty.clone());
            self.stat("function_constant")?;
            return Ok(Item::FunctionConstant {
                name,
                ty,
                attrs,
                span: self.span_from(start),
            });
        }
        reject("global-constant-form")
    }

    fn function(&mut self) -> PResult<Item> {
        let start = self.i;
        let attrs0 = self.attrs()?;
        for a in &attrs0 {
            if a.name != "early_fragment_tests" {
                return reject(format!("fn-attr:{}", a.name));
            }
        }
        let mut is_static = false;
        let mut always_inline = false;
        let mut is_inline = false;
        while matches!(self.peek()?, "static" | "inline" | "__attribute__") {
            if self.next()? == "__attribute__" {
                self.expect("(")?;
                self.expect("(")?;
                let a = self.ident()?;
                self.expect(")")?;
                self.expect(")")?;
                if a != "always_inline" {
                    return reject(format!("gnu-attr:{a}"));
                }
                always_inline = true;
            } else {
                if self.toks[self.i - 1].text == "inline" {
                    is_inline = true;
                }
                is_static = true;
            }
        }
        let mut entry = None;
        if matches!(self.peek()?, "vertex" | "fragment" | "kernel") {
            entry = Stage::from_name(self.next()?);
        }
        let ret = if self.peek()? != "void" {
            self.type_()?
        } else {
            self.next()?;
            Ty::Void
        };
        let name = self.ident()?;
        let name = self.intern(name);
        self.expect("(")?;
        let mut params = Vec::new();
        if !self.accept(")")? {
            loop {
                let (pty, array_ref) = self.frame(|p| p.type_body(entry.is_none(), false))?;
                let is_const = self.last_const;
                if array_ref.is_some() {
                    self.expect("(")?;
                    self.expect("&")?;
                }
                let pname = self.ident()?;
                let pname = self.intern(pname);
                if array_ref.is_some() {
                    self.expect(")")?;
                    if self.peek()? != "[" {
                        return reject("array-reference-dimension");
                    }
                }
                let pty = self.array_suffix(pty)?;
                let pty = match array_ref {
                    Some(space) => Ty::Ref(space, Rc::new(pty)),
                    None => pty,
                };
                let pattrs = self.attrs()?;
                for a in &pattrs {
                    if !PARAM_ATTRS.contains(&a.name.as_str()) {
                        return reject(format!("param-attr:{}", a.name));
                    }
                    if a.name == "function_constant" {
                        match a.args.first() {
                            None => return crash(PyExc::IndexError),
                            Some(g) if !self.globals.contains_key(g.as_str()) => {
                                return reject("function_constant-unknown")
                            }
                            Some(_) => {}
                        }
                    }
                }
                params.push(Param {
                    name: pname,
                    ty: pty,
                    attrs: pattrs,
                    is_const,
                });
                if self.accept(")")? {
                    break;
                }
                self.expect(",")?;
            }
        }
        if self.peek()? == ";" {
            return reject("fn-prototype");
        }
        let sig = Rc::new(Signature {
            name: name.clone(),
            entry,
            ret,
            params,
        });
        self.funcs.entry(name).or_default().push(sig.clone());
        match entry {
            Some(st) => {
                let n = match st {
                    Stage::Vertex => "entry:vertex",
                    Stage::Fragment => "entry:fragment",
                    Stage::Kernel => "entry:kernel",
                };
                self.stat(n)?;
            }
            None => self.stat("function")?,
        }
        self.cur = Some(sig.clone());
        let mut scope = FxHashMap::default();
        for p in &sig.params {
            scope.insert(p.name.clone(), (p.ty.clone(), true));
        }
        self.scopes = vec![scope];
        let body = self.block()?;
        self.scopes = Vec::new();
        Ok(Item::Function(Rc::new(Function {
            sig,
            attrs: attrs0,
            is_static,
            is_inline,
            always_inline,
            body,
            span: self.span_from(start),
        })))
    }

    /// `is_global_const(end)`
    fn is_global_const(&mut self, end: usize) -> PResult<bool> {
        self.frame(|p| {
            let toks = &p.toks[p.i..end];
            let has_init = toks.iter().any(|t| t.text == "[[" || t.text == "=");
            let has_body = (p.i..end).any(|j| p.toks[j].text == "{" && p.toks[j - 1].text == ")");
            Ok(has_init && !has_body)
        })
    }

    /// `init_list(ty)`: `{ e, e, ... }` with optional commas.
    fn init_list(&mut self) -> PResult<Vec<Expr>> {
        self.frame(|p| {
            p.expect("{")?;
            let mut out = Vec::new();
            while !p.accept("}")? {
                out.push(*p.expr_assign()?);
                p.accept(",")?;
            }
            Ok(out)
        })
    }

    fn attrs(&mut self) -> PResult<Vec<Attr>> {
        self.frame(|p| {
            let mut out = Vec::new();
            while p.peek()? == "[[" {
                p.next()?;
                loop {
                    let mut name = p.ident()?.to_string();
                    if p.accept("::")? {
                        name.push_str("::");
                        name.push_str(p.ident()?);
                    }
                    let mut args = Vec::new();
                    if p.accept("(")? {
                        while !p.accept(")")? {
                            let a = p.next()?;
                            if a != "," {
                                args.push(a.to_string());
                            }
                        }
                    }
                    out.push(Attr { name, args });
                    if !p.accept(",")? {
                        break;
                    }
                }
                p.expect("]")?;
                p.expect("]")?;
            }
            Ok(out)
        })
    }

    fn array_suffix(&mut self, ty: Ty) -> PResult<Ty> {
        self.frame(|p| {
            let mut dims = Vec::new();
            while p.accept("[")? {
                let n = p.next()?;
                p.leaf(2)?;
                if !is_int_literal(n, false) {
                    return reject("array-dim-nonliteral");
                }
                p.expect("]")?;
                dims.push(py_int_text(n)?);
            }
            let mut ty = ty;
            for n in dims.into_iter().rev() {
                ty = Ty::Array {
                    elem: Rc::new(ty),
                    n,
                    unsafe_: false,
                    metal: false,
                };
            }
            Ok(ty)
        })
    }

    // ------------------------------------------------------------ types

    fn is_type_start(&mut self, o: usize) -> PResult<bool> {
        self.frame(|p| {
            let v = p.peek_at(o)?;
            if ADDR.contains(&v) || v == "const" {
                return Ok(true);
            }
            if p.kind_at(o)? != TokKind::Id {
                return Ok(false);
            }
            Ok(vec_name(v).is_some()
                || mat_name(v).is_some()
                || p.structs.contains_key(v)
                || tex_dim(v).is_some()
                || matches!(v, "sampler" | "spvUnsafeArray" | "void" | "size_t")
                || (v == "array" && p.peek_at(o + 1)? == "<"))
        })
    }

    fn type_(&mut self) -> PResult<Ty> {
        self.frame(|p| p.type_body(false, false).map(|(ty, _)| ty))
    }

    fn type_body(&mut self, helper_param: bool, kernel_local: bool) -> PResult<(Ty, Option<Space>)> {
        let mut space: Option<Space> = None;
        self.last_const = false;
        loop {
            let v = self.peek()?;
            if !(ADDR.contains(&v) || v == "const") {
                break;
            }
            let v = self.next()?;
            if v != "const" {
                space = Space::from_name(v);
            } else {
                self.last_const = true;
            }
        }
        let mut v = self.ident()?;
        if v == "const" {
            self.last_const = true;
            v = self.ident()?;
        }
        if v == "atomic_uint" && space == Some(Space::Threadgroup) && !self.last_const {
            self.expect("*")?;
            return Ok((Ty::ThreadgroupAtomicUintPtr, None));
        }
        let mut t = if let Some((packed, base, w)) = vec_name(v) {
            if packed {
                self.stat("packed")?;
                match w {
                    Some(w) => Ty::Vec {
                        base: ScalarName::Known(base),
                        w,
                        packed: true,
                    },
                    // prototype: int(None)
                    None => return crash(PyExc::TypeError),
                }
            } else {
                vk(base, w.unwrap_or(1))
            }
        } else if let Some((base, cols, rows)) = mat_name(v) {
            Ty::Mat { base, cols, rows }
        } else if v == "size_t" {
            s(Scalar::SizeT)
        } else if v == "void" {
            Ty::Void
        } else if v == "sampler" {
            Ty::Sampler
        } else if tex_dim(v).is_some() {
            self.expect("<")?;
            let el = self.ident()?;
            let mut acc = "sample";
            if self.accept(",")? {
                self.expect("access")?;
                self.expect("::")?;
                acc = self.ident()?;
            }
            self.expect(">")?;
            let el = match Scalar::from_name(el) {
                Some(k @ (Scalar::Float | Scalar::Half | Scalar::Int | Scalar::Uint)) => k,
                _ => return reject(format!("texture-elem:{el}")),
            };
            if !TEX_ACCEPTED.contains(&v) {
                return reject(format!("texture-kind:{v}"));
            }
            Ty::Tex {
                kind: self.intern(v),
                el,
                access: self.intern(acc),
            }
        } else if v == "spvUnsafeArray" {
            if !self.prelude.contains("spvUnsafeArray") {
                return reject("spvUnsafeArray-without-prelude");
            }
            self.expect("<")?;
            let el = self.type_()?;
            self.expect(",")?;
            let n = self.next()?;
            self.expect(">")?;
            Ty::Array {
                elem: Rc::new(el),
                n: py_int_text(n)?,
                unsafe_: true,
                metal: false,
            }
        } else if v == "array" && self.peek()? == "<" {
            self.expect("<")?;
            let el = self.type_()?;
            if !matches!(el, Ty::Tex { .. } | Ty::Sampler) {
                return reject("metal-array-elem");
            }
            self.expect(",")?;
            let n = self.next()?;
            if !is_int_literal(n, false) {
                return reject("array-dim-nonliteral");
            }
            self.expect(">")?;
            self.stat("type:metal-array")?;
            Ty::Array {
                elem: Rc::new(el),
                n: py_int_text(n)?,
                unsafe_: false,
                metal: true,
            }
        } else if self.structs.contains_key(v) {
            Ty::Struct(self.intern(v))
        } else {
            return reject(format!("type:{v}"));
        };
        loop {
            let pk = self.peek()?;
            if !matches!(pk, "*" | "&" | "const") {
                break;
            }
            let pk = self.next()?;
            if pk == "const" {
                continue;
            }
            let sp = space.unwrap_or(Space::Thread);
            t = if pk == "*" {
                Ty::Ptr(sp, Rc::new(t))
            } else {
                Ty::Ref(sp, Rc::new(t))
            };
            if pk == "&" && space.is_none() {
                return reject("thread-reference");
            }
        }
        if let Some(sp) = space {
            if !matches!(t, Ty::Ptr(..) | Ty::Ref(..)) {
                // The translator passes arrays to helpers by reference.
                // The address-space qualifier applies to the whole array.
                if helper_param
                    && matches!(sp, Space::Thread | Space::Threadgroup)
                    && self.peek()? == "("
                    && self.peek_at(1)? == "&"
                {
                    return Ok((t, Some(sp)));
                }
                if kernel_local && sp == Space::Threadgroup {
                    return Ok((t, Some(sp)));
                }
                if sp != Space::Constant {
                    return reject(format!("addrspace-value:{}", sp.name()));
                }
            }
        }
        Ok((t, None))
    }

    // ------------------------------------------------------------ statements

    fn lookup(&mut self, name: &str) -> PResult<Option<(Ty, bool, VarScope)>> {
        self.frame(|p| {
            for s in p.scopes.iter().rev() {
                if let Some((t, lv)) = s.get(name) {
                    return Ok(Some((t.clone(), *lv, VarScope::Local)));
                }
            }
            Ok(p.globals
                .get(name)
                .map(|t| (t.clone(), false, VarScope::Global)))
        })
    }

    fn block(&mut self) -> PResult<Block> {
        self.frame(|p| {
            let start = p.i;
            p.expect("{")?;
            p.scopes.push(FxHashMap::default());
            let contract_off = p.kind()? == TokKind::Pp && p.peek()? == FP_CONTRACT_OFF;
            if contract_off {
                p.next()?;
                p.stat("pp:fp-contract")?;
            }
            let mut stmts = Vec::new();
            while !p.accept("}")? {
                stmts.push(p.stmt()?);
            }
            p.scopes.pop();
            Ok(Block {
                stmts,
                contract_off,
                span: p.span_from(start),
            })
        })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        self.frame(|p| p.stmt_body())
    }

    fn stmt_body(&mut self) -> PResult<Stmt> {
        let v = self.peek()?;
        self.stat("stmt")?;
        match v {
            "{" => {
                self.stat("stmt:block")?;
                return Ok(Stmt::Block(self.block()?));
            }
            ";" => {
                self.next()?;
                return Ok(Stmt::Empty);
            }
            "if" => {
                self.next()?;
                self.expect("(")?;
                let c = self.expr()?;
                self.expect(")")?;
                self.cond(&c.ty)?;
                self.stat("stmt:if")?;
                let then = Box::new(self.sub_stmt()?);
                let mut els = None;
                if self.accept("else")? {
                    self.stat("stmt:else")?;
                    els = Some(Box::new(self.sub_stmt()?));
                }
                return Ok(Stmt::If { cond: c, then, els });
            }
            "switch" => {
                self.next()?;
                self.expect("(")?;
                let c = self.expr()?;
                let ct = c.ty.strip();
                self.expect(")")?;
                if !(matches!(ct, Ty::Scalar(_)) && ct.is_int()?) {
                    return reject("switch-non-int");
                }
                self.stat("stmt:switch")?;
                self.expect("{")?;
                self.scopes.push(FxHashMap::default());
                let mut body = Vec::new();
                while !self.accept("}")? {
                    if self.accept("case")? {
                        let lab = self.next()?;
                        self.leaf(2)?;
                        if !is_int_literal(lab, true) {
                            return reject(format!(
                                "case-label:{}",
                                self.toks[self.i - 1].kind.name()
                            ));
                        }
                        self.expect(":")?;
                        body.push(SwitchEntry::Case(lab.to_string()));
                    } else if self.accept("default")? {
                        self.expect(":")?;
                        body.push(SwitchEntry::Default);
                    } else {
                        body.push(SwitchEntry::Stmt(self.stmt()?));
                    }
                }
                self.scopes.pop();
                return Ok(Stmt::Switch { cond: c, body });
            }
            "do" => {
                self.next()?;
                self.stat("stmt:do")?;
                let body = Box::new(self.sub_stmt()?);
                self.expect("while")?;
                self.expect("(")?;
                let c = self.expr()?;
                self.cond(&c.ty)?;
                self.expect(")")?;
                self.expect(";")?;
                return Ok(Stmt::DoWhile { body, cond: c });
            }
            "while" => {
                self.next()?;
                self.stat("stmt:while")?;
                self.expect("(")?;
                let c = self.expr()?;
                self.cond(&c.ty)?;
                self.expect(")")?;
                let body = Box::new(self.sub_stmt()?);
                return Ok(Stmt::While { cond: c, body });
            }
            "for" => {
                self.next()?;
                self.stat("stmt:for")?;
                self.expect("(")?;
                self.scopes.push(FxHashMap::default());
                let mut init = None;
                if !self.accept(";")? {
                    if self.is_decl()? {
                        init = Some(ForInit::Decl(self.decl()?));
                    } else {
                        init = Some(ForInit::Expr(self.expr()?));
                        self.expect(";")?;
                    }
                }
                let mut cond = None;
                if !self.accept(";")? {
                    let c = self.expr()?;
                    self.cond(&c.ty)?;
                    cond = Some(c);
                    self.expect(";")?;
                }
                let mut step = None;
                if self.peek()? != ")" {
                    step = Some(self.expr()?);
                }
                self.expect(")")?;
                let body = Box::new(self.sub_stmt()?);
                self.scopes.pop();
                return Ok(Stmt::For {
                    init,
                    cond,
                    step,
                    body,
                });
            }
            "break" | "continue" => {
                self.next()?;
                self.expect(";")?;
                self.stat2("stmt:", v)?;
                return Ok(if v == "break" {
                    Stmt::Break
                } else {
                    Stmt::Continue
                });
            }
            "return" => {
                self.next()?;
                self.stat("stmt:return")?;
                if self.scopes.len() > 2 {
                    self.stat("stmt:return-nested")?;
                }
                let ret = self.cur.as_ref().map(|c| c.ret.clone()).unwrap_or(Ty::Void);
                if self.accept(";")? {
                    if ret != Ty::Void {
                        return reject("return-missing-value");
                    }
                    return Ok(Stmt::Return(None));
                }
                let e = self.expr()?;
                self.convert(&e.ty, &ret, "return")?;
                self.expect(";")?;
                return Ok(Stmt::Return(Some(e)));
            }
            _ => {}
        }
        if v == "discard_fragment" && self.peek_at(1)? == "(" {
            self.next()?;
            self.expect("(")?;
            self.expect(")")?;
            self.expect(";")?;
            self.stat("stmt:discard")?;
            return Ok(Stmt::DiscardFragment);
        }
        if self.is_decl()? {
            return Ok(Stmt::Decl(self.decl()?));
        }
        self.stat("stmt:expr")?;
        let e = self.expr()?;
        self.expect(";")?;
        Ok(Stmt::Expr(e))
    }

    fn sub_stmt(&mut self) -> PResult<Stmt> {
        self.frame(|p| {
            if p.peek()? == "{" {
                return Ok(Stmt::Block(p.block()?));
            }
            p.scopes.push(FxHashMap::default());
            let s = p.stmt()?;
            p.scopes.pop();
            Ok(s)
        })
    }

    fn is_decl(&mut self) -> PResult<bool> {
        self.frame(|p| {
            if !p.is_type_start(0)? {
                return Ok(false);
            }
            let v = p.peek()?;
            if ADDR.contains(&v) || v == "const" {
                return Ok(true);
            }
            // `T(...)` is a constructor call, not a declaration.
            Ok(p.peek_at(1)? != "(")
        })
    }

    fn decl(&mut self) -> PResult<Decl> {
        self.frame(|p| {
            let kernel_local = p.scopes.len() == 2
                && p.cur
                    .as_ref()
                    .is_some_and(|sig| sig.entry == Some(Stage::Kernel));
            let (ty, storage) = p.type_body(false, kernel_local)?;
            let threadgroup = storage == Some(Space::Threadgroup);
            let mut vars = Vec::new();
            loop {
                let name = p.ident()?;
                let name = p.intern(name);
                let vty = p.array_suffix(ty.clone())?;
                let mut init = Init::None;
                if p.accept("=")? {
                    if threadgroup {
                        return reject("threadgroup-initializer");
                    }
                    if p.peek()? == "{" {
                        p.next()?;
                        if !p.accept("}")? {
                            return reject("brace-init-nonempty");
                        }
                        p.stat("decl:zero-init")?;
                        init = Init::Zero;
                    } else {
                        let et = p.expr_assign()?;
                        p.convert(&et.ty, &vty, "init")?;
                        p.stat("decl:init")?;
                        p.leaf(2)?;
                        if is_ssa_name(&name) {
                            p.stat("decl:ssa")?;
                        }
                        if et.ty.strip() == vty {
                            p.stat("decl:init-exact-type")?;
                        } else {
                            p.stat("decl:init-converted")?;
                        }
                        init = Init::Expr(et);
                    }
                } else if p.peek()? == "(" {
                    return reject("decl-ctor-syntax");
                } else {
                    p.stat("decl:noinit")?;
                }
                // prototype: tests `d != 'unsafe'`, but `unsafe` sits in `c`, so every array counts.
                if matches!(vty, Ty::Array { .. }) {
                    p.stat("decl:local-array")?;
                }
                if let Some(s) = p.scopes.last_mut() {
                    let scope_ty = if threadgroup {
                        Ty::Ref(Space::Threadgroup, Rc::new(vty.clone()))
                    } else {
                        vty.clone()
                    };
                    s.insert(name.clone(), (scope_ty, true));
                } else {
                    return crash(PyExc::IndexError);
                }
                vars.push(DeclVar {
                    name,
                    ty: vty,
                    init,
                });
                if !p.accept(",")? {
                    break;
                }
            }
            p.expect(";")?;
            Ok(Decl { ty, vars, threadgroup })
        })
    }

    fn cond(&mut self, t: &Ty) -> PResult<()> {
        self.frame(|_| {
            let t = t.strip();
            if !matches!(t, Ty::Scalar(_)) {
                return reject_d("condition-not-scalar", t.repr()?);
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------ conversions

    fn convert(&mut self, frm: &Ty, to: &Ty, why: &str) -> PResult<()> {
        self.frame(|p| {
            let f = frm.strip();
            let to = to.strip();
            if f == to {
                return Ok(());
            }
            match (&f, &to) {
                (Ty::Scalar(_), Ty::Scalar(_)) => {
                    p.stat("conv:scalar-implicit")?;
                    return Ok(());
                }
                (Ty::Scalar(_), Ty::Vec { .. }) => {
                    p.stat("conv:splat-implicit")?;
                    return Ok(());
                }
                // packed <-> unpacked
                (Ty::Vec { base: a, w: wa, .. }, Ty::Vec { base: b, w: wb, .. })
                    if wa == wb && a == b =>
                {
                    return Ok(())
                }
                (
                    Ty::Array {
                        elem: ea, n: na, ..
                    },
                    Ty::Array {
                        elem: eb, n: nb, ..
                    },
                ) if ea == eb && na == nb => return Ok(()),
                (
                    Ty::Tex {
                        kind: ka, el: la, ..
                    },
                    Ty::Tex {
                        kind: kb, el: lb, ..
                    },
                ) if ka == kb && la == lb => return Ok(()),
                _ => {}
            }
            reject_d(
                "implicit-conversion",
                format!("{}: {} -> {}", why, f.repr()?, to.repr()?),
            )
        })
    }

    // ------------------------------------------------------------ expressions

    fn expr(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let t = p.expr_assign()?;
            if p.peek()? == "," {
                return reject("comma-operator");
            }
            Ok(t)
        })
    }

    fn expr_assign(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let start = p.i;
            let t = p.expr_cond()?;
            let op = p.peek()?;
            if !ASSIGN_OPS.contains(&op) {
                return Ok(t);
            }
            p.next()?;
            if !p.lv {
                return reject("assign-to-rvalue");
            }
            let lhs_name = p.lvname.clone();
            let r = p.expr_assign()?;
            let bop = if op == "=" {
                p.convert(&r.ty, &t.ty, "assign")?;
                None
            } else {
                let o = &op[..op.len() - 1];
                p.binary_type(o, &t.ty.strip(), &r.ty.strip())?;
                BinOp::from_op(o)
            };
            let ssa = match &lhs_name {
                Some(n) => {
                    p.leaf(2)?;
                    is_ssa_name(n)
                }
                None => false,
            };
            p.stat(if ssa {
                "assign:ssa-reassign"
            } else {
                "assign:other"
            })?;
            if let Some(n) = &lhs_name {
                if p.assigned.inc(n) {
                    p.leaf(1)?;
                }
            }
            p.lv = false;
            let ty = t.ty.strip();
            bx(Expr {
                kind: ExprKind::Assign {
                    op: bop,
                    lhs: t,
                    rhs: r,
                },
                ty,
                span: p.span_from(start),
            })
        })
    }

    fn expr_cond(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let start = p.i;
            let c = p.expr_bin(0)?;
            if !p.accept("?")? {
                return Ok(c);
            }
            p.cond(&c.ty)?;
            let a = p.expr_assign()?;
            p.expect(":")?;
            let b = p.expr_assign()?;
            let (ta, tb) = (a.ty.strip(), b.ty.strip());
            p.lv = false;
            p.stat("op:?")?;
            let ty = if ta == tb {
                ta
            } else if ta.is_num() && tb.is_num() && ta.width() == tb.width() {
                v(
                    arith(ta.num_base().unwrap(), tb.num_base().unwrap())?,
                    ta.width().unwrap(),
                )
            } else if ta.is_num() && tb.is_num() && (ta.width() == Some(1) || tb.width() == Some(1))
            {
                if ta.width() > Some(1) {
                    ta
                } else {
                    tb
                }
            } else {
                return reject_d("ternary-types", format!("{} {}", ta.repr()?, tb.repr()?));
            };
            bx(Expr {
                kind: ExprKind::Cond {
                    cond: c,
                    then: a,
                    els: b,
                },
                ty,
                span: p.span_from(start),
            })
        })
    }

    /// `expr_bin(0)`: binary operators by precedence.
    ///
    /// The prototype descends through one method frame per precedence level (`expr_bin(lvl)`
    /// calls `expr_bin(lvl + 1)`, and level 10 calls `expr_unary`). This port uses precedence
    /// climbing, which parses the same tree with the same order of checks, and sets the
    /// emulated Python depth to what the prototype would have: every operand's `expr_unary`
    /// runs at the depth of `expr_bin(0)` + 11, and `binary_type` for an operator of level
    /// `l` at that depth + `l` + 1. The prototype's operator tests between levels run at
    /// smaller depths than the operand before them, so they cannot hit the limit first.
    fn expr_bin(&mut self, lvl: usize) -> PResult<Box<Expr>> {
        debug_assert_eq!(lvl, 0);
        self.frame(|p| {
            let d0 = p.depth;
            p.climb(lvl, d0)
        })
    }

    /// Parses operands and operators of level `min` and higher.`d0` is the depth of `expr_bin(0)`.
    fn climb(&mut self, min: usize, d0: u32) -> PResult<Box<Expr>> {
        let start = self.i;
        let saved = self.depth;
        self.depth = d0 + PREC.len() as u32; // inside expr_bin(10)
        let first = self.expr_unary();
        self.depth = saved;
        let mut t = first?;
        loop {
            // `self.peek() in PREC[lvl] and self.kind() == 'punct'`, from the precomputed table
            let Some(&lvl) = self.prec.get(self.i) else {
                return crash(PyExc::IndexError);
            };
            if lvl == NO_LEVEL || (lvl as usize) < min {
                return Ok(t);
            }
            let op = self.next()?;
            let r = self.climb(lvl as usize + 1, d0)?;
            self.depth = d0 + lvl as u32; // binary_type is called from expr_bin(lvl)
            let ty = self.binary_type(op, &t.ty.strip(), &r.ty.strip());
            self.depth = saved;
            let ty = ty?;
            self.lv = false;
            let bop = BinOp::from_op(op).expect("operator from the precedence table");
            t = Box::new(Expr {
                kind: ExprKind::Binary {
                    op: bop,
                    lhs: t,
                    rhs: r,
                },
                ty,
                span: self.span_from(start),
            });
        }
    }

    fn binary_type(&mut self, op: &str, a: &Ty, b: &Ty) -> PResult<Ty> {
        self.frame(|p| {
            p.stat2("op:", op)?;
            if matches!(a, Ty::Ptr(..))
                && b.is_num()
                && b.is_int()?
                && b.width() == Some(1)
                && matches!(op, "+" | "-")
            {
                p.stat("pointer-arith")?;
                return Ok(a.clone());
            }
            if !(a.is_num() && b.is_num()) {
                if matches!(a, Ty::Mat { .. }) || matches!(b, Ty::Mat { .. }) {
                    return reject("matrix-op");
                }
                return reject_d(
                    "binary-nonnumeric",
                    format!("{} {} {}", op, a.repr()?, b.repr()?),
                );
            }
            let (wa, wb) = (a.width().unwrap(), b.width().unwrap());
            let (an, bn) = (a.num_base().unwrap(), b.num_base().unwrap());
            if matches!(op, "&&" | "||") {
                if wa != 1 || wb != 1 {
                    return reject("logical-vector");
                }
                return Ok(BOOL);
            }
            if wa > 1 && wb > 1 && wa != wb {
                return reject("vector-width-mismatch");
            }
            let w = wa.max(wb);
            let mut r: ScalarName = if wa > 1 && wb > 1 {
                if an != bn {
                    return reject_d(
                        "vector-base-mismatch",
                        format!("{} {} {}", op, a.repr()?, b.repr()?),
                    );
                }
                an.clone()
            } else if wa > 1 {
                an.clone()
            } else if wb > 1 {
                bn.clone()
            } else {
                arith(an, bn)?
            };
            if matches!(op, "<<" | ">>" | "&" | "|" | "^" | "%") {
                if r.class()? == Class::F {
                    return reject("bitop-on-float");
                }
                if matches!(op, "<<" | ">>") {
                    // the type of the promoted left operand
                    r = if wa > 1 || an.rank_or(3) >= 3 {
                        an.clone()
                    } else {
                        ScalarName::Known(Scalar::Int)
                    };
                }
                if r.is(Scalar::Bool) && wa == 1 && wb == 1 {
                    r = ScalarName::Known(Scalar::Int);
                }
            }
            if matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=") {
                return Ok(vk(Scalar::Bool, w));
            }
            if r.is(Scalar::Bool) && matches!(op, "+" | "-" | "*" | "/") {
                r = ScalarName::Known(Scalar::Int);
            }
            if w == 1 && r.rank_or(9) < 3 && r.class()? != Class::F {
                r = ScalarName::Known(Scalar::Int);
            }
            Ok(v(r, w))
        })
    }

    fn expr_unary(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let start = p.i;
            let op = p.peek()?;
            if !(p.kind()? == TokKind::Punct
                && matches!(op, "-" | "+" | "!" | "~" | "*" | "&" | "++" | "--"))
            {
                return p.expr_postfix();
            }
            p.next()?;
            let e = p.expr_unary()?;
            let t = if matches!(op, "*" | "&") {
                e.ty.clone()
            } else {
                e.ty.strip()
            };
            p.stat2("unary:", op)?;
            if op == "*" {
                let t = t.strip();
                let Ty::Ptr(space, inner) = t else {
                    return reject_d("deref-nonpointer", t.repr()?);
                };
                p.lv = true;
                p.lvname = None;
                return bx(Expr {
                    kind: ExprKind::Deref(e),
                    ty: Ty::Ref(space, inner),
                    span: p.span_from(start),
                });
            }
            if op == "&" {
                let Ty::Ref(space, inner) = t else {
                    return reject("address-of-rvalue");
                };
                p.stat("address-of")?;
                p.lv = false;
                return bx(Expr {
                    kind: ExprKind::AddrOf(e),
                    ty: Ty::Ptr(space, inner),
                    span: p.span_from(start),
                });
            }
            if matches!(op, "++" | "--") {
                return reject("prefix-incdec");
            }
            p.lv = false;
            let (uop, ty) = if op == "!" {
                if !matches!(t, Ty::Scalar(_)) {
                    return reject("not-vector");
                }
                (UnOp::Not, BOOL)
            } else {
                if !t.is_num() {
                    return reject("unary-nonnumeric");
                }
                if op == "~" && t.is_float()? {
                    return reject("bitnot-float");
                }
                let uop = match op {
                    "-" => UnOp::Neg,
                    "+" => UnOp::Plus,
                    _ => UnOp::BitNot,
                };
                let ty = if t.width() == Some(1) {
                    let k = t.num_base().unwrap().known()?;
                    // `RANK[t.a] < 3` after the float test: a non-float always has a rank.
                    if k.class() != Class::F && k.rank().unwrap_or(9) < 3 {
                        INT
                    } else {
                        t
                    }
                } else {
                    t
                };
                (uop, ty)
            };
            bx(Expr {
                kind: ExprKind::Unary {
                    op: uop,
                    operand: e,
                },
                ty,
                span: p.span_from(start),
            })
        })
    }

    fn expr_postfix(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let start = p.i;
            let mut e = p.primary()?;
            loop {
                let v = p.peek()?;
                if v == "." {
                    p.next()?;
                    let m = p.ident()?;
                    if p.peek()? == "(" {
                        e = p.method(e, m, start)?;
                        p.lv = false;
                        continue;
                    }
                    let t = e.ty.clone();
                    e = p.member(e, t, m, false, start)?;
                    continue;
                }
                if v == "->" {
                    p.next()?;
                    let m = p.ident()?;
                    let pt = e.ty.strip();
                    let (space, inner) = match &pt {
                        Ty::Ptr(space, inner) if matches!(**inner, Ty::Struct(_)) => {
                            (*space, inner.clone())
                        }
                        _ => return reject("arrow-nonpointer"),
                    };
                    p.stat("op:->")?;
                    // The member check sees `Ref(space, struct)`.
                    e = p.member(e, Ty::Ref(space, inner), m, true, start)?;
                    continue;
                }
                if v == "[" {
                    p.next()?;
                    let idx = p.expr()?;
                    let it = idx.ty.strip();
                    p.expect("]")?;
                    if !(matches!(it, Ty::Scalar(_)) && it.is_int()?) {
                        return reject_d("index-non-int", it.repr()?);
                    }
                    // prototype: `lv` is the flag the index expression left behind.
                    let lv = p.lv;
                    let b = &e.ty;
                    let bs = b.strip();
                    let ty = match &bs {
                        Ty::Ptr(space, inner) => {
                            p.lv = true;
                            p.stat("index:pointer")?;
                            Ty::Ref(*space, inner.clone())
                        }
                        Ty::Array { elem, .. } => {
                            let space = if let Ty::Ref(sp, _) = b {
                                *sp
                            } else {
                                Space::Thread
                            };
                            p.lv = lv || b.is_ref();
                            // prototype: the '-unsafe' suffix tests the always-empty field `d`.
                            p.stat("index:array")?;
                            Ty::Ref(space, elem.clone())
                        }
                        Ty::Vec { base, .. } => {
                            let r = if let Ty::Ref(sp, _) = b {
                                Ty::Ref(*sp, Rc::new(Ty::Scalar(base.clone())))
                            } else {
                                Ty::Scalar(base.clone())
                            };
                            p.stat("index:vector-dynamic")?;
                            r
                        }
                        Ty::Mat { .. } => return reject("matrix-index"),
                        other => return reject(format!("index-on:{}", other.kind_name())),
                    };
                    e = Box::new(Expr {
                        kind: ExprKind::Index {
                            base: e,
                            index: idx,
                        },
                        ty,
                        span: p.span_from(start),
                    });
                    continue;
                }
                if matches!(v, "++" | "--") && p.kind()? == TokKind::Punct {
                    return reject("postfix-incdec");
                }
                return Ok(e);
            }
        })
    }

    /// `member(t, m)`: `t` is the type the prototype checks (`Ref(space, S)` for `->`).
    fn member(
        &mut self,
        base: Box<Expr>,
        t: Ty,
        m: &'a str,
        arrow: bool,
        start: usize,
    ) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let t = &t;
            let b = t.strip();
            match &b {
                Ty::Struct(sname) => {
                    let fields = match p.structs.get(sname) {
                        Some(Some(f)) => f.clone(),
                        // prototype: iterating None
                        _ => return crash(PyExc::TypeError),
                    };
                    for (fname, fty) in fields.iter() {
                        if &**fname == m {
                            p.stat("member")?;
                            let space = if let Ty::Ref(sp, _) = t {
                                *sp
                            } else {
                                Space::Thread
                            };
                            let ty = if p.lv || t.is_ref() {
                                Ty::Ref(space, Rc::new(fty.clone()))
                            } else {
                                fty.clone()
                            };
                            let field = fname.clone();
                            return bx(Expr {
                                kind: ExprKind::Member { base, field, arrow },
                                ty,
                                span: p.span_from(start),
                            });
                        }
                    }
                    reject("unknown-member")
                }
                Ty::Vec { base: vb, w, .. } => {
                    p.leaf(2)?;
                    let comps = match swizzle(m) {
                        Some(c) => c,
                        None => return reject("swizzle-form"),
                    };
                    if comps.iter().copied().max().unwrap_or(0) >= *w {
                        return reject("swizzle-out-of-range");
                    }
                    p.stat(["swizzle:1", "swizzle:2", "swizzle:3", "swizzle:4"][comps.len() - 1])?;
                    let r = v(vb.clone(), comps.len() as u8);
                    let distinct = (0..comps.len()).all(|i| !comps[i + 1..].contains(&comps[i]));
                    let ty = match t {
                        Ty::Ref(sp, _) if distinct => Ty::Ref(*sp, Rc::new(r)),
                        _ => {
                            p.lv = false;
                            r
                        }
                    };
                    bx(Expr {
                        kind: ExprKind::Swizzle { base, comps },
                        ty,
                        span: p.span_from(start),
                    })
                }
                Ty::Scalar(_) => reject("swizzle-on-scalar"),
                other => reject(format!("member-on:{}", other.kind_name())),
            }
        })
    }

    fn args(&mut self) -> PResult<Vec<Expr>> {
        self.frame(|p| {
            p.expect("(")?;
            let mut out = Vec::new();
            if p.accept(")")? {
                return Ok(out);
            }
            loop {
                out.push(*p.expr_assign()?);
                if p.accept(")")? {
                    return Ok(out);
                }
                p.expect(",")?;
            }
        })
    }

    fn primary(&mut self) -> PResult<Box<Expr>> {
        self.frame(|p| p.primary_body())
    }

    fn primary_body(&mut self) -> PResult<Box<Expr>> {
        let start = self.i;
        let k = self.kind()?;
        let v = self.peek()?;
        self.lv = false;
        self.lvname = None;
        if k == TokKind::Num {
            self.next()?;
            self.stat("lit")?;
            let ty = self.literal_type(v)?;
            return bx(Expr {
                kind: ExprKind::Literal(v.into()),
                ty,
                span: self.span_from(start),
            });
        }
        if v == "true" || v == "false" {
            self.next()?;
            return bx(Expr {
                kind: ExprKind::Bool(v == "true"),
                ty: BOOL,
                span: self.span_from(start),
            });
        }
        if v == "(" {
            self.next()?;
            let cast = self.is_type_start(0)? && {
                let n = self.peek()?;
                ADDR.contains(&n) || n == "const" || matches!(self.peek_at(1)?, "*" | "&" | ")")
            };
            if cast {
                let to = self.type_()?;
                self.expect(")")?;
                let operand = self.expr_unary()?;
                let f = operand.ty.strip();
                self.stat("c-cast")?;
                let ok = if matches!(to, Ty::Ptr(..)) && matches!(f, Ty::Ptr(..)) {
                    self.stat("c-cast:pointer")?;
                    true
                } else {
                    to.is_num() && f.is_num() && to.width() == f.width()
                };
                if !ok {
                    return reject_d("c-cast", format!("{} -> {}", f.repr()?, to.repr()?));
                }
                self.lv = false;
                return bx(Expr {
                    kind: ExprKind::Cast {
                        kind: CastKind::CStyle,
                        to: to.clone(),
                        operand,
                    },
                    ty: to,
                    span: self.span_from(start),
                });
            }
            let inner = self.expr()?;
            self.expect(")")?;
            let ty = inner.ty.clone();
            return bx(Expr {
                kind: ExprKind::Paren(inner),
                ty,
                span: self.span_from(start),
            });
        }
        if k != TokKind::Id {
            let line = self.toks[self.i].line;
            return reject_d("syntax", format!("primary {} line {}", py_repr(v), line));
        }
        let first = self.next()?;
        let qualified: String;
        let name_str: &str = if self.peek()? == "::" {
            self.next()?;
            let m = self.ident()?;
            if first == "component" {
                self.leaf(1)?;
                let m = self.intern(m);
                return bx(Expr {
                    kind: ExprKind::Component(m.clone()),
                    ty: Ty::Comp(m),
                    span: self.span_from(start),
                });
            }
            match first {
                "precise" | "fast" => {
                    qualified = format!("{first}::{m}");
                    qualified.as_str()
                }
                "metal" => m,
                _ => return reject(format!("qualified:{first}")),
            }
        } else {
            first
        };
        // constructors
        if (vec_name(name_str).is_some() || mat_name(name_str).is_some()) && self.peek()? == "(" {
            if name_str.starts_with("packed_") {
                return reject("packed-ctor");
            }
            return self.ctor(name_str, start);
        }
        if matches!(name_str, "as_type" | "static_cast" | "reinterpret_cast") && self.peek()? == "<"
        {
            self.next()?;
            let to = self.type_()?;
            self.expect(">")?;
            let mut a = self.args()?;
            if a.len() != 1 {
                return reject("cast-args");
            }
            let operand = Box::new(a.pop().unwrap());
            let f = operand.ty.strip();
            let kind = match name_str {
                "as_type" => {
                    let fits = f.is_num()
                        && to.is_num()
                        && f.num_base().unwrap().known()?.size() * f.width().unwrap() as u32
                            == to.num_base().unwrap().known()?.size() * to.width().unwrap() as u32;
                    if !fits {
                        return reject_d("as_type-size", format!("{}->{}", f.repr()?, to.repr()?));
                    }
                    self.stat("call:as_type")?;
                    CastKind::AsType
                }
                "static_cast" => {
                    self.stat("call:static_cast")?;
                    CastKind::Static
                }
                _ => {
                    let from_ok =
                        matches!(f, Ty::Ptr(..)) || f == s(Scalar::Ulong) || f == s(Scalar::Long);
                    let ok = if to == Ty::ThreadgroupAtomicUintPtr {
                        matches!(&f, Ty::Ptr(Space::Threadgroup, inner) if **inner == UINT)
                    } else {
                        matches!(to, Ty::Ptr(..)) && from_ok
                    };
                    if !ok {
                        return reject("reinterpret_cast");
                    }
                    self.stat("call:reinterpret_cast")?;
                    CastKind::Reinterpret
                }
            };
            return bx(Expr {
                kind: ExprKind::Cast {
                    kind,
                    to: to.clone(),
                    operand,
                },
                ty: to,
                span: self.span_from(start),
            });
        }
        if self.peek()? == "(" {
            return self.call(name_str, start);
        }
        let name = self.intern_str(name_str);
        if self.structs.contains_key(&name) && self.peek()? == "{" {
            let fields = self.structs.get(&name).cloned().flatten();
            self.next()?;
            let mut n = 0usize;
            let mut args = Vec::new();
            while !self.accept("}")? {
                let et = self.expr_assign()?;
                let Some(fields) = &fields else {
                    // prototype: len(None)
                    return crash(PyExc::TypeError);
                };
                if n >= fields.len() {
                    return reject("struct-init-too-many");
                }
                let fty = fields[n].1.clone();
                self.convert(&et.ty, &fty, "struct-init")?;
                args.push(*et);
                n += 1;
                if !self.accept(",")? {
                    self.expect("}")?;
                    break;
                }
            }
            self.stat("struct-brace-init")?;
            self.leaf(1)?;
            return bx(Expr {
                kind: ExprKind::StructInit {
                    name: name.clone(),
                    args,
                },
                ty: Ty::Struct(name),
                span: self.span_from(start),
            });
        }
        if option(&name).is_some() {
            return reject("option-not-call");
        }
        let Some((t, lv, scope)) = self.lookup(&name)? else {
            if self.funcs.contains_key(&name) {
                return reject("function-pointer");
            }
            return reject_d("unknown-identifier", name.to_string());
        };
        self.lv = lv;
        self.lvname = Some(name.clone());
        let ty = if lv && !t.is_ref() {
            self.leaf(1)?;
            Ty::Ref(Space::Thread, Rc::new(t))
        } else {
            t
        };
        bx(Expr {
            kind: ExprKind::Var { name, scope },
            ty,
            span: self.span_from(start),
        })
    }

    fn intern_str(&mut self, s: &str) -> Rc<str> {
        match self.names.get(s) {
            Some(r) => r.clone(),
            None => Rc::from(s),
        }
    }

    /// The type of a numeric literal (the prototype's rules, including its `int()` failures).
    fn literal_type(&mut self, v: &str) -> PResult<Ty> {
        let lo = v.to_ascii_lowercase();
        if let Some(hex) = lo.strip_prefix("0x") {
            if lo.contains('u') {
                return Ok(UINT);
            }
            let digits = hex.trim_end_matches(['u', 'l']);
            let value = digits.bytes().fold(0u128, |acc, b| {
                acc.saturating_mul(16)
                    .saturating_add((b as char).to_digit(16).unwrap_or(0) as u128)
            });
            return Ok(if value < (1 << 31) { INT } else { UINT });
        }
        self.leaf(2)?;
        if lo.contains('.') || lo.contains('e') {
            return Ok(if lo.ends_with('h') { HALF } else { FLOAT });
        }
        if lo.ends_with("ul") || lo.ends_with("lu") {
            self.leaf(2)?;
            return Ok(s(Scalar::Ulong));
        }
        if lo.ends_with('u') {
            return Ok(UINT);
        }
        if lo.ends_with('l') {
            self.leaf(2)?;
            return Ok(s(Scalar::Long));
        }
        if py_int(&lo)? < (1 << 31) {
            Ok(INT)
        } else {
            self.leaf(2)?;
            Ok(s(Scalar::Long))
        }
    }

    fn ctor(&mut self, name: &str, start: usize) -> PResult<Box<Expr>> {
        self.frame(|p| {
            if mat_name(name).is_some() {
                return reject("matrix-ctor");
            }
            // The vote object has an explicit conversion to the unsigned 64-bit mask.
            if name == "ulong" && p.peek_at(1)? == "simd_ballot"
                && p.peek_at(2)? == "(" && !p.funcs.contains_key("simd_ballot")
            {
                p.expect("(")?;
                p.expect("simd_ballot")?;
                let args = p.args()?;
                p.expect(")")?;
                if args.len() != 1 || args[0].ty.strip() != BOOL {
                    return reject("simd-ballot-form");
                }
                p.stat("call:simd_ballot")?;
                return bx(Expr {
                    kind: ExprKind::Call { callee: Callee::Builtin("simd_ballot".into()), args },
                    ty: s(Scalar::Ulong),
                    span: p.span_from(start),
                });
            }
            let (_, b, w) = vec_name(name).expect("constructor name is a vector or scalar type");
            let w = w.unwrap_or(1);
            let args = p.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            p.stat(if w == 1 { "ctor:scalar" } else { "ctor:vector" })?;
            let ty = vk(b, w);
            let done = |p: &mut Self, args: Vec<Expr>, ty: Ty| {
                bx(Expr {
                    kind: ExprKind::Construct { args },
                    ty,
                    span: p.span_from(start),
                })
            };
            if w == 1 {
                if a.len() != 1 || !a[0].is_num() || a[0].width() != Some(1) {
                    return reject_d("scalar-ctor-form", repr_list(&a)?);
                }
                return done(p, args, ty);
            }
            if a.is_empty() {
                p.stat("ctor:vector-zero")?;
                return done(p, args, ty);
            }
            let mut tot = 0u32;
            for x in &a {
                if !x.is_num() {
                    return reject("ctor-arg-nonnumeric");
                }
                tot += x.width().unwrap() as u32;
            }
            if a.len() == 1 && a[0].width() == Some(1) {
                p.stat("ctor:splat")?;
                return done(p, args, ty);
            }
            if tot != w as u32 {
                return reject_d(
                    "ctor-component-count",
                    format!("{} {}", name, repr_list(&a)?),
                );
            }
            if a.iter()
                .any(|x| x.width().unwrap() > 1 && !x.num_base().unwrap().is(b))
            {
                p.stat("ctor:vector-convert")?;
            }
            done(p, args, ty)
        })
    }

    fn call(&mut self, name: &str, start: usize) -> PResult<Box<Expr>> {
        self.frame(|p| p.call_body(name, start))
    }

    fn call_body(&mut self, name: &str, start: usize) -> PResult<Box<Expr>> {
        let mk = |p: &Self, callee: Callee, args: Vec<Expr>, ty: Ty| Expr {
            kind: ExprKind::Call { callee, args },
            ty,
            span: p.span_from(start),
        };
        if matches!(name, "atomic_fetch_add_explicit" | "atomic_fetch_min_explicit" | "atomic_fetch_max_explicit")
            && !self.funcs.contains_key(name)
        {
            self.expect("(")?;
            let ptr = self.expr_assign()?;
            self.expect(",")?;
            let val = self.expr_assign()?;
            self.expect(",")?;
            if ptr.ty.strip() != Ty::ThreadgroupAtomicUintPtr {
                self.expr_assign()?;
                self.expect(")")?;
                return reject(format!("call:{name}"));
            }
            self.expect("memory_order_relaxed")?;
            self.expect(")")?;
            if val.ty.strip() != UINT {
                return reject("threadgroup-atomic-form");
            }
            return bx(mk(self, Callee::Builtin(name.into()), vec![*ptr, *val], UINT));
        }
        if name == "threadgroup_barrier" && !self.funcs.contains_key(name) {
            self.expect("(")?;
            self.expect("mem_flags")?;
            self.expect("::")?;
            self.expect("mem_threadgroup")?;
            self.expect(")")?;
            return bx(mk(
                self,
                Callee::Builtin("threadgroup_barrier".into()),
                vec![],
                Ty::Void,
            ));
        }
        if let Some(cands) = self.funcs.get(name).cloned() {
            let args = self.args()?;
            self.stat("call:user-fn")?;
            for f in &cands {
                if f.entry.is_some() {
                    return reject("call-entry-point");
                }
                if f.params.len() == args.len() {
                    let mut all = true;
                    for (x, prm) in args.iter().zip(f.params.iter()) {
                        if !self.passes(&x.ty, &prm.ty)? {
                            all = false;
                            break;
                        }
                    }
                    if all {
                        let ret = f.ret.clone();
                        return bx(mk(self, Callee::User(f.clone()), args, ret));
                    }
                }
            }
            let stripped: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            return reject_d(
                "overload-resolution",
                format!("{} {}", name, repr_list(&stripped)?),
            );
        }
        if name == "dm_bs" && self.prelude.contains("dm_bs") {
            let args = self.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            if a.len() < 6 || !matches!(a[3], Ty::Tex { .. }) || a[4] != Ty::Sampler {
                return reject("dm_bs-form");
            }
            self.stat("call:dm_bs")?;
            // The template's body: r = dm_bmix(r, t.sample(w, a...), b->k[i]) must type-check.
            let mut sargs = vec![a[4].clone()];
            sargs.extend_from_slice(&a[5..]);
            let st = self.sample_type(&a[3], "sample", &sargs)?;
            if a[0] != st && !(a[0] == FLOAT && st == vk(Scalar::Float, 4)) {
                return reject_d(
                    "dm_bs-instantiation",
                    format!("{} {}", a[0].repr()?, st.repr()?),
                );
            }
            if !matches!(a[1], Ty::Ptr(..)) || a[2] != UINT {
                return reject("dm_bs-form");
            }
            let ty = a[0].clone();
            return bx(mk(self, Callee::Prelude("dm_bs"), args, ty));
        }
        if name == "dm_pool_mark" && self.prelude.contains("dm_pool_mark") {
            // uint dm_pool_mark(constant P* p, uint i), P deduced from the pool pointer
            let args = self.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            let pool = matches!(a.first(), Some(Ty::Ptr(Space::Constant, p)) if matches!(**p, Ty::Struct(_)));
            if a.len() != 2 || !pool || a[1] != UINT {
                return reject("dm_pool_mark-form");
            }
            self.stat("call:dm_pool_mark")?;
            return bx(mk(self, Callee::Prelude("dm_pool_mark"), args, UINT));
        }
        if name == "spvDepthCast" && self.prelude.contains("spvDepthCast") {
            let args = self.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            let Some(Ty::Tex { kind, el, access }) = a.first().filter(|_| a.len() == 1) else {
                return reject("spvDepthCast-form");
            };
            self.stat("call:spvDepthCast")?;
            let ty = Ty::Tex {
                kind: kind.replace("texture", "depth").into(),
                el: *el,
                access: access.clone(),
            };
            return bx(mk(self, Callee::Prelude("spvDepthCast"), args, ty));
        }
        if name == "spvFAdd" && self.prelude.contains("spvFAdd") {
            let args = self.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            self.stat("call:spvFAdd")?;
            if a.len() != 2 || a[0] != a[1] {
                return reject("spvFAdd-form");
            }
            let ty = a[0].clone();
            return bx(mk(self, Callee::Prelude("spvFAdd"), args, ty));
        }
        let args = self.args()?;
        let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
        self.stat2("call:", name)?;
        let ty = self.builtin_type(name, &a)?;
        let callee = Callee::Builtin(self.intern_str(name));
        bx(mk(self, callee, args, ty))
    }

    /// The result type of a metal_stdlib builtin or sampling option.
    fn builtin_type(&mut self, name: &str, a: &[Ty]) -> PResult<Ty> {
        if let Some((oname, n)) = option(name) {
            if a.len() != n {
                return reject("option-args");
            }
            return Ok(Ty::Opt(oname));
        }
        let arity = if in_f1(name) || I1.contains(&name) {
            Some(1)
        } else if F2.contains(&name) {
            Some(2)
        } else if F3.contains(&name) {
            Some(3)
        } else {
            None
        };
        if let Some(n) = arity {
            if a.len() != n {
                return reject(format!("builtin-arity:{name}"));
            }
            if !a.iter().all(|x| x.is_num()) {
                return reject(format!("builtin-nonnumeric:{name}"));
            }
            let widths: HashSet<u8> = a.iter().map(|x| x.width().unwrap()).collect();
            let w = *widths.iter().max().unwrap();
            if widths.iter().filter(|&&x| x != 1).count() > 1 {
                return reject(format!("builtin-width-mix:{name}"));
            }
            let has_vec = a.iter().any(|x| x.width().unwrap() > 1);
            if has_vec && a.iter().any(|x| x.width() == Some(1)) {
                self.stat("builtin-scalar-broadcast")?;
            }
            let first = a[0].num_base().unwrap();
            if a.iter().any(|x| x.num_base().unwrap() != first) {
                // metal_stdlib overloads are per type: a mix is ambiguous
                return reject_d(format!("builtin-arg-types-mixed:{name}"), repr_list(a)?);
            }
            let r = first.clone();
            if in_f1(name) && name != "abs" && name != "sign" && r.class()? != Class::F {
                return reject(format!("builtin-int-to-float-fn:{name}"));
            }
            if I1.contains(&name) && r.class()? == Class::F {
                return reject(format!("builtin-float-to-int-fn:{name}"));
            }
            return Ok(v(r, w));
        }
        match name {
            "isnan" | "isinf" | "isfinite" => {
                if a.len() != 1 || !a[0].is_float()? {
                    return reject(format!("builtin-form:{name}"));
                }
                Ok(vk(Scalar::Bool, a[0].width().unwrap()))
            }
            "select" => {
                if a.len() != 3 || !a[0].is_num() || (a[0] != a[1] && a[0].width() != a[1].width())
                {
                    return reject_d("select-form", repr_list(a)?);
                }
                if !a[2].is_bool() || a[2].width() != a[0].width() {
                    return reject_d("select-cond", repr_list(a)?);
                }
                if a[0] != a[1] {
                    return reject_d("select-type-mix", repr_list(a)?);
                }
                Ok(a[0].clone())
            }
            "all" | "any" => {
                if a.len() != 1 || !a[0].is_bool() {
                    return reject("all-any-form");
                }
                Ok(BOOL)
            }
            "dot" | "length" | "distance" => match a.first() {
                Some(t) => Ok(t.a_as_scalar()),
                None => crash(PyExc::IndexError),
            },
            "cross" => match a.first() {
                Some(t) => Ok(t.clone()),
                None => crash(PyExc::IndexError),
            },
            "mulhi" => {
                if a.len() != 2 || !a[0].is_int()? || a[0] != a[1] {
                    return reject("mulhi-form");
                }
                Ok(a[0].clone())
            }
            "simd_shuffle" => {
                // `simd_shuffle(T data, ushort lane)`: 32-bit data, an integer scalar lane
                let data_ok = a.first().and_then(|x| x.num_base()).is_some_and(|b| {
                    b.is(Scalar::Float) || b.is(Scalar::Int) || b.is(Scalar::Uint)
                });
                let lane_ok = match a.get(1) {
                    Some(x @ Ty::Scalar(_)) => x.is_int()?,
                    _ => false,
                };
                if a.len() != 2 || !data_ok || !lane_ok {
                    return reject("simd_shuffle-form");
                }
                Ok(a[0].clone())
            }
            "simd_any" | "simd_all" => {
                if a.len() != 1 || a[0] != BOOL {
                    return reject("simd-vote-form");
                }
                Ok(BOOL)
            }
            "extract_bits" => {
                if a.len() != 3 || !a[0].is_int()? {
                    return reject("extract_bits-form");
                }
                Ok(a[0].clone())
            }
            "insert_bits" => {
                if a.len() != 4 || !a[0].is_int()? || a[0] != a[1] {
                    return reject("insert_bits-form");
                }
                Ok(a[0].clone())
            }
            _ => {
                if let Some(w) = unpack_width(name) {
                    if a.len() != 1 || a[0] != UINT {
                        return reject("unpack-arg");
                    }
                    return Ok(vk(Scalar::Float, w));
                }
                reject(format!("call:{name}"))
            }
        }
    }

    /// `passes(x, p)`: may an argument of type `x` bind to a parameter of type `p`?
    fn passes(&mut self, x: &Ty, p: &Ty) -> PResult<bool> {
        self.frame(|_| {
            let x = x.strip();
            if let Ty::Ref(_, inner) = p {
                return Ok(x == **inner);
            }
            if x == *p {
                return Ok(true);
            }
            Ok(match (&x, p) {
                (Ty::Scalar(_), Ty::Scalar(_)) => true,
                (Ty::Vec { base: a, w: wa, .. }, Ty::Vec { base: b, w: wb, .. }) => {
                    a == b && wa == wb
                }
                _ => false,
            })
        })
    }

    fn method(&mut self, base: Box<Expr>, m: &'a str, start: usize) -> PResult<Box<Expr>> {
        self.frame(|p| {
            let args = p.args()?;
            let a: Vec<Ty> = args.iter().map(|x| x.ty.strip()).collect();
            let t = base.ty.strip();
            if !matches!(t, Ty::Tex { .. }) {
                return reject(format!("method-on:{}", t.kind_name()));
            }
            let ty = p.sample_type(&t, m, &a)?;
            let name = p.intern(m);
            bx(Expr {
                kind: ExprKind::Method { base, name, args },
                ty,
                span: p.span_from(start),
            })
        })
    }

    fn sample_type(&mut self, t: &Ty, m: &str, a: &[Ty]) -> PResult<Ty> {
        self.frame(|p| {
            let Ty::Tex { kind, el, .. } = t else { unreachable!("sample_type on a texture") };
            let Some(dim) = tex_dim(kind) else {
                return crash(PyExc::KeyError);
            };
            let arr = kind.ends_with("_array");
            let depth = kind.starts_with("depth");
            let mut key = std::mem::take(&mut p.keybuf);
            key.clear();
            key.push_str("method:");
            key.push_str(m);
            key.push(':');
            key.push_str(kind);
            let r = p.stat(&key);
            p.keybuf = key;
            r?;
            let mut r = if depth { FLOAT } else { vk(*el, 4) };
            match m {
                "sample" | "sample_compare" | "gather" | "gather_compare" => {
                    if a.first().is_none_or(|x| *x != Ty::Sampler) {
                        return reject("sample-no-sampler");
                    }
                    let Some(coord) = a.get(1) else {
                        return crash(PyExc::IndexError);
                    };
                    let coord_ok = matches!(coord, Ty::Vec { base, w, .. } if base.is(Scalar::Float) && *w == dim);
                    if !coord_ok {
                        return reject_d("sample-coord", coord.repr()?);
                    }
                    let mut rest = &a[2..];
                    if arr {
                        let ok = match rest.first() {
                            Some(x) => matches!(x, Ty::Scalar(_)) && x.is_int()?,
                            None => false,
                        };
                        if !ok {
                            return reject("sample-array-index");
                        }
                        rest = &rest[1..];
                    }
                    if m == "sample_compare" {
                        if !depth {
                            return reject("sample_compare-nondepth");
                        }
                        if rest.first() != Some(&FLOAT) {
                            return reject("sample_compare-ref");
                        }
                        rest = &rest[1..];
                        r = FLOAT;
                    }
                    if m == "gather" && depth {
                        r = vk(Scalar::Float, 4);
                    }
                    for x in rest {
                        if let Ty::Opt(o) = x {
                            if m == "gather" {
                                return reject("gather-option");
                            }
                            if matches!(*o, "gradient2d" | "gradient3d" | "gradientcube") {
                                p.stat("sample-gradient")?;
                            }
                            continue;
                        }
                        if let Ty::Vec { base, .. } = x {
                            if x.is_int()? && base.is(Scalar::Int) {
                                p.stat("sample-offset")?;
                                continue;
                            }
                        }
                        if matches!(x, Ty::Comp(_)) && m == "gather" {
                            continue;
                        }
                        return reject_d(format!("{m}-arg"), x.repr()?);
                    }
                    Ok(r)
                }
                "read" => {
                    if a.is_empty() {
                        return reject("read-args");
                    }
                    Ok(if depth { FLOAT } else { vk(*el, 4) })
                }
                "write" => Ok(Ty::Void),
                "get_width" | "get_height" | "get_depth" | "get_array_size" | "get_num_mip_levels" => Ok(UINT),
                "calculate_clamped_lod" | "calculate_unclamped_lod" => {
                    if a.len() != 2 || a[0] != Ty::Sampler {
                        return reject("lod-form");
                    }
                    Ok(FLOAT)
                }
                _ => reject(format!("method:{m}")),
            }
        })
    }
}

const NO_LEVEL: u8 = u8::MAX;

/// The `PREC` level of a binary operator token.
fn prec_level(t: &Token<'_>) -> u8 {
    if t.kind != TokKind::Punct {
        return NO_LEVEL;
    }
    PREC.iter()
        .position(|ops| ops.contains(&t.text))
        .map_or(NO_LEVEL, |l| l as u8)
}

/// Component indices of a swizzle `[xyzw]{1,4}|[rgba]{1,4}`.
fn swizzle(m: &str) -> Option<Vec<u8>> {
    if m.is_empty() || m.len() > 4 {
        return None;
    }
    let set = if m.bytes().all(|c| b"xyzw".contains(&c)) {
        b"xyzw"
    } else if m.bytes().all(|c| b"rgba".contains(&c)) {
        b"rgba"
    } else {
        return None;
    };
    Some(
        m.bytes()
            .map(|c| set.iter().position(|&s| s == c).unwrap() as u8)
            .collect(),
    )
}
