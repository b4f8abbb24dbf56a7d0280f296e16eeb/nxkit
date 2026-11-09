//! Function bodies: clang -O0 style code (every local in an `alloca`, values in SSA temporaries,
//! structured control flow as branches).

use std::fmt::Write;

use msl_sema::hir::{
    Arg, BinOp, CmpOp, Expr, ExprKind, Func, LocalId, Place, PlaceKind, Stmt, UnOp,
};
use msl_sema::types::{is_float, is_signed, Scalar, Space, Type};
use msl_sema::Error;

use crate::ir::{
    const_text, int_const, kind_letter, llty, scalar_bits, scalar_ty, space_num, space_suffix,
    suffix, zero_text, CVal,
};
use crate::ModCx;

type R<T> = Result<T, Error>;

/// Fast-math flags clang sets on float operations for the engine's flags
/// (`-O1 -fpreserve-invariance -fwrapv`, no `contract`/`reassoc`).
pub const FM: &str = "nnan ninf nsz arcp afn";

/// A value: its LLVM spelling and its type (value form).
#[derive(Clone, Debug)]
pub struct V {
    /// `%v3`, a constant, `undef` ...
    pub r: String,
    /// Type.
    pub ty: Type,
}

/// A pointer to storage.
#[derive(Clone, Debug)]
pub struct Addr {
    /// Pointer value.
    pub ptr: String,
    /// Pointee type (memory form spelled from it).
    pub ty: Type,
    /// Address space number.
    pub space: u32,
}

/// Lowers an entry point's `return` into its ABI.
pub type EntryRet = Box<dyn Fn(&mut FnGen<'_>, Option<V>) -> R<()>>;

/// A branch through cleanups that is not resolved yet.
#[derive(Clone, Debug)]
pub struct Fixup {
    /// Placeholder label the branch was emitted with.
    ph: String,
    /// Final destination label.
    dest: String,
    /// Cleanup depth at the destination (resolve once the stack is this deep).
    depth: usize,
}

/// How `return` leaves the function.
pub enum RetKind {
    /// A helper: `ret <ty> v`.
    Plain,
    /// An entry point: the caller-provided closure turns the value into the entry ABI.
    Entry(EntryRet),
}

/// Code generator for one function.
pub struct FnGen<'a> {
    /// Module context (types, declarations, globals).
    pub cx: &'a mut ModCx,
    /// The function.
    pub f: &'a Func,
    allocas: Vec<String>,
    body: Vec<String>,
    next: u32,
    /// Current block label.
    pub cur: String,
    terminated: bool,
    /// Pointer of each local's alloca.
    pub locals: Vec<String>,
    /// (break label, continue label, cleanup depth at loop entry)
    loops: Vec<(String, String, usize)>,
    /// clang's cleanup stack: one entry per declared local (lifetime end), with the pending
    /// branch-throughs that still have to pass it.
    cleanups: Vec<(LocalId, Vec<Fixup>)>,
    /// Code to put before a branch-through's `br` when cleanups are emitted at the branch.
    inline_at: crate::fx::HashMap<String, Vec<String>>,
    /// `cleanup.dest.slot`
    dest_slot: Option<String>,
    /// placeholder label -> resolved label
    fixes: crate::fx::HashMap<String, String>,
    /// destination index of each branch-through target label (return is 1)
    dest_ids: crate::fx::HashMap<String, i32>,
    deferred_fix: Vec<Fixup>,
    unreachable_blocks: Vec<String>,
    ret: Option<RetKind>,
    /// Struct-path of field addresses: pointer -> (outermost struct, byte offset).
    paths: crate::fx::HashMap<String, (usize, u32)>,
    /// `a*b + c` may become `llvm.fmuladd` here (no `#pragma clang fp contract(off)` in effect).
    contract: bool,
    /// Nonzero while loading or storing a whole vector for a component access (clang tags none).
    pub(crate) no_tbaa: u32,
    /// clang's `%retval` slot of a non-void function.
    ret_slot: Option<String>,
    /// Known alignment of derived addresses (clang: the base's alignment at the field's offset).
    aligns: crate::fx::HashMap<String, u32>,
    /// Loading a sampler for a sampling call: clang copies it (`!tbaa.struct`).
    pub(crate) sampler_copy: bool,
    /// Loading the texture a method is called on (clang: through `this`, tagged).
    pub(crate) tex_method: bool,
    /// Texture addresses of the enclosing texture method calls that were computed before their
    /// arguments (innermost last, `None` where the handle is evaluated after them).
    pub(crate) tex_pre: Vec<Option<Addr>>,
    seq_depth: u32,
    /// The function passes textures by value to a template (dm_bs): clang's GVN merges the method's
    /// tagged handle load with the untagged copy, so no tag.
    copies_texture: bool,
}

impl<'a> FnGen<'a> {
    /// A generator, allocas for every local are created up front.
    pub fn new(cx: &'a mut ModCx, f: &'a Func) -> FnGen<'a> {
        let mut g = FnGen {
            cx,
            f,
            allocas: Vec::new(),
            body: Vec::new(),
            next: 0,
            cur: "entry".into(),
            terminated: false,
            locals: Vec::new(),
            loops: Vec::new(),
            cleanups: Vec::new(),
            inline_at: crate::fx::HashMap::default(),
            dest_slot: None,
            fixes: crate::fx::HashMap::default(),
            deferred_fix: Vec::new(),
            unreachable_blocks: Vec::new(),
            dest_ids: crate::fx::HashMap::default(),
            ret: None,
            paths: crate::fx::HashMap::default(),
            contract: f.contract,
            no_tbaa: 0,
            ret_slot: None,
            aligns: crate::fx::HashMap::default(),
            sampler_copy: false,
            tex_method: false,
            tex_pre: Vec::new(),
            seq_depth: 0,
            copies_texture: false,
        };
        let mut calls_template = false;
        msl_sema::hir::visit_calls(&f.body, &mut |fid| {
            // dm_pool_mark takes no texture
            calls_template |=
                g.cx.funcs_template[fid] && !g.cx.funcs_sig[fid].0.starts_with("_ZL12dm_pool_mark")
        });
        g.copies_texture = calls_template;
        if f.ret != Type::Void {
            let r = f.ret.clone();
            g.ret_slot = Some(g.alloca(&r));
        }
        for l in &f.locals {
            let p = g.alloca(&l.ty.clone());
            g.locals.push(p);
        }
        g
    }

    /// Sets how `return` is lowered.
    pub fn set_ret(&mut self, r: RetKind) {
        self.ret = Some(r);
    }

    /// A fresh SSA name.
    pub fn tmp(&mut self) -> String {
        self.next += 1;
        format!("%v{}", self.next)
    }

    /// A fresh block label (without `%`).
    pub fn label(&mut self, hint: &str) -> String {
        self.next += 1;
        format!("{hint}{}", self.next)
    }

    /// Appends an instruction line.
    pub fn emit(&mut self, line: String) {
        if self.terminated {
            // Code after a terminator goes into a fresh unreachable block.
            let l = self.label("dead");
            self.body.push(format!("{l}:"));
            self.cur = l;
            self.terminated = false;
        }
        self.body.push(format!("  {line}"));
    }

    /// Ends the block with a terminator.
    pub fn term(&mut self, line: String) {
        self.emit(line);
        self.terminated = true;
    }

    /// Starts a block, falling through from the current one.
    pub fn block(&mut self, label: &str) {
        if !self.terminated {
            self.body.push(format!("  br label %{label}"));
        }
        self.body.push(format!("{label}:"));
        self.cur = label.to_string();
        self.terminated = false;
    }

    /// True when the current block already ended.
    pub fn is_terminated(&self) -> bool {
        self.terminated
    }

    /// A new stack slot of type `t` in the entry block.
    pub fn alloca(&mut self, t: &Type) -> String {
        let p = self.tmp();
        let lt = llty(&self.cx.types, t, true);
        let a = self.cx.types.layout(t).align.max(1);
        self.allocas.push(format!("  {p} = alloca {lt}, align {a}"));
        p
    }

    /// Ends the body: falls through to the return block and lowers it (plain `ret` or the entry ABI).
    pub fn body_done(&mut self) -> R<()> {
        if !self.terminated && self.f.ret != Type::Void {
            self.term("unreachable".into());
        }
        while !self.cleanups.is_empty() {
            self.pop_cleanup();
        }
        self.block("return");
        let val = match self.ret_slot.clone() {
            Some(slot) => {
                self.no_tbaa += 1;
                let v = self.load_from(&Addr {
                    ptr: slot,
                    ty: self.f.ret.clone(),
                    space: 0,
                });
                self.no_tbaa -= 1;
                Some(v)
            }
            None => None,
        };
        let r = self.ret.take();
        match &r {
            Some(RetKind::Entry(f)) => f(self, val)?,
            _ => match val {
                Some(v) => {
                    let t = self.lt(&v.ty);
                    self.term(format!("ret {t} {}", v.r));
                }
                None => self.term("ret void".into()),
            },
        }
        self.ret = r;
        Ok(())
    }

    /// The finished body text (allocas first).
    pub fn finish(mut self) -> String {
        if !self.terminated {
            match self.f.ret {
                Type::Void
                    if self
                        .ret
                        .as_ref()
                        .is_some_and(|r| matches!(r, RetKind::Plain)) =>
                {
                    self.body.push("  ret void".into());
                }
                _ => self.body.push("  unreachable".into()),
            }
        }
        for u in std::mem::take(&mut self.unreachable_blocks) {
            self.body.push(format!("{u}:"));
            self.body.push("  unreachable".into());
        }
        let inline = std::mem::take(&mut self.inline_at);
        let mut body2 = Vec::with_capacity(self.body.len());
        for line in std::mem::take(&mut self.body) {
            if let Some(ph) = line.trim_start().strip_prefix(";CLEANUP ") {
                if let Some(code) = inline.get(ph) {
                    for c in code {
                        body2.push(format!("  {c}"));
                    }
                }
                continue;
            }
            body2.push(line);
        }
        self.body = body2;
        let fixes = std::mem::take(&mut self.fixes);
        let resolve = |l: &str| -> String {
            let mut cur = l.to_string();
            for _ in 0..64 {
                match fixes.get(&cur) {
                    Some(n) => cur = n.clone(),
                    None => break,
                }
            }
            cur
        };
        if !fixes.is_empty() {
            for line in self.body.iter_mut() {
                if line.contains("%clfix") {
                    let mut out = String::new();
                    let mut rest = line.as_str();
                    while let Some(k) = rest.find("%clfix") {
                        out.push_str(&rest[..k + 1]);
                        let tail = &rest[k + 1..];
                        let end = tail
                            .find(|c: char| !c.is_ascii_alphanumeric())
                            .unwrap_or(tail.len());
                        out.push_str(&resolve(&tail[..end]));
                        rest = &tail[end..];
                    }
                    out.push_str(rest);
                    *line = out;
                }
            }
        }
        let mut out = String::from("entry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for l in &self.body {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    fn lt(&self, t: &Type) -> String {
        llty(&self.cx.types, t, false)
    }

    fn mt(&self, t: &Type) -> String {
        llty(&self.cx.types, t, true)
    }

    fn align(&self, t: &Type) -> u32 {
        match t {
            Type::Packed(s, _) => s.size(),
            t => self.cx.types.layout(t).align.max(1),
        }
    }

    // ------------------------------------------------------------------ statements

    /// Emits statements.
    pub fn stmts(&mut self, ss: &[Stmt]) -> R<()> {
        for s in ss {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// Statements of a scope: cleanups pushed by its declarations are popped at its end.
    pub fn scope(&mut self, ss: &[Stmt]) -> R<()> {
        let mark = self.cleanups.len();
        self.stmts(ss)?;
        while self.cleanups.len() > mark {
            self.pop_cleanup();
        }
        Ok(())
    }

    /// `bitcast` and `llvm.lifetime.start/end` lines for a local.
    fn lifetime(&mut self, which: &str, id: LocalId) -> Vec<String> {
        let ty = self.f.locals[id].ty.clone();
        let size = self.cx.types.layout(&ty).size;
        let mt = self.mt(&ty);
        let name = format!("llvm.lifetime.{which}.p0i8");
        let d = self.cx.declare(
            &name,
            &format!("void @{name}(i64 immarg, i8* nocapture)"),
            crate::Attrs::Lifetime,
        );
        let c = self.tmp();
        let p = self.locals[id].clone();
        vec![
            format!("{c} = bitcast {mt}* {p} to i8*"),
            format!(
                "call void @{d}(i64 {size}, i8* {c}) #{}",
                crate::Attrs::NoUnwind.group()
            ),
        ]
    }

    fn dest_index(&mut self, label: &str) -> i32 {
        if label == "return" {
            return 1;
        }
        let n = self.dest_ids.len() as i32 + 2;
        *self.dest_ids.entry(label.to_string()).or_insert(n)
    }

    /// clang's EmitBranchThroughCleanup.
    fn branch_through(&mut self, dest: &str, depth: usize) {
        if self.terminated {
            return;
        }
        if self.cleanups.len() <= depth {
            self.term(format!("br label %{dest}"));
            return;
        }
        let idx = self.dest_index(dest);
        let slot = match self.dest_slot.clone() {
            Some(s) => s,
            None => {
                let s = self.alloca(&Type::INT);
                self.dest_slot = Some(s.clone());
                s
            }
        };
        self.emit(format!("store i32 {idx}, i32* {slot}, align 4"));
        let ph = self.label("clfix");
        self.emit(format!(";CLEANUP {ph}"));
        self.term(format!("br label %{ph}"));
        let f = Fixup {
            ph,
            dest: dest.to_string(),
            depth,
        };
        self.cleanups.last_mut().expect("cleanup").1.push(f);
    }

    /// Pops the innermost cleanup (clang's PopCleanupBlock, lifetime markers left out).
    fn pop_cleanup(&mut self) {
        let Some((local, pending)) = self.cleanups.pop() else {
            return;
        };
        let ft = !self.terminated;
        let entries = pending.len() + ft as usize;
        if entries == 0 {
            return;
        }
        let end = self.lifetime("end", local);
        if entries == 1 && ft {
            for l in end {
                self.emit(l);
            }
            return;
        }
        if entries == 1 {
            // one branch and no fallthrough: the cleanup is emitted at the branch, which goes on
            let f = pending.into_iter().next().expect("one");
            self.inline_at.entry(f.ph.clone()).or_default().extend(end);
            self.forward(f);
            return;
        }
        let l = self.label("cleanup");
        for f in &pending {
            self.fixes.insert(f.ph.clone(), l.clone());
        }
        if ft {
            let slot = self
                .dest_slot
                .clone()
                .expect("slot exists with pending branches");
            self.emit(format!("store i32 0, i32* {slot}, align 4"));
        }
        self.block(&l);
        for line in end {
            self.emit(line);
        }
        let mut dests: Vec<Fixup> = Vec::new();
        for f in pending {
            if !dests.iter().any(|d| d.dest == f.dest) {
                dests.push(f);
            }
        }
        if !ft && dests.len() == 1 {
            let f = dests.pop().expect("one");
            let ph = self.label("clfix");
            self.emit(format!(";CLEANUP {ph}"));
            self.term(format!("br label %{ph}"));
            self.forward(Fixup { ph, ..f });
            return;
        }
        let slot = self.dest_slot.clone().expect("slot");
        let d = self.tmp();
        self.emit(format!("{d} = load i32, i32* {slot}, align 4"));
        let cont = self.label("cleanup.cont");
        let mut targets: Vec<(i32, String)> = Vec::new();
        for f in dests {
            let idx = self.dest_index(&f.dest);
            let ph = self.label("clfix");
            targets.push((idx, ph.clone()));
            self.forward_later(Fixup { ph, ..f });
        }
        let (default, cases) = if ft {
            let (_, first) = targets.remove(0);
            let mut cases = format!(" i32 0, label %{cont}");
            for (i, t) in &targets {
                cases.push_str(&format!(" i32 {i}, label %{t}"));
            }
            (first, cases)
        } else {
            let un = self.label("unreachable");
            let mut cases = String::new();
            for (i, t) in &targets {
                cases.push_str(&format!(" i32 {i}, label %{t}"));
            }
            self.unreachable_blocks.push(un.clone());
            (un, cases)
        };
        self.term(format!("switch i32 {d}, label %{default} [{cases} ]"));
        if ft {
            self.block(&cont);
        }
        self.flush_forward();
    }

    fn forward(&mut self, f: Fixup) {
        if self.cleanups.len() <= f.depth {
            self.fixes.insert(f.ph, f.dest);
        } else {
            self.cleanups.last_mut().expect("parent").1.push(f);
        }
    }

    fn forward_later(&mut self, f: Fixup) {
        self.deferred_fix.push(f);
    }

    fn flush_forward(&mut self) {
        for f in std::mem::take(&mut self.deferred_fix) {
            self.forward(f);
        }
    }

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        match s {
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
            Stmt::Block(b) => self.scope(b)?,
            Stmt::NoContract(b) => {
                let saved = std::mem::replace(&mut self.contract, false);
                let r = self.scope(b);
                self.contract = saved;
                r?;
            }
            Stmt::Decl(id) => {
                let lines = self.lifetime("start", *id);
                for l in lines {
                    self.emit(l);
                }
                self.cleanups.push((*id, Vec::new()));
            }
            Stmt::If { cond, then, els } => {
                let lt = self.label("then");
                let le = self.label("else");
                let lx = self.label("endif");
                let target_else = if els.is_empty() {
                    lx.clone()
                } else {
                    le.clone()
                };
                self.branch_on(cond, &lt, &target_else, "")?;
                self.block(&lt);
                self.scope(then)?;
                if !els.is_empty() {
                    self.br(&lx);
                    self.block(&le);
                    self.scope(els)?;
                }
                self.block(&lx);
            }
            Stmt::Switch { cond, arms } => {
                let c = self.expr(cond)?;
                let Type::Scalar(cs) = c.ty else {
                    return Err(Error::invalid("switch type"));
                };
                let lx = self.label("swend");
                let labels: Vec<String> = (0..arms.len()).map(|_| self.label("case")).collect();
                let mut default = lx.clone();
                let mut cases = String::new();
                for (arm, l) in arms.iter().zip(labels.iter()) {
                    for v in &arm.labels {
                        match v {
                            None => default = l.clone(),
                            Some(v) => {
                                let _ = write!(
                                    cases,
                                    " {} {}, label %{l}",
                                    scalar_ty(cs, false),
                                    int_const(*v as i128, cs, false)
                                );
                            }
                        }
                    }
                }
                self.term(format!(
                    "switch {} {}, label %{default} [{cases} ]",
                    self.lt(&c.ty),
                    c.r
                ));
                let depth = self.cleanups.len();
                self.loops.push((lx.clone(), String::new(), depth));
                for (arm, l) in arms.iter().zip(labels.iter()) {
                    self.block(l);
                    self.stmts(&arm.body)?;
                }
                while self.cleanups.len() > depth {
                    self.pop_cleanup();
                }
                self.loops.pop();
                self.block(&lx);
            }
            Stmt::While { cond, body } => {
                let lh = self.label("while");
                let lb = self.label("wbody");
                let lx = self.label("wend");
                self.block(&lh);
                let const_cond = matches!(cond.kind, ExprKind::Int(_));
                if const_cond {
                    self.not_mustprogress();
                }
                match &cond.kind {
                    ExprKind::Int(v) if *v != 0 => self.br(&lb),
                    _ => self.branch_on(cond, &lb, &lx, "")?,
                }
                self.block(&lb);
                let depth = self.cleanups.len();
                self.loops.push((lx.clone(), lh.clone(), depth));
                self.scope(body)?;
                self.loops.pop();
                if !self.terminated {
                    let md = if const_cond {
                        self.loop_md_const()
                    } else {
                        self.loop_md()
                    };
                    self.term(format!("br label %{lh}{md}"));
                }
                self.block(&lx);
            }
            Stmt::DoWhile { body, cond } => {
                let lb = self.label("do");
                let lc = self.label("docond");
                let lx = self.label("doend");
                self.block(&lb);
                let depth = self.cleanups.len();
                self.loops.push((lx.clone(), lc.clone(), depth));
                self.scope(body)?;
                self.loops.pop();
                self.block(&lc);
                match &cond.kind {
                    // clang: a constant condition makes no conditional branch, and such a loop
                    // (and its function) is not mustprogress
                    ExprKind::Int(v) => {
                        self.not_mustprogress();
                        if *v == 0 {
                            self.br(&lx);
                        } else {
                            let md = self.loop_md_const();
                            self.term(format!("br label %{lb}{md}"));
                        }
                    }
                    _ => {
                        let md = self.loop_md();
                        self.branch_on(cond, &lb, &lx, &md)?;
                    }
                }
                self.block(&lx);
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                let for_mark = self.cleanups.len();
                self.stmts(init)?;
                let lh = self.label("for");
                let lb = self.label("fbody");
                let ls = self.label("fstep");
                let lx = self.label("fend");
                self.block(&lh);
                match cond {
                    Some(c) => self.branch_on(c, &lb, &lx, "")?,
                    None => self.br(&lb),
                }
                self.block(&lb);
                let depth = self.cleanups.len();
                self.loops.push((lx.clone(), ls.clone(), depth));
                self.scope(body)?;
                self.loops.pop();
                self.block(&ls);
                if let Some(s) = step {
                    self.expr(s)?;
                }
                if !self.terminated {
                    let md = self.loop_md();
                    self.term(format!("br label %{lh}{md}"));
                }
                self.block(&lx);
                while self.cleanups.len() > for_mark {
                    self.pop_cleanup();
                }
            }
            Stmt::Break => {
                let (b, _, depth) = self
                    .loops
                    .last()
                    .cloned()
                    .ok_or_else(|| Error::invalid("break outside loop"))?;
                self.branch_through(&b, depth);
            }
            Stmt::Continue => {
                let (c, depth) = self
                    .loops
                    .iter()
                    .rev()
                    .find(|(_, c, _)| !c.is_empty())
                    .map(|(_, c, d)| (c.clone(), *d))
                    .ok_or_else(|| Error::invalid("continue outside loop"))?;
                self.branch_through(&c, depth);
            }
            Stmt::Return(v) => {
                // clang: store to %retval and branch to the one return block
                if let Some(e) = v {
                    let val = self.expr(e)?;
                    if let Some(slot) = self.ret_slot.clone() {
                        self.no_tbaa += 1;
                        self.store_to(
                            &Addr {
                                ptr: slot,
                                ty: self.f.ret.clone(),
                                space: 0,
                            },
                            &val,
                        );
                        self.no_tbaa -= 1;
                    }
                }
                self.branch_through("return", 0);
            }
            Stmt::Discard => {
                let d = self.cx.declare_wrapped(
                    "air.discard_fragment",
                    "void @air.discard_fragment()",
                    crate::Attrs::NoUnwindWillReturn,
                );
                self.emit(format!(
                    "call void @{d}() #{}",
                    crate::Attrs::NoUnwindWillReturn.group()
                ));
            }
        }
        Ok(())
    }

    /// clang's EmitBranchOnBoolExpr: `&&`, `||` and `!` become branches, not values.
    fn branch_on(&mut self, e: &Expr, t: &str, f: &str, md: &str) -> R<()> {
        match &e.kind {
            ExprKind::Logical(true, a, b) => {
                let l = self.label("land");
                self.branch_on(a, &l, f, "")?;
                self.block(&l);
                self.branch_on(b, t, f, md)
            }
            ExprKind::Logical(false, a, b) => {
                let l = self.label("lor");
                self.branch_on(a, t, &l, "")?;
                self.block(&l);
                self.branch_on(b, t, f, md)
            }
            ExprKind::Unary(UnOp::Not, x) => self.branch_on(x, f, t, md),
            _ => {
                let c = self.expr(e)?;
                self.term(format!("br i1 {}, label %{t}, label %{f}{md}", c.r));
                Ok(())
            }
        }
    }

    /// Loop metadata of a loop with a constant condition (no mustprogress).
    fn loop_md_const(&mut self) -> String {
        let b = self
            .cx
            .meta
            .node(&[crate::meta::s("llvm.loop.unroll.disable")]);
        let n = self.cx.meta.distinct_self(&[crate::meta::Md::Node(b)]);
        format!(", !llvm.loop !{n}")
    }

    /// The function contains a loop that is not mustprogress: clang drops the function attribute.
    pub(crate) fn not_mustprogress(&mut self) {
        self.allocas.push("; air-emit: no-mustprogress".into());
    }

    /// clang's loop metadata for a backedge: `, !llvm.loop !N`.
    fn loop_md(&mut self) -> String {
        let a = self
            .cx
            .meta
            .node(&[crate::meta::s("llvm.loop.mustprogress")]);
        let b = self
            .cx
            .meta
            .node(&[crate::meta::s("llvm.loop.unroll.disable")]);
        let n = self
            .cx
            .meta
            .distinct_self(&[crate::meta::Md::Node(a), crate::meta::Md::Node(b)]);
        format!(", !llvm.loop !{n}")
    }

    /// Unconditional branch ending the block.
    pub fn br(&mut self, label: &str) {
        if !self.terminated {
            self.term(format!("br label %{label}"));
        }
    }

    // ------------------------------------------------------------------ places

    /// The address of an addressable place (`None` for a vector element or swizzle of a vector value).
    pub fn addr(&mut self, p: &Place) -> R<Option<Addr>> {
        Ok(Some(match &p.kind {
            PlaceKind::Local(id) => Addr {
                ptr: self.locals[*id].clone(),
                ty: p.ty.clone(),
                space: 0,
            },
            PlaceKind::Const(id) => Addr {
                ptr: self.cx.const_names[*id].clone(),
                ty: p.ty.clone(),
                space: 2,
            },
            PlaceKind::Deref(e) => {
                let v = self.expr(e)?;
                Addr {
                    ptr: v.r,
                    ty: p.ty.clone(),
                    space: space_num(p.space),
                }
            }
            PlaceKind::Temp(e) => {
                let v = self.expr(e)?;
                let slot = self.alloca(&v.ty);
                self.store_to(
                    &Addr {
                        ptr: slot.clone(),
                        ty: v.ty.clone(),
                        space: 0,
                    },
                    &v,
                );
                Addr {
                    ptr: slot,
                    ty: v.ty.clone(),
                    space: 0,
                }
            }
            PlaceKind::Field(base, i) => {
                let Some(b) = self.addr(base)? else {
                    return Err(Error::invalid("field of non-addressable"));
                };
                let st = self.mt(&b.ty);
                let r = self.tmp();
                let sp = space_suffix(b.space);
                self.emit(format!(
                    "{r} = getelementptr inbounds {st}, {st}{sp}* {}, i32 0, i32 {i}",
                    b.ptr
                ));
                if let Type::Struct(sid) = b.ty {
                    let fo = self.cx.types.structs[sid].fields[*i as usize].offset;
                    let ba = self.addr_align(&b);
                    self.aligns.insert(r.clone(), common_align(ba, fo));
                    let path = match self.paths.get(&b.ptr) {
                        Some((root, off)) => (*root, off + fo),
                        None => (sid, fo),
                    };
                    self.paths.insert(r.clone(), path);
                }
                Addr {
                    ptr: r,
                    ty: p.ty.clone(),
                    space: b.space,
                }
            }
            PlaceKind::Index(base, idx) => {
                // C++17: the array operand is evaluated before the index
                let Some(b) = self.addr(base)? else {
                    return Err(Error::invalid("index of non-addressable"));
                };
                let i = self.expr(idx)?;
                let i64v = self.index_i64(i);
                let at = self.mt(&b.ty);
                let r = self.tmp();
                let sp = space_suffix(b.space);
                self.emit(format!(
                    "{r} = getelementptr inbounds {at}, {at}{sp}* {}, i64 0, i64 {i64v}",
                    b.ptr
                ));
                let es = self.cx.types.layout(&p.ty).size;
                let ba = self.addr_align(&b);
                let off = i64v.parse::<u32>().map(|k| k * es).unwrap_or(es);
                self.aligns.insert(r.clone(), common_align(ba, off));
                Addr {
                    ptr: r,
                    ty: p.ty.clone(),
                    space: b.space,
                }
            }
            PlaceKind::Element(base, idx) => {
                if !matches!(base.ty, Type::Packed(..)) {
                    return Ok(None);
                }
                let i = self.expr(idx)?;
                let i64v = self.index_i64(i);
                let Some(b) = self.addr(base)? else {
                    return Ok(None);
                };
                let at = self.mt(&b.ty);
                let r = self.tmp();
                let sp = space_suffix(b.space);
                self.emit(format!(
                    "{r} = getelementptr inbounds {at}, {at}{sp}* {}, i64 0, i64 {i64v}",
                    b.ptr
                ));
                Addr {
                    ptr: r,
                    ty: p.ty.clone(),
                    space: b.space,
                }
            }
            PlaceKind::Swizzle(..) => return Ok(None),
        }))
    }

    fn index_i64(&mut self, v: V) -> String {
        let Type::Scalar(s) = v.ty else { return v.r };
        if scalar_bits(s) == 64 {
            return v.r;
        }
        if let Ok(n) = v.r.parse::<i64>() {
            let n = if is_signed(s) {
                n
            } else {
                n & ((1i64 << scalar_bits(s)) - 1)
            };
            return n.to_string();
        }
        let r = self.tmp();
        let op = if is_signed(s) { "sext" } else { "zext" };
        self.emit(format!("{r} = {op} {} {} to i64", scalar_ty(s, false), v.r));
        r
    }

    /// The handle inside a `metal::texture*` / `metal::sampler` wrapper and its TBAA tag.
    fn handle_addr(&mut self, a: &Addr) -> (String, String) {
        let wt = self.mt(&a.ty);
        let sp = space_suffix(a.space);
        let r = self.tmp();
        self.emit(format!(
            "{r} = getelementptr inbounds {wt}, {wt}{sp}* {}, i32 0, i32 0",
            a.ptr
        ));
        let md = if self.no_tbaa > 0 {
            String::new()
        } else if a.ty == Type::Sampler {
            let tag = crate::tbaa::handle_scalar_tag(self.cx, &a.ty);
            let n = self.cx.meta.node(&[
                crate::meta::Md::Raw("i64 0".into()),
                crate::meta::Md::Raw("i64 8".into()),
                crate::meta::Md::Node(tag),
            ]);
            format!(", !tbaa.struct !{n}")
        } else if self.tex_method {
            let tag = crate::tbaa::handle_tag(self.cx, &a.ty);
            format!(", !tbaa !{tag}")
        } else {
            String::new()
        };
        (r, md)
    }

    /// The alignment clang uses for an access through this address.
    fn addr_align(&self, a: &Addr) -> u32 {
        self.aligns
            .get(&a.ptr)
            .copied()
            .unwrap_or_else(|| self.align(&a.ty))
    }

    /// Loads a value from an address (memory form to value form).
    pub fn load_from(&mut self, a: &Addr) -> V {
        if matches!(a.ty, Type::Texture(_) | Type::Sampler) {
            let (p, md) = self.handle_addr(a);
            let ht = self.lt(&a.ty);
            let sp = space_suffix(a.space);
            let r = self.tmp();
            self.emit(format!("{r} = load {ht}, {ht}{sp}* {p}, align 8{md}"));
            return V {
                r,
                ty: a.ty.clone(),
            };
        }
        let sp = space_suffix(a.space);
        let vt = a.ty.unpacked();
        if let Type::Packed(es, n) = a.ty {
            // clang reads a packed vector element by element (no TBAA)
            let mt = self.mt(&a.ty);
            let et = scalar_ty(es, true);
            let vtt = self.lt(&vt);
            let base = self.addr_align(a);
            let mut cur = "undef".to_string();
            for k in 0..n {
                let g = self.tmp();
                self.emit(format!(
                    "{g} = getelementptr inbounds {mt}, {mt}{sp}* {}, i64 0, i64 {k}",
                    a.ptr
                ));
                let x = self.tmp();
                let al = common_align(base, k as u32 * es.size());
                self.emit(format!("{x} = load {et}, {et}{sp}* {g}, align {al}"));
                let r = self.tmp();
                self.emit(format!(
                    "{r} = insertelement {vtt} {cur}, {et} {x}, i32 {k}"
                ));
                cur = r;
            }
            return V { r: cur, ty: vt };
        }
        let al = self.addr_align(a);
        let mt = self.mt(&a.ty);
        let r = self.tmp();
        let md = self.access_md(a);
        self.emit(format!(
            "{r} = load {mt}, {mt}{sp}* {}, align {al}{md}",
            a.ptr
        ));
        if a.ty.elem() == Some(Scalar::Bool) {
            let t = self.tmp();
            self.emit(format!("{t} = trunc {mt} {r} to {}", self.lt(&a.ty)));
            return V { r: t, ty: vt };
        }
        V { r, ty: vt }
    }

    /// Stores a value to an address (value form to memory form).
    pub fn store_to(&mut self, a: &Addr, v: &V) {
        if matches!(a.ty, Type::Texture(_) | Type::Sampler) {
            let (p, md) = self.handle_addr(a);
            let ht = self.lt(&a.ty);
            let sp = space_suffix(a.space);
            self.emit(format!("store {ht} {}, {ht}{sp}* {p}, align 8{md}", v.r));
            return;
        }
        let sp = space_suffix(a.space);
        if let Type::Packed(es, n) = a.ty {
            // clang writes a packed vector element by element, each with the element's TBAA tag
            let mt = self.mt(&a.ty);
            let et = scalar_ty(es, true);
            let vtt = self.lt(&v.ty);
            let base = self.addr_align(a);
            let md = if self.no_tbaa > 0 {
                String::new()
            } else {
                let t = crate::tbaa::scalar(self.cx, es);
                format!(", !tbaa !{}", crate::tbaa::tag(self.cx, t, t, 0))
            };
            for k in 0..n {
                let g = self.tmp();
                self.emit(format!(
                    "{g} = getelementptr inbounds {mt}, {mt}{sp}* {}, i64 0, i64 {k}",
                    a.ptr
                ));
                let x = self.tmp();
                self.emit(format!("{x} = extractelement {vtt} {}, i32 {k}", v.r));
                let al = common_align(base, k as u32 * es.size());
                self.emit(format!("store {et} {x}, {et}{sp}* {g}, align {al}{md}"));
            }
            return;
        }
        let al = self.addr_align(a);
        let mt = self.mt(&a.ty);
        let mut val = v.r.clone();
        if a.ty.elem() == Some(Scalar::Bool) {
            let t = self.tmp();
            self.emit(format!("{t} = zext {} {} to {mt}", self.lt(&v.ty), v.r));
            val = t;
        }
        let mut md = self.access_md(a);
        if let Some(k) = md.find(", !range") {
            md.truncate(k);
        }
        self.emit(format!(
            "store {mt} {val}, {mt}{sp}* {}, align {al}{md}",
            a.ptr
        ));
    }

    /// `, !tbaa !N[, !range !M]` for a scalar or vector access, as clang's struct-path TBAA has it.
    fn access_md(&mut self, a: &Addr) -> String {
        if self.no_tbaa > 0 {
            return String::new();
        }
        let path = self.paths.get(&a.ptr).copied();
        let tag = match &a.ty {
            Type::Scalar(s) => {
                let n = crate::tbaa::scalar(self.cx, *s);
                match path {
                    Some((root, off)) => {
                        let st = crate::tbaa::structure(self.cx, root);
                        crate::tbaa::tag(self.cx, st, n, off)
                    }
                    None => crate::tbaa::tag(self.cx, n, n, 0),
                }
            }
            Type::Ptr(..) => {
                let n = crate::tbaa::named(self.cx, "any pointer");
                match path {
                    Some((root, off)) => {
                        let st = crate::tbaa::structure(self.cx, root);
                        crate::tbaa::tag(self.cx, st, n, off)
                    }
                    None => crate::tbaa::tag(self.cx, n, n, 0),
                }
            }
            Type::Vector(s, _) => {
                let n = if path.is_some() || self.cx.vec_tbaa_char {
                    crate::tbaa::char(self.cx)
                } else {
                    crate::tbaa::scalar(self.cx, *s)
                };
                crate::tbaa::tag(self.cx, n, n, 0)
            }
            _ => return String::new(),
        };
        let mut out = format!(", !tbaa !{tag}");
        if a.ty == Type::BOOL {
            let r = self.cx.meta.node(&[
                crate::meta::Md::Raw("i8 0".into()),
                crate::meta::Md::Raw("i8 2".into()),
            ]);
            out.push_str(&format!(", !range !{r}"));
        }
        out
    }

    /// Loads a whole vector for a component access: the address is computed with its tags,
    /// the vector load itself carries none (as in clang).
    fn load_whole(&mut self, p: &Place) -> R<V> {
        match &p.kind {
            PlaceKind::Element(..) | PlaceKind::Swizzle(..) => self.load(p),
            _ => match self.addr(p)? {
                Some(a) => {
                    self.no_tbaa += 1;
                    let v = self.load_from(&a);
                    self.no_tbaa -= 1;
                    Ok(v)
                }
                None => self.load(p),
            },
        }
    }

    /// Loads a value without a TBAA tag on the final load (texture handles).
    pub fn load_untagged(&mut self, e: &Expr) -> R<V> {
        match &e.kind {
            ExprKind::Load(p) => self.load_whole(p),
            _ => self.expr(e),
        }
    }

    /// Loads the texture handle a method is called on, with the wrapper's TBAA tag.
    pub fn load_texture_this(&mut self, e: &Expr) -> R<V> {
        // inside a dm_bs expansion clang also copies the texture by value, GVN then merges the
        // two loads and the tag is dropped
        self.tex_method = self.seq_depth == 0 && !self.copies_texture;
        let v = self.expr(e);
        self.tex_method = false;
        v
    }

    /// Loads a texture handle for a method call from an address computed before the call's
    /// arguments. A pool element is never copied into a dm_bs expansion, so its load keeps the tag.
    pub fn load_texture_at(&mut self, a: &Addr) -> R<V> {
        self.tex_method = self.seq_depth == 0;
        let v = self.load_from(a);
        self.tex_method = false;
        Ok(v)
    }

    /// Loads a place.
    pub fn load(&mut self, p: &Place) -> R<V> {
        match &p.kind {
            PlaceKind::Element(base, idx) if !matches!(base.ty, Type::Packed(..)) => {
                // `v[i]`: clang loads the whole vector with the base's TBAA tag
                let b = self.load(base)?;
                let i = self.expr(idx)?;
                let r = self.tmp();
                let bt = self.lt(&b.ty);
                self.emit(format!(
                    "{r} = extractelement {bt} {}, {} {}",
                    b.r,
                    self.lt(&i.ty),
                    i.r
                ));
                Ok(V {
                    r,
                    ty: p.ty.unpacked(),
                })
            }
            PlaceKind::Swizzle(base, comps) => {
                let b = self.load_whole(base)?;
                Ok(self.shuffle(&b, comps))
            }
            _ => {
                let a = self
                    .addr(p)?
                    .ok_or_else(|| Error::invalid("load of non-addressable"))?;
                Ok(self.load_from(&a))
            }
        }
    }

    /// Stores to a place. A component store loads the whole vector, inserts and stores it back
    /// through one address computation, without TBAA tags (as clang does).
    pub fn store(&mut self, p: &Place, v: &V) -> R<()> {
        let (base, idx, comps) = match &p.kind {
            PlaceKind::Element(base, idx) if !matches!(base.ty, Type::Packed(..)) => {
                (base, Some(idx), None)
            }
            PlaceKind::Swizzle(base, comps) => (base, None, Some(comps)),
            _ => {
                let a = self
                    .addr(p)?
                    .ok_or_else(|| Error::invalid("store to non-addressable"))?;
                self.store_to(&a, v);
                return Ok(());
            }
        };
        let i = match idx {
            Some(x) => Some(self.expr(x)?),
            None => None,
        };
        let a = self.addr(base)?;
        let tagged = i.is_some();
        let b = match &a {
            Some(a) => {
                if !tagged {
                    self.no_tbaa += 1;
                }
                let b = self.load_from(a);
                if !tagged {
                    self.no_tbaa -= 1;
                }
                b
            }
            None => self.load(base)?,
        };
        let bt = self.lt(&b.ty);
        let cur = match (i, comps) {
            _ if matches!(b.ty, Type::Scalar(_)) => v.r.clone(),
            (Some(i), _) => {
                let r = self.tmp();
                self.emit(format!(
                    "{r} = insertelement {bt} {}, {} {}, {} {}",
                    b.r,
                    self.lt(&v.ty),
                    v.r,
                    self.lt(&i.ty),
                    i.r
                ));
                r
            }
            (None, Some(comps)) if comps.len() == 1 || matches!(v.ty, Type::Scalar(_)) => {
                let et = scalar_ty(b.ty.elem().unwrap_or(Scalar::Float), false);
                let r = self.tmp();
                self.emit(format!(
                    "{r} = insertelement {bt} {}, {et} {}, i64 {}",
                    b.r, v.r, comps[0]
                ));
                r
            }
            (None, Some(comps)) => {
                // clang: widen the stored vector with a shuffle, then merge it into the old value
                let n = b.ty.width().unwrap_or(4) as usize;
                let k = comps.len();
                let vt = self.lt(&v.ty);
                let src = if k == n {
                    v.r.clone()
                } else {
                    let ext: Vec<String> = (0..n)
                        .map(|i| {
                            if i < k {
                                format!("i32 {i}")
                            } else {
                                "i32 undef".to_string()
                            }
                        })
                        .collect();
                    let e = self.tmp();
                    self.emit(format!(
                        "{e} = shufflevector {vt} {}, {vt} poison, <{n} x i32> <{}>",
                        v.r,
                        ext.join(", ")
                    ));
                    e
                };
                let r = self.tmp();
                if k == n {
                    let mut mask = vec![0usize; n];
                    for (i, c) in comps.iter().enumerate() {
                        mask[*c as usize] = i;
                    }
                    let m: Vec<String> = mask.iter().map(|x| format!("i32 {x}")).collect();
                    self.emit(format!(
                        "{r} = shufflevector {vt} {src}, {vt} poison, <{n} x i32> <{}>",
                        m.join(", ")
                    ));
                } else {
                    let mut mask: Vec<usize> = (0..n).collect();
                    for (i, c) in comps.iter().enumerate() {
                        mask[*c as usize] = i + n;
                    }
                    let m: Vec<String> = mask.iter().map(|x| format!("i32 {x}")).collect();
                    self.emit(format!(
                        "{r} = shufflevector {bt} {}, {bt} {src}, <{n} x i32> <{}>",
                        b.r,
                        m.join(", ")
                    ));
                }
                r
            }
            _ => return Err(Error::invalid("component store")),
        };
        let nv = V { r: cur, ty: b.ty };
        match &a {
            Some(a) => {
                if !tagged {
                    self.no_tbaa += 1;
                }
                self.store_to(a, &nv);
                if !tagged {
                    self.no_tbaa -= 1;
                }
                Ok(())
            }
            None => self.store(base, &nv),
        }
    }

    fn shuffle(&mut self, b: &V, comps: &[u8]) -> V {
        if let Type::Scalar(_) = b.ty {
            return b.clone();
        }
        let s = b.ty.elem().unwrap_or(Scalar::Float);
        let ty = Type::vec(s, comps.len() as u8);
        let bt = self.lt(&b.ty);
        let r = self.tmp();
        if comps.len() == 1 {
            self.emit(format!(
                "{r} = extractelement {bt} {}, i32 {}",
                b.r, comps[0]
            ));
            return V { r, ty };
        }
        let mask: Vec<String> = comps.iter().map(|c| format!("i32 {c}")).collect();
        self.emit(format!(
            "{r} = shufflevector {bt} {}, {bt} poison, <{} x i32> <{}>",
            b.r,
            comps.len(),
            mask.join(", ")
        ));
        V { r, ty }
    }

    // ------------------------------------------------------------------ expressions

    /// Emits an expression and returns its value.
    pub fn expr(&mut self, e: &Expr) -> R<V> {
        let ty = e.ty.clone();
        match &e.kind {
            ExprKind::Int(v) => {
                let Type::Scalar(s) = ty else {
                    return Err(Error::invalid("int constant type"));
                };
                Ok(V {
                    r: crate::ir::scalar_const(&CVal::Int(*v), s, false),
                    ty,
                })
            }
            ExprKind::Float(v) => {
                let Type::Scalar(s) = ty else {
                    return Err(Error::invalid("float constant type"));
                };
                Ok(V {
                    r: crate::ir::scalar_const(&CVal::Float(*v), s, false),
                    ty,
                })
            }
            ExprKind::Zero => {
                let r = if matches!(ty, Type::Struct(_) | Type::Array(..)) {
                    zero_text(&self.cx.types, &ty, true)
                } else {
                    zero_text(&self.cx.types, &ty, false)
                };
                Ok(V { r, ty })
            }
            ExprKind::Load(p) => self.load(p),
            ExprKind::AddrOf(p) => {
                let a = self
                    .addr(p)?
                    .ok_or_else(|| Error::unsupported("address-of-component"))?;
                Ok(V { r: a.ptr, ty })
            }
            ExprKind::Unary(UnOp::Neg, x)
                if matches!(x.kind, ExprKind::Int(_) | ExprKind::Float(_)) =>
            {
                // clang folds the negation of a literal into the constant
                let Type::Scalar(s) = ty else {
                    return Err(Error::invalid("negated literal type"));
                };
                let c = match x.kind {
                    ExprKind::Int(v) => CVal::Int(-v),
                    ExprKind::Float(v) => CVal::Float(-v),
                    _ => CVal::Zero,
                };
                let c = match (c, is_float(s)) {
                    (CVal::Int(v), true) => CVal::Float(v as f64),
                    (c, _) => c,
                };
                Ok(V {
                    r: crate::ir::scalar_const(&c, s, false),
                    ty,
                })
            }
            ExprKind::Unary(op, x) => {
                let v = self.expr(x)?;
                let t = self.lt(&ty);
                let r = self.tmp();
                let s = ty.elem().ok_or_else(|| Error::invalid("unary type"))?;
                match op {
                    UnOp::Neg if is_float(s) => self.emit(format!("{r} = fneg {FM} {t} {}", v.r)),
                    UnOp::Neg => self.emit(format!(
                        "{r} = sub {t} {}, {}",
                        zero_text(&self.cx.types, &ty, false),
                        v.r
                    )),
                    UnOp::Not => self.emit(format!(
                        "{r} = xor {t} {}, {}",
                        v.r,
                        splat_text(&ty, "true")
                    )),
                    UnOp::BitNot => {
                        self.emit(format!("{r} = xor {t} {}, {}", v.r, splat_text(&ty, "-1")))
                    }
                }
                Ok(V { r, ty })
            }
            ExprKind::Binary(op, a, b) => {
                if let Some(v) = self.try_fmuladd(*op, a, b, &ty)? {
                    return Ok(v);
                }
                let x = self.expr(a)?;
                let y = self.expr(b)?;
                self.binop(*op, x, y, &ty)
            }
            ExprKind::Compare(op, a, b) => {
                let x = self.expr(a)?;
                let y = self.expr(b)?;
                let s = x.ty.elem().ok_or_else(|| Error::invalid("compare type"))?;
                let t = self.lt(&x.ty);
                let r = self.tmp();
                if is_float(s) {
                    let c = match op {
                        CmpOp::Eq => "oeq",
                        CmpOp::Ne => "une",
                        CmpOp::Lt => "olt",
                        CmpOp::Gt => "ogt",
                        CmpOp::Le => "ole",
                        CmpOp::Ge => "oge",
                    };
                    self.emit(format!("{r} = fcmp {FM} {c} {t} {}, {}", x.r, y.r));
                } else {
                    let sg = is_signed(s);
                    let c = match op {
                        CmpOp::Eq => "eq",
                        CmpOp::Ne => "ne",
                        CmpOp::Lt => {
                            if sg {
                                "slt"
                            } else {
                                "ult"
                            }
                        }
                        CmpOp::Gt => {
                            if sg {
                                "sgt"
                            } else {
                                "ugt"
                            }
                        }
                        CmpOp::Le => {
                            if sg {
                                "sle"
                            } else {
                                "ule"
                            }
                        }
                        CmpOp::Ge => {
                            if sg {
                                "sge"
                            } else {
                                "uge"
                            }
                        }
                    };
                    self.emit(format!("{r} = icmp {c} {t} {}, {}", x.r, y.r));
                }
                Ok(V { r, ty })
            }
            ExprKind::Logical(and, a, b) => {
                let x = self.expr(a)?;
                let from = self.cur.clone();
                let lb = self.label("rhs");
                let lx = self.label("logend");
                if *and {
                    self.term(format!("br i1 {}, label %{lb}, label %{lx}", x.r));
                } else {
                    self.term(format!("br i1 {}, label %{lx}, label %{lb}", x.r));
                }
                self.block(&lb);
                let y = self.expr(b)?;
                let yb = self.cur.clone();
                self.br(&lx);
                self.block(&lx);
                let r = self.tmp();
                let short = if *and { "false" } else { "true" };
                self.emit(format!(
                    "{r} = phi i1 [ {short}, %{from} ], [ {}, %{yb} ]",
                    y.r
                ));
                Ok(V { r, ty })
            }
            ExprKind::Cond(c, a, b) if FnGen::fold(a).is_some() && FnGen::fold(b).is_some() => {
                // clang: `c ? K1 : K2` with cheap operands is a select, the condition a value
                let cv = self.expr(c)?;
                let x = self.expr(a)?;
                let y = self.expr(b)?;
                let t = self.lt(&ty);
                let fm = if ty.elem().is_some_and(is_float) {
                    format!("{FM} ")
                } else {
                    String::new()
                };
                let r = self.tmp();
                self.emit(format!(
                    "{r} = select {fm}i1 {}, {t} {}, {t} {}",
                    cv.r, x.r, y.r
                ));
                Ok(V { r, ty })
            }
            ExprKind::Cond(c, a, b) => {
                let lt = self.label("cthen");
                let le = self.label("celse");
                let lx = self.label("cend");
                self.branch_on(c, &lt, &le, "")?;
                self.block(&lt);
                let x = self.expr(a)?;
                let xb = self.cur.clone();
                self.br(&lx);
                self.block(&le);
                let y = self.expr(b)?;
                let yb = self.cur.clone();
                self.br(&lx);
                self.block(&lx);
                let r = self.tmp();
                let t = self.lt(&ty);
                let fm = if ty.elem().is_some_and(is_float) {
                    format!("{FM} ")
                } else {
                    String::new()
                };
                self.emit(format!(
                    "{r} = phi {fm}{t} [ {}, %{xb} ], [ {}, %{yb} ]",
                    x.r, y.r
                ));
                Ok(V { r, ty })
            }
            ExprKind::Convert(x) => {
                let v = self.expr(x)?;
                self.convert(v, &ty)
            }
            ExprKind::Splat(x) => {
                let v = self.expr(x)?;
                Ok(self.splat(v, &ty))
            }
            ExprKind::Bitcast(x) => {
                let v = self.expr(x)?;
                let r = self.tmp();
                self.emit(format!(
                    "{r} = bitcast {} {} to {}",
                    self.lt(&v.ty),
                    v.r,
                    self.lt(&ty)
                ));
                Ok(V { r, ty })
            }
            ExprKind::PtrCast(x) | ExprKind::TexCast(x) => {
                let v = self.expr(x)?;
                let r = self.tmp();
                let op = if matches!(v.ty, Type::Scalar(_)) {
                    "inttoptr"
                } else {
                    "bitcast"
                };
                self.emit(format!(
                    "{r} = {op} {} {} to {}",
                    self.lt(&v.ty),
                    v.r,
                    self.lt(&ty)
                ));
                Ok(V { r, ty })
            }
            ExprKind::Vector(parts) => {
                let s = ty.elem().ok_or_else(|| Error::invalid("vector type"))?;
                let t = self.lt(&ty);
                let et = scalar_ty(s, false);
                let mut cur = "undef".to_string();
                let mut k = 0;
                for p in parts {
                    let v = self.expr(p)?;
                    match v.ty {
                        Type::Scalar(_) => {
                            let r = self.tmp();
                            self.emit(format!(
                                "{r} = insertelement {t} {cur}, {et} {}, i32 {k}",
                                v.r
                            ));
                            cur = r;
                            k += 1;
                        }
                        Type::Vector(_, n) => {
                            let vt = self.lt(&v.ty);
                            for j in 0..n {
                                let x = self.tmp();
                                self.emit(format!("{x} = extractelement {vt} {}, i32 {j}", v.r));
                                let r = self.tmp();
                                self.emit(format!(
                                    "{r} = insertelement {t} {cur}, {et} {x}, i32 {k}"
                                ));
                                cur = r;
                                k += 1;
                            }
                        }
                        _ => return Err(Error::invalid("vector part")),
                    }
                }
                Ok(V { r: cur, ty })
            }
            ExprKind::Swizzle(x, comps) => {
                let v = self.expr(x)?;
                Ok(self.shuffle(&v, comps))
            }
            ExprKind::Assign(p, x) => {
                let v = self.expr(x)?;
                self.store(p, &v)?;
                Ok(v)
            }
            ExprKind::CompoundAssign {
                op,
                place,
                value,
                op_ty,
            } => {
                let contract = self.contract
                    && matches!(op, BinOp::Add | BinOp::Sub)
                    && op_ty.elem().is_some_and(is_float);
                let r = match &value.kind {
                    ExprKind::Binary(BinOp::Mul, m1, m2) if contract => {
                        // `x += a*b` contracts like `x + a*b` (right operand first, as clang evaluates it).
                        let a = self.expr(m1)?;
                        let b = self.expr(m2)?;
                        let cur = self.load(place)?;
                        let x = self.convert(cur, op_ty)?;
                        let t = self.lt(op_ty);
                        let mut ar = a.r;
                        if *op == BinOp::Sub {
                            let n = self.tmp();
                            self.emit(format!("{n} = fneg {FM} {t} {ar}"));
                            ar = n;
                        }
                        let name = format!("llvm.fmuladd.{}", suffix(op_ty));
                        let d = self.cx.declare(
                            &name,
                            &format!("{t} @{name}({t}, {t}, {t})"),
                            crate::Attrs::Speculatable,
                        );
                        let r = self.tmp();
                        self.emit(format!(
                            "{r} = call {FM} {t} @{d}({t} {ar}, {t} {}, {t} {}) #{}",
                            b.r,
                            x.r,
                            crate::Attrs::NoUnwind.group()
                        ));
                        V {
                            r,
                            ty: op_ty.clone(),
                        }
                    }
                    _ => {
                        let y = self.expr(value)?;
                        let cur = self.load(place)?;
                        let x = self.convert(cur, op_ty)?;
                        self.binop(*op, x, y, op_ty)?
                    }
                };
                let back = self.convert(r, &place.ty.unpacked())?;
                self.store(place, &back)?;
                Ok(back)
            }
            ExprKind::Call(fid, args) => {
                let mut ops = Vec::with_capacity(args.len());
                // templates are instantiated in Sema's order (arguments parsed first), helpers in
                // codegen's (callee created before the arguments)
                let template = self.cx.funcs_template[*fid];
                if !template {
                    self.cx.reference(*fid);
                }
                let callee = &self.cx.funcs_sig[*fid];
                let ptys = callee.1.clone();
                let name = callee.0.clone();
                for (a, pt) in args.iter().zip(ptys.iter()) {
                    match a {
                        Arg::Value(x) => {
                            let v = self.expr(x)?;
                            if let Type::Struct(sid) = &v.ty {
                                // expanded like the callee's parameters
                                let st = self.mt(&v.ty);
                                let fields = self.cx.types.structs[*sid].fields.clone();
                                for (j, fd) in fields.iter().enumerate() {
                                    let e = self.tmp();
                                    self.emit(format!("{e} = extractvalue {st} {}, {j}", v.r));
                                    ops.push(format!("{} {e}", self.mt(&fd.ty.unpacked())));
                                }
                            } else {
                                ops.push(format!("{} noundef {}", self.lt(&v.ty), v.r));
                            }
                        }
                        Arg::Ref(p) => {
                            let ptr = self.ref_arg(p, pt)?;
                            let l = self.cx.types.layout(&p.ty);
                            ops.push(format!(
                                "{pt} noundef align {} dereferenceable({}) {ptr}",
                                l.align, l.size
                            ));
                        }
                    }
                }
                if template {
                    self.cx.reference(*fid);
                }
                let rt = self.lt(&ty);
                if ty == Type::Void {
                    self.emit(format!("call void @{name}({}) #11", ops.join(", ")));
                    return Ok(V {
                        r: "undef".into(),
                        ty,
                    });
                }
                let r = self.tmp();
                let fm = if ty.elem().is_some_and(is_float) {
                    format!("{FM} ")
                } else {
                    String::new()
                };
                let za = if ty == Type::BOOL { "zeroext " } else { "" };
                self.emit(format!(
                    "{r} = call {fm}{za}{rt} @{name}({}) #11",
                    ops.join(", ")
                ));
                Ok(V { r, ty })
            }
            ExprKind::Builtin(name, args) => {
                let slot = self.cx.reserve();
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a)?);
                }
                self.cx.fill(slot);
                crate::intrin::builtin(self, name, vals, &ty)
            }
            ExprKind::Texture { tex, tex_ty, op } => {
                let slot = self.cx.reserve();
                crate::intrin::texture(self, tex, *tex_ty, op, &ty, slot)
            }
            ExprKind::FnConst(id) => {
                let (shadow, _) = self.cx.fconst_names[*id].clone();
                let fc_ty = self.cx.fconst_tys[*id].clone();
                let a = Addr {
                    ptr: shadow,
                    ty: fc_ty,
                    space: 2,
                };
                Ok(self.load_from(&a))
            }
            ExprKind::PtrOffset(p, off) => {
                let pv = self.expr(p)?;
                let o = self.expr(off)?;
                let i = self.index_i64(o);
                let Type::Ptr(sp, inner) = &pv.ty else {
                    return Err(Error::invalid("ptr offset"));
                };
                let et = self.mt(inner);
                let ss = space_suffix(space_num(*sp));
                let r = self.tmp();
                self.emit(format!(
                    "{r} = getelementptr inbounds {et}, {et}{ss}* {}, i64 {i}",
                    pv.r
                ));
                Ok(V { r, ty })
            }
            ExprKind::StructInit(vals) => {
                let t = self.mt(&ty);
                let Type::Struct(sid) = ty else {
                    return Err(Error::invalid("struct init"));
                };
                let ftys: Vec<Type> = self.cx.types.structs[sid]
                    .fields
                    .iter()
                    .map(|f| f.ty.clone())
                    .collect();
                let mut cur = "zeroinitializer".to_string();
                for (k, (x, ft)) in vals.iter().zip(ftys.iter()).enumerate() {
                    let v = self.expr(x)?;
                    let m = self.mem_form(v, ft);
                    let r = self.tmp();
                    self.emit(format!(
                        "{r} = insertvalue {t} {cur}, {} {}, {k}",
                        self.mt(ft),
                        m
                    ));
                    cur = r;
                }
                Ok(V { r: cur, ty })
            }
            ExprKind::Seq(stmts, x) => {
                // the expanded body of the dm_bs template: what it references comes with the instantiation
                self.cx.in_template += 1;
                self.seq_depth += 1;
                let r = self.stmts(stmts).and_then(|_| self.expr(x));
                self.seq_depth -= 1;
                self.cx.in_template -= 1;
                r
            }
        }
    }

    /// Contracts `a*b + c`, `c + a*b`, `a*b - c` and `c - a*b` into `llvm.fmuladd`, as clang does
    /// under `-ffp-contract=on` (the multiply must be an operand of the same expression).
    fn try_fmuladd(&mut self, op: BinOp, a: &Expr, b: &Expr, ty: &Type) -> R<Option<V>> {
        if !self.contract
            || !matches!(op, BinOp::Add | BinOp::Sub)
            || !ty.elem().is_some_and(is_float)
        {
            return Ok(None);
        }
        let is_mul = |e: &Expr| matches!(e.kind, ExprKind::Binary(BinOp::Mul, ..));
        let (lhs_mul, rhs_mul) = (is_mul(a), is_mul(b));
        if !lhs_mul && !rhs_mul {
            return Ok(None);
        }
        let t = self.lt(ty);
        let (m1, m2, addend, neg_mul, neg_add) = if lhs_mul {
            let ExprKind::Binary(_, x, y) = &a.kind else {
                return Ok(None);
            };
            let x = self.expr(x)?;
            let y = self.expr(y)?;
            let c = self.expr(b)?;
            (x, y, c, false, op == BinOp::Sub)
        } else {
            let c = self.expr(a)?;
            let ExprKind::Binary(_, x, y) = &b.kind else {
                return Ok(None);
            };
            let x = self.expr(x)?;
            let y = self.expr(y)?;
            (x, y, c, op == BinOp::Sub, false)
        };
        let mut m1r = m1.r;
        if neg_mul {
            let r = self.tmp();
            self.emit(format!("{r} = fneg {FM} {t} {m1r}"));
            m1r = r;
        }
        let mut cr = addend.r;
        if neg_add {
            let r = self.tmp();
            self.emit(format!("{r} = fneg {FM} {t} {cr}"));
            cr = r;
        }
        let sfx = suffix(ty);
        let name = format!("llvm.fmuladd.{sfx}");
        let d = self.cx.declare(
            &name,
            &format!("{t} @{name}({t}, {t}, {t})"),
            crate::Attrs::Speculatable,
        );
        let r = self.tmp();
        self.emit(format!(
            "{r} = call {FM} {t} @{d}({t} {m1r}, {t} {}, {t} {cr}) #{}",
            m2.r,
            crate::Attrs::NoUnwind.group()
        ));
        Ok(Some(V { r, ty: ty.clone() }))
    }

    /// A pointer for a by-reference argument, copies into a thread temporary when the address
    /// spaces differ or the place has no address.
    fn ref_arg(&mut self, p: &Place, pty: &str) -> R<String> {
        let want_space = if pty.contains("addrspace(") {
            pty.split("addrspace(")
                .nth(1)
                .and_then(|s| s.split(')').next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        } else {
            0
        };
        if let Some(a) = self.addr(p)? {
            if a.space == want_space {
                return Ok(a.ptr);
            }
            let v = self.load_from(&a);
            let slot = self.alloca(&p.ty);
            self.store_to(
                &Addr {
                    ptr: slot.clone(),
                    ty: p.ty.clone(),
                    space: 0,
                },
                &v,
            );
            return Ok(slot);
        }
        let v = self.load(p)?;
        let slot = self.alloca(&p.ty);
        self.store_to(
            &Addr {
                ptr: slot.clone(),
                ty: p.ty.clone(),
                space: 0,
            },
            &v,
        );
        Ok(slot)
    }

    /// Value form to the memory form of a field type (bool to i8).
    pub fn mem_form(&mut self, v: V, ft: &Type) -> String {
        if ft.elem() == Some(Scalar::Bool) && matches!(ft, Type::Scalar(_) | Type::Vector(..)) {
            let r = self.tmp();
            self.emit(format!(
                "{r} = zext {} {} to {}",
                self.lt(&v.ty),
                v.r,
                self.mt(ft)
            ));
            return r;
        }
        v.r
    }

    /// Memory form to value form (i8 to bool).
    pub fn from_mem(&mut self, r: String, ft: &Type) -> V {
        if ft.elem() == Some(Scalar::Bool) && matches!(ft, Type::Scalar(_) | Type::Vector(..)) {
            let t = self.tmp();
            self.emit(format!(
                "{t} = trunc {} {r} to {}",
                self.mt(ft),
                self.lt(ft)
            ));
            return V {
                r: t,
                ty: ft.unpacked(),
            };
        }
        V {
            r,
            ty: ft.unpacked(),
        }
    }

    fn binop(&mut self, op: BinOp, x: V, y: V, ty: &Type) -> R<V> {
        let s = ty
            .elem()
            .ok_or_else(|| Error::invalid(format!("binary on {ty}")))?;
        let t = self.lt(ty);
        let r = self.tmp();
        if is_float(s) {
            let o = match op {
                BinOp::Add => "fadd",
                BinOp::Sub => "fsub",
                BinOp::Mul => "fmul",
                BinOp::Div => "fdiv",
                BinOp::Rem => "frem",
                _ => return Err(Error::invalid("bit op on float")),
            };
            self.emit(format!("{r} = {o} {FM} {t} {}, {}", x.r, y.r));
            return Ok(V { r, ty: ty.clone() });
        }
        let sg = is_signed(s);
        let o = match op {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => {
                if sg {
                    "sdiv"
                } else {
                    "udiv"
                }
            }
            BinOp::Rem => {
                if sg {
                    "srem"
                } else {
                    "urem"
                }
            }
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Xor => "xor",
            BinOp::Shl => "shl",
            BinOp::Shr => {
                if sg {
                    "ashr"
                } else {
                    "lshr"
                }
            }
        };
        let mut yv = y.r.clone();
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            // Metal masks the shift amount to the bit width.
            let bits = scalar_bits(s) as i64;
            match y.r.parse::<i64>() {
                Ok(n) => yv = (n & (bits - 1)).to_string(),
                Err(_) => {
                    let m = self.tmp();
                    self.emit(format!(
                        "{m} = and {t} {}, {}",
                        y.r,
                        splat_text(ty, &(bits - 1).to_string())
                    ));
                    yv = m;
                }
            }
        }
        // Signed arithmetic wraps (`-fwrapv`), so the compiler writes no `nsw` on signed add, sub, mul and negation.
        self.emit(format!("{r} = {o} {t} {}, {yv}", x.r));
        Ok(V { r, ty: ty.clone() })
    }

    /// Broadcasts a scalar to a vector type.
    pub fn splat(&mut self, v: V, ty: &Type) -> V {
        let Type::Vector(s, n) = ty else { return v };
        let t = self.lt(ty);
        let a = self.tmp();
        self.emit(format!(
            "{a} = insertelement {t} poison, {} {}, i32 0",
            scalar_ty(*s, false),
            v.r
        ));
        let r = self.tmp();
        self.emit(format!(
            "{r} = shufflevector {t} {a}, {t} poison, <{n} x i32> zeroinitializer"
        ));
        V { r, ty: ty.clone() }
    }

    /// Numeric conversion between scalars or vectors of the same width.
    pub fn convert(&mut self, v: V, to: &Type) -> R<V> {
        let to = to.unpacked();
        if v.ty == to {
            return Ok(v);
        }
        let (Some(fs), Some(ts)) = (v.ty.elem(), to.elem()) else {
            return Err(Error::invalid(format!("convert {} -> {}", v.ty, to)));
        };
        if v.ty.width() != to.width() {
            return Err(Error::invalid(format!("convert width {} -> {}", v.ty, to)));
        }
        let ft = self.lt(&v.ty);
        let tt = self.lt(&to);
        let r = self.tmp();
        if ts == Scalar::Bool {
            if is_float(fs) {
                self.emit(format!(
                    "{r} = fcmp {FM} une {ft} {}, {}",
                    v.r,
                    zero_text(&self.cx.types, &v.ty, false)
                ));
            } else {
                self.emit(format!(
                    "{r} = icmp ne {ft} {}, {}",
                    v.r,
                    zero_text(&self.cx.types, &v.ty, false)
                ));
            }
            return Ok(V { r, ty: to });
        }
        if fs == Scalar::Bool && !is_float(ts) {
            self.emit(format!("{r} = zext {ft} {} to {tt}", v.r));
            return Ok(V { r, ty: to });
        }
        let vector = matches!(to, Type::Vector(..));
        if vector
            && fs != Scalar::Bool
            && (is_float(fs) || is_float(ts) || scalar_bits(fs) != scalar_bits(ts))
        {
            // clang converts vectors through air.convert, also float<->float and integer width changes
        } else if is_float(fs) && is_float(ts) {
            let o = if ts.size() > fs.size() {
                "fpext"
            } else {
                "fptrunc"
            };
            self.emit(format!("{r} = {o} {ft} {} to {tt}", v.r));
            return Ok(V { r, ty: to });
        }
        if (!vector || scalar_bits(fs) == scalar_bits(ts)) && !is_float(fs) && !is_float(ts) {
            let (fb, tb) = (scalar_bits(fs), scalar_bits(ts));
            if fb == tb {
                return Ok(V { r: v.r, ty: to });
            }
            let o = if tb < fb {
                "trunc"
            } else if is_signed(fs) {
                "sext"
            } else {
                "zext"
            };
            self.emit(format!("{r} = {o} {ft} {} to {tt}", v.r));
            return Ok(V { r, ty: to });
        }
        // int <-> float through air.convert
        let name = format!(
            "air.convert.{}.{}.{}.{}",
            kind_letter(ts),
            suffix(&to),
            kind_letter(fs),
            suffix(&v.ty)
        );
        let d = self.cx.declare(
            &name,
            &format!("{tt} @{name}({ft})"),
            crate::Attrs::ReadNone,
        );
        let fm = if is_float(ts) {
            format!("{FM} ")
        } else {
            String::new()
        };
        self.emit(format!(
            "{r} = call {fm}{tt} @{d}({ft} {}) #{}",
            v.r,
            crate::Attrs::ReadNone.group()
        ));
        Ok(V { r, ty: to })
    }

    /// Folds a constant expression (for constant array initialisers).
    pub fn fold(e: &Expr) -> Option<CVal> {
        Some(match &e.kind {
            ExprKind::Int(v) => CVal::Int(*v),
            ExprKind::Float(v) => CVal::Float(*v),
            ExprKind::Zero => CVal::Zero,
            ExprKind::Unary(UnOp::Neg, x) => match FnGen::fold(x)? {
                CVal::Int(v) => CVal::Int(-v),
                CVal::Float(v) => CVal::Float(-v),
                _ => return None,
            },
            ExprKind::Convert(x) => {
                let v = FnGen::fold(x)?;
                let s = e.ty.elem()?;
                match v {
                    CVal::Int(i) if is_float(s) => CVal::Float(i as f64),
                    CVal::Float(f) if !is_float(s) => CVal::Int(f as i128),
                    v => v,
                }
            }
            ExprKind::Splat(x) => {
                let v = FnGen::fold(x)?;
                CVal::Vec(vec![v; e.ty.width()? as usize])
            }
            ExprKind::Vector(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match FnGen::fold(p)? {
                        CVal::Vec(v) => out.extend(v),
                        v => out.push(v),
                    }
                }
                CVal::Vec(out)
            }
            _ => return None,
        })
    }

    /// Spells a folded constant of a type in memory form.
    pub fn const_mem(&self, v: &CVal, t: &Type) -> String {
        const_text(&self.cx.types, v, t, true)
    }

    /// The thread address space number.
    pub fn thread() -> u32 {
        space_num(Space::Thread)
    }
}

/// clang's `commonAlignment(base, offset)`.
fn common_align(base: u32, off: u32) -> u32 {
    if off == 0 {
        base
    } else {
        base.min(1 << off.trailing_zeros())
    }
}

/// `<n x T> <v, v, ...>` constant operand text for a splat (scalar: the value itself).
fn splat_text(ty: &Type, v: &str) -> String {
    match ty {
        Type::Vector(s, n) => {
            let et = scalar_ty(*s, false);
            let items: Vec<String> = (0..*n).map(|_| format!("{et} {v}")).collect();
            format!("<{}>", items.join(", "))
        }
        _ => v.to_string(),
    }
}
