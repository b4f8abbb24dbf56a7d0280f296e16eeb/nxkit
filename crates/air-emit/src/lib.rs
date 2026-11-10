//! Emitter of AIR text IR (typed-pointer LLVM 14 syntax) from the resolved MSL of `msl-sema`.
//!
//! The output follows the conventions of Apple's front end at `-O0` without LLVM passes
//! (`metal -S -emit-llvm -Xclang -disable-llvm-passes`): every local is an `alloca`, helpers
//! are `internal alwaysinline` functions, entry points use the AIR calling convention with
//! argument metadata, function constants use the `MTL_FC_INIT` globals and a static
//! initialiser. `air-as` assembles it and `metallib` links it into a valid library.`metallib`
//! runs the per-function AIR passes (inlining, predicate resolution, metadata fixup) itself.
//!
//! Correctness, not byte identity with Apple's output, is the goal: the names of values,
//! the instruction order and the optimisation level differ.

#![deny(missing_docs)]

mod atomic;
mod entry;
pub mod func;
pub mod fx;
pub mod intrin;
pub mod ir;
mod mangle;
pub mod meta;
mod tbaa;

use crate::fx::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use msl_sema::hir::Module;
use msl_sema::types::{Scalar, Type, Types};
use msl_sema::Error;

use func::{Addr, FnGen, RetKind, V};
use ir::{escape, llty, space_num, space_suffix};
use meta::{i, s, Md, Meta};

/// The deployment target: triple and the versions in the module metadata.
#[derive(Clone, Debug)]
pub struct Target {
    /// `air64_v29-apple-macosx27.2.0`
    pub triple: String,
    /// `!air.version`
    pub air_version: (u32, u32, u32),
    /// `!air.language_version` (Metal)
    pub language: (u32, u32, u32),
    /// `SDK Version` module flag.
    pub sdk: (u32, u32),
    /// `-fmetal-math-mode=relaxed`: no `nnan ninf` flags, no `no-infs`/`no-nans` function attributes.
    pub relaxed: bool,
    /// Metal before 4: vectors get the `omnipotent char` TBAA node, not their element's.
    pub vec_tbaa_char: bool,
    /// AIR 2.9: texture reads take `air.get_read_sampler()` and a zero offset (AIR 2.7 has neither).
    pub read_sampler: bool,
}

impl Target {
    /// The macOS SDK default target of the installed toolchain (macOS 27.2, AIR 2.9, Metal 4.1).
    pub fn macos() -> Target {
        Target {
            triple: "air64_v29-apple-macosx27.2.0".into(),
            air_version: (2, 9, 0),
            language: (4, 1, 0),
            sdk: (27, 2),
            relaxed: true,
            vec_tbaa_char: false,
            read_sampler: true,
        }
    }

    /// The engine's iOS convention (`-sdk iphoneos -mios-version-min=18.0 -fmetal-math-mode=relaxed`):
    /// AIR 2.7, Metal 3.2.
    pub fn ios() -> Target {
        Target {
            triple: "air64_v27-apple-ios18.0.0".into(),
            air_version: (2, 7, 0),
            language: (3, 2, 0),
            sdk: (27, 2),
            relaxed: true,
            vec_tbaa_char: true,
            read_sampler: false,
        }
    }
}

/// Attribute group of an intrinsic declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attrs {
    /// Pure math and conversions.
    ReadNone,
    /// `discard_fragment`.
    NoUnwindWillReturn,
    /// Sampling with implicit derivatives (convergent).
    SampleConvergent,
    /// Texture reads, gathers and queries.
    ReadOnly,
    /// `air.get_read_sampler`.
    Inaccessible,
    /// `llvm.fmuladd`.
    Speculatable,
    /// Texture writes.
    Write,
    /// Call sites of LLVM intrinsics (`nounwind`).
    NoUnwind,
    /// Derivatives.
    Convergent,
    /// `llvm.lifetime.*`
    Lifetime,
}

impl Attrs {
    pub(crate) fn group(self) -> u32 {
        // #11 is the group of user function call sites (`convergent nobuiltin`).
        let n = 3 + self as u32;
        if n >= USER_CALL_GROUP {
            n + 1
        } else {
            n
        }
    }

    fn text(self) -> &'static str {
        match self {
            Attrs::ReadNone => "nounwind readnone willreturn",
            Attrs::NoUnwindWillReturn => "nounwind willreturn",
            Attrs::SampleConvergent => "argmemonly convergent nounwind readonly willreturn",
            Attrs::ReadOnly => "argmemonly nounwind readonly willreturn",
            Attrs::Inaccessible => "inaccessiblememonly nounwind readonly willreturn",
            Attrs::Write => "argmemonly nounwind willreturn",
            Attrs::NoUnwind => "nounwind",
            Attrs::Convergent => "convergent nounwind willreturn",
            Attrs::Lifetime => "argmemonly nocallback nofree nosync nounwind willreturn",
            Attrs::Speculatable => {
                "nocallback nofree nosync nounwind readnone speculatable willreturn"
            }
        }
    }
}

/// Attribute group of call sites of user functions.
const USER_CALL_GROUP: u32 = 11;

const FN_ATTRS: &str = "\"approx-func-fp-math\"=\"true\" \"frame-pointer\"=\"all\" \"invariance-late-contract\" \"invariance-late-reassoc\" \"invariance-late-unsafe-fp-math\" \"min-legal-vector-width\"=\"0\" \"no-builtins\" \"no-infs-fp-math\"=\"true\" \"no-nans-fp-math\"=\"true\" \"no-signed-zeros-fp-math\"=\"true\" \"no-trapping-math\"=\"true\" \"stack-protector-buffer-size\"=\"8\"";

/// Something clang emits after the top-level declarations (EmitDeferred).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deferred {
    /// A user helper function.
    Func(usize),
    /// A metal_stdlib wrapper, standing for the intrinsic it declares.
    Wrapper(String),
    /// A reserved position that was not needed.
    Pending,
}

/// Module-wide state shared by the function generators.
pub struct ModCx {
    /// Types and layouts.
    pub types: Types,
    decls: BTreeMap<String, (String, Attrs)>,
    /// Metadata nodes.
    pub meta: Meta,
    /// Global name of each constant array.
    pub const_names: Vec<String>,
    /// (shadow global, `MTL_FC_INIT` global) of each function constant.
    pub fconst_names: Vec<(String, String)>,
    /// Type of each function constant.
    pub fconst_tys: Vec<Type>,
    fconst_index: HashMap<String, usize>,
    /// Name and parameter types of each function.
    pub funcs_sig: Vec<(String, Vec<String>)>,
    /// Whether each function is a template instantiation.
    pub funcs_template: Vec<bool>,
    /// Parameter attributes clang puts on each helper parameter (`noundef`, `align`, `dereferenceable`).
    pub param_attrs: Vec<Vec<String>>,
    preds: Vec<(String, usize)>,
    order: Vec<String>,
    order_set: HashSet<String>,
    referenced: Vec<Deferred>,
    fill_slot: Option<usize>,
    /// Nonzero while emitting code clang would have in a template instantiation.
    pub in_template: u32,
    /// Function being emitted (source order of template references).
    cur_fid: usize,
    template_refs: Vec<(usize, usize, Deferred)>,
    vertex: Vec<usize>,
    fragment: Vec<usize>,
    kernel: Vec<usize>,
    /// [`Target::vec_tbaa_char`].
    pub vec_tbaa_char: bool,
    /// [`Target::read_sampler`].
    pub read_sampler: bool,
    tbaa: tbaa::Cache,
}

impl ModCx {
    /// Declares an intrinsic once, returns its name.
    pub fn declare(&mut self, name: &str, sig: &str, attrs: Attrs) -> String {
        if self.in_template > 0 {
            return self.declare_template(name, sig, attrs);
        }
        self.note(name);
        self.decls
            .entry(name.to_string())
            .or_insert_with(|| (sig.to_string(), attrs));
        name.to_string()
    }

    /// Records that a function exists from now on (clang's module order is creation order).
    pub fn note(&mut self, name: &str) {
        if self.order_set.insert(name.to_string()) {
            self.order.push(name.to_string());
        }
    }

    /// Records a call of user function `fid` (deferred emission like clang's).
    pub fn reference(&mut self, fid: usize) {
        if self.funcs_template[fid] || self.in_template > 0 {
            self.push_template(Deferred::Func(fid));
            return;
        }
        let name = self.funcs_sig[fid].0.clone();
        self.note(&name);
        if !self.referenced.contains(&Deferred::Func(fid)) {
            self.referenced.push(Deferred::Func(fid));
        }
    }

    /// Declares an intrinsic that clang reaches through a `linkonce_odr` metal_stdlib wrapper: the
    /// declaration appears when the deferred wrapper is emitted, not at the call.
    pub fn declare_wrapped(&mut self, name: &str, sig: &str, attrs: Attrs) -> String {
        if self.in_template > 0 {
            return self.declare_template(name, sig, attrs);
        }
        self.decls
            .entry(name.to_string())
            .or_insert_with(|| (sig.to_string(), attrs));
        let d = Deferred::Wrapper(name.to_string());
        let seen = self.referenced.contains(&d);
        match self.fill_slot.take() {
            Some(k) if !seen && self.referenced[k] == Deferred::Pending => self.referenced[k] = d,
            _ if !seen => self.referenced.push(d),
            _ => {}
        }
        name.to_string()
    }

    /// Declares an intrinsic reached through a template (texture methods, `dm_bs`): clang instantiates
    /// templates at the end of the translation unit, so their bodies join the deferred list last.
    pub fn declare_template(&mut self, name: &str, sig: &str, attrs: Attrs) -> String {
        self.decls
            .entry(name.to_string())
            .or_insert_with(|| (sig.to_string(), attrs));
        self.push_template(Deferred::Wrapper(name.to_string()));
        name.to_string()
    }

    fn push_template(&mut self, d: Deferred) {
        if !self.template_refs.iter().any(|(_, _, x)| *x == d) {
            let seq = self.template_refs.len();
            self.template_refs.push((self.cur_fid, seq, d));
        }
    }

    /// Reserves the deferred-list position of a wrapper call before its arguments are emitted
    /// (clang creates the callee first).
    pub fn reserve(&mut self) -> usize {
        self.referenced.push(Deferred::Pending);
        self.referenced.len() - 1
    }

    /// The next wrapped declaration goes to reserved position `k`.
    pub fn fill(&mut self, k: usize) {
        self.fill_slot = Some(k);
    }

    /// The function constant of a name.
    pub fn fconst_by_name(&self, name: &str) -> Option<usize> {
        self.fconst_index.get(name).copied()
    }

    /// A new `__metal_implicit_fc_pred_N` global for a function constant gate.
    pub fn new_pred(&mut self, fc: usize) -> String {
        self.note("air.normalize_function_constant_predicate.i8");
        let n = format!("@__metal_implicit_fc_pred_{}", self.preds.len());
        self.preds.push((n.clone(), fc));
        n
    }
}

fn fc_code(s: Scalar) -> &'static str {
    match s {
        Scalar::Bool => "b",
        Scalar::Char => "c",
        Scalar::Uchar => "h",
        Scalar::Short => "s",
        Scalar::Ushort => "t",
        Scalar::Int => "i",
        Scalar::Uint => "j",
        Scalar::Long => "l",
        Scalar::Ulong | Scalar::SizeT => "m",
        Scalar::Half => "Dh",
        Scalar::Float => "f",
    }
}

/// Emits a whole module as AIR text IR. `source_name` goes to `source_filename`.
pub fn emit(m: &Module, t: &Target, source_name: &str) -> Result<String, Error> {
    let mut cx = ModCx {
        types: m.types.clone(),
        decls: BTreeMap::new(),
        meta: Meta::default(),
        const_names: m
            .consts
            .iter()
            .map(|c| format!("@_ZL{}{}", c.name.len(), c.name))
            .collect(),
        fconst_names: m
            .fconsts
            .iter()
            .map(|f| {
                let code = fc_code(f.ty.elem().unwrap_or(Scalar::Int));
                (
                    format!("@_ZL{}{}", f.name.len(), f.name),
                    format!(
                        "@_Z{}{}.MTL_FC_INIT_{}_{}",
                        f.name.len(),
                        f.name,
                        f.index,
                        code
                    ),
                )
            })
            .collect(),
        fconst_tys: m.fconsts.iter().map(|f| f.ty.clone()).collect(),
        fconst_index: m
            .fconsts
            .iter()
            .enumerate()
            .map(|(k, f)| (f.name.to_string(), k))
            .collect(),
        funcs_sig: Vec::new(),
        param_attrs: Vec::new(),
        funcs_template: m.funcs.iter().map(|f| f.template).collect(),
        preds: Vec::new(),
        order: Vec::new(),
        order_set: HashSet::default(),
        referenced: Vec::new(),
        fill_slot: None,
        in_template: 0,
        cur_fid: 0,
        template_refs: Vec::new(),
        vertex: Vec::new(),
        fragment: Vec::new(),
        kernel: Vec::new(),
        vec_tbaa_char: t.vec_tbaa_char,
        read_sampler: t.read_sampler,
        tbaa: tbaa::Cache::default(),
    };
    // constant arrays, as clang emits them: a long run of trailing zeros becomes a packed struct
    // { [nonzero x T], [rest x T] zeroinitializer } that accesses reach through a bitcast
    let mut const_globals = String::new();
    for (k, c) in m.consts.iter().enumerate() {
        let Type::Array(el, n) = &c.ty else {
            return Err(Error::invalid("constant array type"));
        };
        let et = llty(&cx.types, el, true);
        let mut vals = Vec::with_capacity(c.init.len());
        for e in &c.init {
            vals.push(
                FnGen::fold(e)
                    .ok_or_else(|| Error::unsupported("const-array:non-constant-init"))?,
            );
        }
        let is_zero = |v: &ir::CVal| match v {
            ir::CVal::Zero => true,
            ir::CVal::Int(i) => *i == 0,
            ir::CVal::Float(f) => f.to_bits() == 0,
            ir::CVal::Vec(items) => items.iter().all(|x| {
                matches!(x, ir::CVal::Int(0) | ir::CVal::Zero)
                    || matches!(x, ir::CVal::Float(f) if f.to_bits() == 0)
            }),
        };
        let mut nonzero = vals.len();
        while nonzero > 0 && is_zero(&vals[nonzero - 1]) {
            nonzero -= 1;
        }
        let item = |v: &ir::CVal| format!("{et} {}", ir::const_text(&cx.types, v, el, true));
        let at = llty(&cx.types, &c.ty, true);
        let al = cx.types.layout(&c.ty).align;
        let name = cx.const_names[k].clone();
        let trailing = *n as usize - nonzero;
        if nonzero == 0 {
            let _ = writeln!(
                const_globals,
                "{name} = internal addrspace(2) constant {at} zeroinitializer, align {al}"
            );
        } else if trailing >= 8 {
            let st = format!("<{{ [{nonzero} x {et}], [{trailing} x {et}] }}>");
            let items: Vec<String> = vals[..nonzero].iter().map(item).collect();
            let _ = writeln!(
                const_globals,
                "{name} = internal addrspace(2) constant {st} <{{ [{nonzero} x {et}] [{}], [{trailing} x {et}] zeroinitializer }}>, align {al}",
                items.join(", ")
            );
            cx.const_names[k] =
                format!("bitcast ({st} addrspace(2)* {name} to {at} addrspace(2)*)");
        } else {
            let items: Vec<String> = vals.iter().map(item).collect();
            let _ = writeln!(
                const_globals,
                "{name} = internal addrspace(2) constant {at} [{}], align {al}",
                items.join(", ")
            );
        }
    }
    for (fid, f) in m.funcs.iter().enumerate() {
        let name = if f.entry.is_some() {
            f.name.to_string()
        } else {
            mangle::template_name(f, &cx.types)
                .or_else(|| mangle::helper_name(f, &cx.types))
                .filter(|n| !cx.funcs_sig.iter().any(|(m, _)| m == n))
                .unwrap_or_else(|| format!("{}.{fid}", f.name))
        };
        let ptys = f
            .params
            .iter()
            .map(|p| match p.by_ref {
                Some(sp) => format!(
                    "{}{}*",
                    llty(&cx.types, &p.ty, true),
                    space_suffix(space_num(sp))
                ),
                None => llty(&cx.types, &p.ty, false),
            })
            .collect();
        cx.funcs_sig.push((name, ptys));
        let attrs: Vec<String> = f
            .params
            .iter()
            .map(|p| match p.by_ref {
                Some(_) => {
                    let l = cx.types.layout(&p.ty);
                    format!(" noundef align {} dereferenceable({})", l.align, l.size)
                }
                None if matches!(p.ty, Type::Struct(_)) => String::new(),
                None => " noundef".to_string(),
            })
            .collect();
        cx.param_attrs.push(attrs);
    }

    // Only functions reachable from an entry point are emitted (clang drops unused statics).
    let mut used = vec![false; m.funcs.len()];
    let mut stack: Vec<usize> = (0..m.funcs.len())
        .filter(|k| m.funcs[*k].entry.is_some())
        .collect();
    while let Some(k) = stack.pop() {
        if used[k] {
            continue;
        }
        used[k] = true;
        stack.extend(calls_of(&m.funcs[k]));
    }
    // clang's order: entry points in source order, then helpers depth-first in order of first call
    let mut texts: HashMap<String, String> = HashMap::default();
    let mut done = vec![false; m.funcs.len()];
    for fid in (0..m.funcs.len()).filter(|k| m.funcs[*k].entry.is_some()) {
        emit_one(&mut cx, m, fid, &used, &mut done, &mut texts, false)?;
    }
    // template instantiations join the deferred list after the top-level references, in source order
    let mut k = 0;
    let mut taken = 0;
    loop {
        let mut fresh: Vec<(usize, usize, Deferred)> = cx.template_refs[taken..].to_vec();
        taken = cx.template_refs.len();
        fresh.sort_by_key(|(f, q, _)| (*f, *q));
        for (_, _, d) in fresh {
            if !cx.referenced.contains(&d) {
                cx.referenced.push(d);
            }
        }
        if k >= cx.referenced.len() {
            break;
        }
        while k < cx.referenced.len() {
            let d = cx.referenced[k].clone();
            emit_deferred(&mut cx, m, &d, &used, &mut done, &mut texts)?;
            k += 1;
        }
    }
    let mut defs = String::new();
    // globals
    let mut globals = String::new();
    globals.push_str(&const_globals);
    let mut fc_md = Vec::new();
    for (k, f) in m.fconsts.iter().enumerate() {
        let (shadow, init) = cx.fconst_names[k].clone();
        let mt = llty(&cx.types, &f.ty, true);
        let al = cx.types.layout(&f.ty).align;
        let _ = writeln!(
            globals,
            "{shadow} = internal addrspace(2) global {mt} undef, align {al}"
        );
        let _ = writeln!(
            globals,
            "{init} = linkonce_odr hidden addrspace(2) externally_initialized constant {mt} undef, section \"air.fc_initializer\", align {al}"
        );
        let tn = cx.types.msl_name(&f.ty);
        fc_md.push(cx.meta.node(&[
            Md::Raw(format!("{mt} addrspace(2)* {init}")),
            s(&tn),
            s(&f.name),
            i(f.index),
        ]));
    }
    for (p, _) in &cx.preds {
        let _ = writeln!(globals, "{p} = internal addrspace(2) global i8 0, align 1");
    }
    let base = source_name.rsplit('/').next().unwrap_or(source_name);
    let ctor_name = format!(
        "_GLOBAL__sub_I_{}",
        base.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            })
            .collect::<String>()
    );
    if !m.fconsts.is_empty() {
        let _ = writeln!(
            globals,
            "@llvm.global_ctors = appending global [1 x {{ i32, void ()*, i8* }}] [{{ i32, void ()*, i8* }} {{ i32 65535, void ()* @{ctor_name}, i8* null }}]"
        );
        // clang: one `__cxx_global_var_init[.N]` per constant, then one per predicate, called in order
        let mut ctor = String::new();
        let mut inits: Vec<String> = Vec::new();
        let next_name = |inits: &mut Vec<String>| {
            let n = if inits.is_empty() {
                "__cxx_global_var_init".to_string()
            } else {
                format!("__cxx_global_var_init.{}", inits.len())
            };
            inits.push(n.clone());
            n
        };
        for (k, f) in m.fconsts.iter().enumerate() {
            let (shadow, init) = cx.fconst_names[k].clone();
            let mt = llty(&cx.types, &f.ty, true);
            let al = cx.types.layout(&f.ty).align;
            let sc = f.ty.elem().unwrap_or(Scalar::Int);
            let tn = tbaa::scalar(&mut cx, sc);
            let tag = tbaa::tag(&mut cx, tn, tn, 0);
            let name = next_name(&mut inits);
            let _ = writeln!(
                ctor,
                "define internal void @{name}() #2 section \"air.static_init\" {{"
            );
            if f.ty == Type::BOOL {
                let range = cx
                    .meta
                    .node(&[Md::Raw("i8 0".into()), Md::Raw("i8 2".into())]);
                let _ = writeln!(ctor, "  %1 = load i8, i8 addrspace(2)* {init}, align 1, !tbaa !{tag}, !range !{range}");
                let _ = writeln!(ctor, "  %2 = trunc i8 %1 to i1\n  %3 = zext i1 %2 to i8");
                let _ = writeln!(
                    ctor,
                    "  store i8 %3, i8 addrspace(2)* {shadow}, align 1, !tbaa !{tag}"
                );
            } else {
                let _ = writeln!(
                    ctor,
                    "  %1 = load {mt}, {mt} addrspace(2)* {init}, align {al}, !tbaa !{tag}"
                );
                let _ = writeln!(
                    ctor,
                    "  store {mt} %1, {mt} addrspace(2)* {shadow}, align {al}, !tbaa !{tag}"
                );
            }
            ctor.push_str("  ret void\n}\n\n");
        }
        if !cx.preds.is_empty() {
            cx.declare(
                "air.normalize_function_constant_predicate.i8",
                "i8 @air.normalize_function_constant_predicate.i8(i8)",
                Attrs::ReadNone,
            );
        }
        let preds = cx.preds.clone();
        for (p, fc) in &preds {
            let (shadow, _) = cx.fconst_names[*fc].clone();
            let tn = tbaa::scalar(&mut cx, Scalar::Bool);
            let tag = tbaa::tag(&mut cx, tn, tn, 0);
            let range = cx
                .meta
                .node(&[Md::Raw("i8 0".into()), Md::Raw("i8 2".into())]);
            let name = next_name(&mut inits);
            let _ = writeln!(
                ctor,
                "define internal void @{name}() #2 section \"air.static_init\" {{"
            );
            let _ = writeln!(
                ctor,
                "  %1 = load i8, i8 addrspace(2)* {shadow}, align 1, !tbaa !{tag}, !range !{range}"
            );
            let _ = writeln!(ctor, "  %2 = trunc i8 %1 to i1\n  %3 = zext i1 %2 to i8");
            let _ = writeln!(
                ctor,
                "  %4 = call i8 @air.normalize_function_constant_predicate.i8(i8 %3) #{}",
                Attrs::ReadNone.group()
            );
            let _ = writeln!(ctor, "  store i8 %4, i8 addrspace(2)* {p}, align 1");
            ctor.push_str("  ret void\n}\n\n");
        }
        let _ = writeln!(
            ctor,
            "define internal void @{ctor_name}() #2 section \"air.static_init\" {{"
        );
        for n in &inits {
            let _ = writeln!(ctor, "  call void @{n}()");
        }
        ctor.push_str("  ret void\n}\n\n");
        defs.push_str(&ctor);
    }
    if !cx.preds.is_empty() {
        let items: Vec<String> = cx
            .preds
            .iter()
            .map(|(p, _)| format!("i8* addrspacecast (i8 addrspace(2)* {p} to i8*)"))
            .collect();
        let _ = writeln!(
            globals,
            "@llvm.compiler.used = appending global [{} x i8*] [{}], section \"llvm.metadata\"",
            items.len(),
            items.join(", ")
        );
    }

    // definitions and declarations in creation order, the static initialiser last
    let ctor_text = std::mem::take(&mut defs);
    let decls = String::new();
    let mut groups: BTreeSet<Attrs> = BTreeSet::new();
    for name in &cx.order {
        if let Some(t) = texts.get(name) {
            defs.push_str(t);
        } else if let Some((sig, a)) = cx.decls.get(name) {
            let _ = writeln!(defs, "declare {sig} #{}", a.group());
            groups.insert(*a);
        }
    }
    defs.push_str(&ctor_text);
    let (defs, fn_groups) = vector_widths(&defs);

    // types
    let mut types = String::new();
    for st in &cx.types.structs {
        let mut fields: Vec<String> = st
            .fields
            .iter()
            .map(|f| llty(&cx.types, &f.ty, true))
            .collect();
        // clang spells tail padding explicitly
        let end = st
            .fields
            .last()
            .map(|f| f.offset + cx.types.layout(&f.ty).size)
            .unwrap_or(0);
        if end < st.layout.size && !st.fields.is_empty() {
            fields.push(format!("[{} x i8]", st.layout.size - end));
        }
        let _ = writeln!(
            types,
            "%struct.{} = type {{ {} }}",
            st.name,
            fields.join(", ")
        );
    }
    // metal::texture*/sampler wrapper structs that appear in memory types
    let mut wrappers = BTreeSet::new();
    for text in [&types, &defs] {
        let mut rest = text.as_str();
        while let Some(k) = rest.find("%\"struct.metal::") {
            let tail = &rest[k + 16..];
            let end = tail.find('"').unwrap_or(0);
            wrappers.insert(tail[..end].to_string());
            rest = &tail[end..];
        }
    }
    for w in &wrappers {
        let h = if w == "sampler" {
            "%struct._sampler_t addrspace(2)*".to_string()
        } else {
            let kind = msl_sema::types::TexKind::from_name(w)
                .map(|k| k.air_name())
                .unwrap_or("texture_2d");
            format!("%struct._{kind}_t addrspace(1)*")
        };
        let _ = writeln!(types, "%\"struct.metal::{w}\" = type {{ {h} }}");
    }
    let mut opaque = BTreeSet::new();
    for text in [&types, &defs, &decls, &globals] {
        let mut rest = text.as_str();
        while let Some(k) = rest.find("%struct._") {
            let tail = &rest[k + 8..];
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            let name = &tail[..end];
            if name.ends_with("_t")
                && (name.starts_with("_texture_")
                    || name.starts_with("_depth_")
                    || name == "_sampler_t")
            {
                opaque.insert(name.to_string());
            }
            rest = &tail[end..];
        }
    }
    for o in &opaque {
        let _ = writeln!(types, "%struct.{o} = type opaque");
    }

    // module metadata
    let flags = [
        cx.meta.node(&[
            i(2),
            s("SDK Version"),
            Md::Raw(format!("[2 x i32] [i32 {}, i32 {}]", t.sdk.0, t.sdk.1)),
        ]),
        cx.meta.node(&[i(1), s("wchar_size"), i(4)]),
        cx.meta.node(&[i(7), s("frame-pointer"), i(2)]),
        cx.meta.node(&[i(7), s("air.max_device_buffers"), i(31)]),
        cx.meta.node(&[i(7), s("air.max_constant_buffers"), i(31)]),
        cx.meta
            .node(&[i(7), s("air.max_threadgroup_buffers"), i(31)]),
        cx.meta.node(&[i(7), s("air.max_textures"), i(128)]),
        cx.meta
            .node(&[i(7), s("air.max_read_write_textures"), i(8)]),
        cx.meta.node(&[i(7), s("air.max_samplers"), i(16)]),
    ];
    let opts = [
        cx.meta.node(&[s("air.compile.denorms_disable")]),
        cx.meta.node(&[s("air.compile.fast_math_disable")]),
        cx.meta.node(&[s("air.compile.framebuffer_fetch_enable")]),
    ];
    let ident = cx
        .meta
        .node(&[s("Apple metal version 32023.922 (metalfe-32023.922.4)")]);
    let ver = cx
        .meta
        .node(&[i(t.air_version.0), i(t.air_version.1), i(t.air_version.2)]);
    let lang = cx.meta.node(&[
        s("Metal"),
        i(t.language.0),
        i(t.language.1),
        i(t.language.2),
    ]);
    let list = |v: &[usize]| {
        v.iter()
            .map(|n| format!("!{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut out = String::new();
    let _ = writeln!(out, "; ModuleID = '{}'", escape(source_name));
    let _ = writeln!(out, "source_filename = \"{}\"", escape(source_name));
    out.push_str("target datalayout = \"e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32\"\n");
    let _ = writeln!(out, "target triple = \"{}\"\n", t.triple);
    out.push_str(&types);
    out.push('\n');
    out.push_str(&globals);
    out.push('\n');
    out.push_str(&defs);
    out.push_str(&decls);
    out.push('\n');
    for (base, w) in &fn_groups {
        let head = match base {
            0 => "convergent mustprogress nounwind",
            1 => "alwaysinline convergent mustprogress nounwind",
            2 => "convergent nounwind",
            _ => "convergent inlinehint mustprogress nounwind",
        };
        let mut attrs = FN_ATTRS.replace(
            "\"min-legal-vector-width\"=\"0\"",
            &format!("\"min-legal-vector-width\"=\"{}\"", w & 0xffff),
        );
        let head = if w & 0x20000 != 0 {
            head.replace(" mustprogress", "")
        } else {
            head.to_string()
        };
        if w & 0x10000 != 0 {
            attrs = attrs
                .replace(
                    "\"no-infs-fp-math\"=\"true\"",
                    "\"no-infs-fp-math\"=\"false\"",
                )
                .replace(
                    "\"no-nans-fp-math\"=\"true\"",
                    "\"no-nans-fp-math\"=\"false\"",
                );
        }
        let _ = writeln!(
            out,
            "attributes #{} = {{ {head} {attrs} }}",
            base * 1000 + w
        );
    }
    let _ = writeln!(
        out,
        "attributes #{USER_CALL_GROUP} = {{ convergent nobuiltin \"no-builtins\" }}"
    );
    for g in &groups {
        let _ = writeln!(out, "attributes #{} = {{ {} }}", g.group(), g.text());
    }
    out.push('\n');
    let _ = writeln!(out, "!llvm.module.flags = !{{{}}}", list(&flags));
    if !cx.vertex.is_empty() {
        let _ = writeln!(out, "!air.vertex = !{{{}}}", list(&cx.vertex));
    }
    if !cx.fragment.is_empty() {
        let _ = writeln!(out, "!air.fragment = !{{{}}}", list(&cx.fragment));
    }
    if !cx.kernel.is_empty() {
        let _ = writeln!(out, "!air.kernel = !{{{}}}", list(&cx.kernel));
    }
    let _ = writeln!(out, "!air.compile_options = !{{{}}}", list(&opts));
    if !fc_md.is_empty() {
        let _ = writeln!(out, "!air.function_constants = !{{{}}}", list(&fc_md));
    }
    let _ = writeln!(out, "!llvm.ident = !{{!{ident}}}");
    let _ = writeln!(out, "!air.version = !{{!{ver}}}");
    let _ = writeln!(out, "!air.language_version = !{{!{lang}}}");
    if source_name != "-" {
        let src = cx.meta.node(&[s(source_name)]);
        let _ = writeln!(out, "!air.source_file_name = !{{!{src}}}");
    }
    out.push('\n');
    cx.meta.render(&mut out);
    if t.relaxed {
        out = relax(&out);
    }
    Ok(out)
}

/// Relaxed math: fast-math flags without `nnan ninf`, function attributes without
/// `no-infs-fp-math`/`no-nans-fp-math` (clang leaves them out instead of writing them false).
fn relax(ir: &str) -> String {
    let mut r = ir.replace(" nnan ninf nsz arcp afn ", " nsz arcp afn ");
    for a in ["no-infs-fp-math", "no-nans-fp-math"] {
        for v in ["true", "false"] {
            r = r.replace(&format!(" \"{a}\"=\"{v}\""), "");
        }
    }
    r
}

/// Emits a function once, with `deep`, the helpers it calls follow right after (clang's EmitDeferred).
fn emit_one(
    cx: &mut ModCx,
    m: &Module,
    fid: usize,
    used: &[bool],
    done: &mut [bool],
    texts: &mut HashMap<String, String>,
    deep: bool,
) -> Result<(), Error> {
    if done[fid] || !used[fid] {
        return Ok(());
    }
    done[fid] = true;
    cx.cur_fid = fid;
    let name = cx.funcs_sig[fid].0.clone();
    cx.note(&name);
    let before = cx.referenced.len();
    let text = emit_function(cx, &m.funcs[fid], fid)?;
    texts.insert(name, text);
    if deep {
        let newly: Vec<Deferred> = cx.referenced[before..].to_vec();
        for d in newly {
            emit_deferred(cx, m, &d, used, done, texts)?;
        }
    }
    Ok(())
}

fn emit_deferred(
    cx: &mut ModCx,
    m: &Module,
    d: &Deferred,
    used: &[bool],
    done: &mut [bool],
    texts: &mut HashMap<String, String>,
) -> Result<(), Error> {
    match d {
        Deferred::Func(fid) => emit_one(cx, m, *fid, used, done, texts, true),
        Deferred::Wrapper(name) => {
            cx.note(name);
            Ok(())
        }
        Deferred::Pending => Ok(()),
    }
}

/// Emits one function definition.
fn emit_function(cx: &mut ModCx, f: &msl_sema::hir::Func, fid: usize) -> Result<String, Error> {
    let mut defs = String::new();
    {
        if f.entry.is_some() {
            defs.push_str(&entry::emit_entry(cx, f)?);
            defs.push('\n');
            return Ok(defs);
        }
        let (name, ptys) = cx.funcs_sig[fid].clone();
        let rt = llty(&cx.types, &f.ret, false);
        let pattrs = cx.param_attrs[fid].clone();
        let mut params: Vec<String> = Vec::new();
        for (k, (t, p)) in ptys.iter().zip(f.params.iter()).enumerate() {
            match (&p.ty, p.by_ref) {
                (Type::Struct(sid), None) => {
                    // clang's AIR ABI passes a struct by value as its expanded fields
                    let fields = cx.types.structs[*sid].fields.clone();
                    for (j, fd) in fields.iter().enumerate() {
                        params.push(format!(
                            "{} %a{k}_{j}",
                            llty(&cx.types, &fd.ty.unpacked(), true)
                        ));
                    }
                }
                _ => params.push(format!("{t}{} %a{k}", pattrs[k])),
            }
        }
        let group = match f.inline {
            msl_sema::hir::Inline::Always => 1,
            msl_sema::hir::Inline::Hint => 10,
            msl_sema::hir::Inline::None => 0,
        };
        let rattr = if f.ret == Type::BOOL { "zeroext " } else { "" };
        let _ = writeln!(
            defs,
            "define internal {rattr}{rt} @{name}({}) #{group} {{",
            params.join(", ")
        );
        let mut g = FnGen::new(cx, f);
        g.set_ret(RetKind::Plain);
        for (k, p) in f.params.iter().enumerate() {
            let local = g.locals[p.local].clone();
            let lty = f.locals[p.local].ty.clone();
            if let (Type::Struct(sid), None) = (&lty, p.by_ref) {
                let fields = g.cx.types.structs[*sid].fields.clone();
                let st = llty(&g.cx.types, &lty, true);
                for (j, fd) in fields.iter().enumerate() {
                    let r = g.tmp();
                    g.emit(format!(
                        "{r} = getelementptr inbounds {st}, {st}* {local}, i32 0, i32 {j}"
                    ));
                    let ft = llty(&g.cx.types, &fd.ty.unpacked(), true);
                    let al = g.cx.types.layout(&fd.ty).align;
                    g.emit(format!("store {ft} %a{k}_{j}, {ft}* {r}, align {al}"));
                }
                continue;
            }
            let v = V {
                r: format!("%a{k}"),
                ty: lty.unpacked(),
            };
            g.store_to(
                &Addr {
                    ptr: local,
                    ty: lty,
                    space: 0,
                },
                &v,
            );
        }
        g.stmts(&f.body)?;
        g.body_done()?;
        defs.push_str(&g.finish());
        defs.push_str("}\n\n");
    }

    Ok(defs)
}

/// Rewrites each definition's attribute group `#N` to `#N*1000+W`, W = the widest vector (bits) in its
/// signature and call sites, returns the text and the (N, W) pairs used.
fn vector_widths(defs: &str) -> (String, BTreeSet<(u32, u32)>) {
    fn width(line: &str) -> u32 {
        let mut best = 0;
        let b = line.as_bytes();
        let mut k = 0;
        while let Some(p) = line[k..].find('<') {
            let st = k + p + 1;
            k = st;
            let digits: String = line[st..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if digits.is_empty() || !line[st + digits.len()..].starts_with(" x ") {
                continue;
            }
            let rest = &line[st + digits.len() + 3..];
            let bits = if rest.starts_with("float") {
                32
            } else if rest.starts_with("half") {
                16
            } else if let Some(r) = rest.strip_prefix('i') {
                r.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            } else {
                0
            };
            let _ = b;
            best = best.max(digits.parse::<u32>().unwrap_or(0) * bits);
        }
        best
    }
    let mut out = String::with_capacity(defs.len() + 64);
    let mut groups = BTreeSet::new();
    let mut chunk: Vec<&str> = Vec::new();
    let mut flush = |chunk: &mut Vec<&str>, out: &mut String| {
        if chunk.is_empty() {
            return;
        }
        let head = chunk[0];
        let mut w = width(head);
        let mut classify = false;
        let mut nomp = false;
        for l in chunk.iter().skip(1) {
            if l.contains(" call ") || l.starts_with("  call ") {
                w = w.max(width(l));
            }
            classify |= l.contains("; air-emit: fp-classify");
            nomp |= l.contains("; air-emit: no-mustprogress");
        }
        // bit 0x10000: the function lost no-nans/no-infs through an inlined metal::isnan
        let w = if classify { w | 0x10000 } else { w };
        let w = if nomp { w | 0x20000 } else { w };
        let mut first = head.to_string();
        if let Some(k) = head.rfind(" #") {
            let tail = &head[k + 2..];
            let n: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(base) = n.parse::<u32>() {
                first = format!("{} #{}{}", &head[..k], base * 1000 + w, &tail[n.len()..]);
                groups.insert((base, w));
            }
        }
        out.push_str(&first);
        out.push('\n');
        for l in chunk.iter().skip(1) {
            out.push_str(l);
            out.push('\n');
        }
        chunk.clear();
    };
    for line in defs.lines() {
        if line.starts_with("define ") {
            flush(&mut chunk, &mut out);
        }
        if chunk.is_empty() && !line.starts_with("define ") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        chunk.push(line);
        if line == "}" {
            flush(&mut chunk, &mut out);
        }
    }
    flush(&mut chunk, &mut out);
    (out, groups)
}

/// User functions a function calls.
fn calls_of(f: &msl_sema::hir::Func) -> Vec<usize> {
    let mut out = Vec::new();
    msl_sema::hir::visit_calls(&f.body, &mut |id| out.push(id));
    out
}

/// Parses, analyses and emits MSL source text.
pub fn compile_source(src: &str, t: &Target, source_name: &str) -> Result<String, Error> {
    let out = msl_parse::check_source(src);
    let checked = match out {
        msl_parse::Outcome::Checked(c) => c,
        o => {
            let key = o.reasons().first().map(|r| r.0.clone()).unwrap_or_default();
            return Err(Error::unsupported(format!("parse:{key}")));
        }
    };
    if let Some((k, _)) = checked.reasons.iter().next() {
        return Err(Error::unsupported(format!("rejected:{k}")));
    }
    let hir = msl_sema::analyze(&checked.module)?;
    emit(&hir, t, source_name)
}
