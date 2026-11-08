//! The declarations of the locals of a function.
//!
//! The structured program has one local for every value of the shader, often thousands. Writing each as
//! `float v1 = 0.0f;` on a line of its own made these lines about a third of the text of a big shader. Two things
//! make them short:
//!
//! - A local that is assigned once and is only read after that assignment, inside the block of the assignment, never
//!   shows its initial value. It is declared without one.
//! - Locals of one type and one initial value share a declaration: `float v1 = 0.0f, v2 = 0.0f;`.
//!
//! Both leave the behaviour as it was. The order of the declarations does not matter, because an initial value
//! is a literal.

use crate::ir::{Inst, OperandType};
use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind};
use std::collections::{BTreeMap, BTreeSet};

/// Most names in one declaration.
const PER_LINE: usize = 16;

/// What the walk learns about a local.
#[derive(Default)]
struct Seen {
    /// The positions of the assignments, with the block that holds each.
    writes: Vec<(u32, A)>,
    /// The positions of the reads.
    reads: Vec<u32>,
    /// The local is an argument of a call, so the callee may write it.
    call_arg: bool,
}

struct Walk<'a> {
    info: &'a StructInfo,
    skip: &'a BTreeSet<A>,
    pos: u32,
    seen: BTreeMap<A, Seen>,
    /// The last position inside each block.
    end: BTreeMap<A, u32>,
}

impl Walk<'_> {
    fn tick(&mut self) -> u32 {
        self.pos += 1;
        self.pos
    }

    fn is_local(&self, a: A) -> bool {
        matches!(
            self.info.ast.kind(a),
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            }
        )
    }

    /// Visit an expression. Its reads come in the order of the text.
    fn expr(&mut self, a: A, call: bool) {
        if self.is_local(a) {
            let p = self.tick();
            let s = self.seen.entry(a).or_default();
            s.reads.push(p);
            s.call_arg |= call;
            return;
        }
        if let Kind::Operation(o) = self.info.ast.kind(a) {
            let call = o.inst.0 & Inst::MASK == Inst::Call.0;
            for &s in &o.sources {
                self.expr(s, call);
            }
        }
    }

    /// Visit the statements of a block. A condition comes first, as the safe order for every kind of block.
    fn block(&mut self, b: A) {
        self.tick();
        if let Some(c) = self.info.ast.cond(b) {
            self.expr(c, false);
        }
        for n in self.info.ast.children(b) {
            if self.skip.contains(&n) {
                continue;
            }
            match self.info.ast.kind(n) {
                Kind::Block(_) => self.block(n),
                Kind::Assign { dest, src } => {
                    self.expr(*src, false);
                    let p = self.tick();
                    if self.is_local(*dest) {
                        self.seen.entry(*dest).or_default().writes.push((p, b));
                    }
                }
                Kind::Operation(_) => self.expr(n, false),
                _ => {}
            }
        }
        let p = self.tick();
        self.end.insert(b, p);
    }
}

/// The locals of function `f` that need no initial value.
pub(super) fn uninit(info: &StructInfo, f: usize, skip: &BTreeSet<A>) -> BTreeSet<A> {
    let Some(func) = info.functions.get(f) else {
        return BTreeSet::new();
    };
    let mut w = Walk {
        info,
        skip,
        pos: 0,
        seen: BTreeMap::new(),
        end: BTreeMap::new(),
    };
    w.block(func.main_block);
    let mut out = BTreeSet::new();
    for (&l, s) in &w.seen {
        let [(at, blk)] = s.writes[..] else {
            continue;
        };
        let end = w.end.get(&blk).copied().unwrap_or(0);
        if !s.call_arg && s.reads.iter().all(|&r| r > at && r < end) {
            out.insert(l);
        }
    }
    // A local that is never written keeps its initial value; this loop only lists locals the walk saw written.
    out
}

/// The declaration lines of `locals`, each as (type, name, initial value). A name in `bare` has no initial value.
/// With `plain`, every local has a line of its own and its initial value.
pub(super) fn lines(
    locals: &[(String, A, String)],
    bare: &BTreeSet<A>,
    plain: bool,
) -> Vec<String> {
    if plain {
        return locals
            .iter()
            .map(|(ty, l, init)| format!("{ty} v{} = {init};", l.0))
            .collect();
    }
    // Group by the type and the initial value, in the order of first appearance.
    let mut order: Vec<(String, Option<String>)> = Vec::new();
    let mut groups: BTreeMap<(String, Option<String>), Vec<String>> = BTreeMap::new();
    for (ty, l, init) in locals {
        let key = (ty.clone(), (!bare.contains(l)).then(|| init.clone()));
        let g = groups.entry(key.clone()).or_default();
        if g.is_empty() {
            order.push(key);
        }
        g.push(format!("v{}", l.0));
    }
    let mut out = Vec::new();
    for key in order {
        let (ty, init) = &key;
        for chunk in groups[&key].chunks(PER_LINE) {
            let names: Vec<String> = chunk
                .iter()
                .map(|n| match init {
                    Some(i) => format!("{n} = {i}"),
                    None => n.clone(),
                })
                .collect();
            out.push(format!("{ty} {};", names.join(", ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(ty: &str, n: u32, init: &str) -> (String, A, String) {
        (ty.to_string(), A(n), init.to_string())
    }

    #[test]
    fn locals_of_one_kind_share_a_line() {
        let locals = [
            l("float", 1, "0.0f"),
            l("int", 2, "0"),
            l("float", 3, "0.0f"),
        ];
        let got = lines(&locals, &BTreeSet::new(), false);
        assert_eq!(got, ["float v1 = 0.0f, v3 = 0.0f;", "int v2 = 0;"]);
    }

    #[test]
    fn a_bare_local_has_no_initial_value() {
        let locals = [l("float", 1, "0.0f"), l("float", 3, "0.0f")];
        let bare = BTreeSet::from([A(3)]);
        let got = lines(&locals, &bare, false);
        assert_eq!(got, ["float v1 = 0.0f;", "float v3;"]);
    }

    #[test]
    fn a_long_run_is_cut() {
        let locals: Vec<_> = (0..PER_LINE as u32 + 1)
            .map(|n| l("bool", n, "false"))
            .collect();
        let bare: BTreeSet<A> = locals.iter().map(|x| x.1).collect();
        let got = lines(&locals, &bare, false);
        assert_eq!(got.len(), 2);
        assert!(got[1].ends_with("v16;"));
    }

    use super::super::gaps_tests::msl;
    use super::super::tests::{B, compute};
    use crate::ir::{Inst, IoVariable, StorageKind};
    use crate::structured::ast::BlockType;
    use crate::structured::types::AggType;

    /// `local = a + 1` as a statement of `block`.
    fn add_in(b: &mut B, block: A, a: A) -> A {
        let one = b.k(1);
        let op = b.op(Inst::Add, StorageKind::None, 0, vec![a, one]);
        let l = b.local(AggType::U32);
        let asg = b.s.info.ast.new_assign(l, op);
        b.s.info.ast.add(block, asg).expect("add");
        l
    }

    /// The declaration lines of a function text (the ones that start with a type).
    fn decl_lines(m: &str) -> Vec<&str> {
        m.lines()
            .filter(|l| {
                ["    uint ", "    int ", "    bool "]
                    .iter()
                    .any(|t| l.starts_with(t))
            })
            .filter(|l| !l.contains('(') && l.ends_with(';'))
            .collect()
    }

    /// True when `v` is declared with an initial value.
    fn has_init(m: &str, v: A) -> bool {
        let name = format!("v{} = ", v.0);
        decl_lines(m).iter().any(|l| l.contains(&name))
    }

    fn declared(m: &str, v: A) -> bool {
        let name = format!("v{}", v.0);
        decl_lines(m)
            .iter()
            .any(|l| l.split([' ', ',', ';']).any(|w| w == name))
    }

    /// `t = thread id; x = 0; if (c) { x = t + 1; w = x + 1 } y = x + 1; z = t + 1; if (c) { u = z + 1 }`.
    fn sample() -> (B, [A; 5]) {
        let mut b = compute();
        let t = b.load_io(
            StorageKind::Input,
            IoVariable::ThreadId,
            None,
            0,
            AggType::U32,
        );
        let zero = b.k(0);
        let c_op = b.op(Inst::CompareNotEqual, StorageKind::None, 0, vec![t, zero]);
        let c = b.def(AggType::BOOL, c_op);
        let blk = b.s.info.ast.new_block(BlockType::If, Some(c));
        let x = add_in(&mut b, blk, t);
        let w = add_in(&mut b, blk, x);
        b.put(blk);
        let main = b.main;
        let _y = add_in(&mut b, main, x);
        let z = add_in(&mut b, main, t);
        let blk2 = b.s.info.ast.new_block(BlockType::If, Some(c));
        let _u = add_in(&mut b, blk2, z);
        b.put(blk2);
        (b, [t, x, w, z, c])
    }

    #[test]
    fn a_local_read_only_after_its_one_assignment_has_no_initial_value() {
        let (b, [t, _, w, z, c]) = sample();
        let m = msl(&b);
        for v in [t, w, z, c] {
            assert!(declared(&m, v) && !has_init(&m, v), "v{}\n{m}", v.0);
        }
    }

    #[test]
    fn a_local_read_outside_its_block_keeps_its_initial_value() {
        let (b, [_, x, ..]) = sample();
        let m = msl(&b);
        assert!(has_init(&m, x), "{m}");
    }

    #[test]
    fn a_local_read_before_its_assignment_keeps_its_initial_value() {
        let mut b = compute();
        let main = b.main;
        let v = b.local(AggType::U32);
        let one = b.k(1);
        let op = b.op(Inst::Add, StorageKind::None, 0, vec![v, one]);
        let asg = b.s.info.ast.new_assign(v, op);
        b.s.info.ast.add(main, asg).expect("add");
        assert!(has_init(&msl(&b), v));
    }

    #[test]
    fn a_local_with_two_assignments_keeps_its_initial_value() {
        let mut b = compute();
        let main = b.main;
        let v = b.local(AggType::U32);
        for k in [1, 2] {
            let c = b.k(k);
            let asg = b.s.info.ast.new_assign(v, c);
            b.s.info.ast.add(main, asg).expect("add");
        }
        assert!(has_init(&msl(&b), v));
    }

    #[test]
    fn the_plain_form_keeps_one_declaration_per_line() {
        let locals = [l("float", 1, "0.0f"), l("float", 2, "0.0f")];
        let got = lines(&locals, &BTreeSet::from([A(1)]), true);
        assert_eq!(got, ["float v1 = 0.0f;", "float v2 = 0.0f;"]);
    }
}
