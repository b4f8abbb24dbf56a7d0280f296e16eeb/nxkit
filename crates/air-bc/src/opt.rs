//! A light optimiser on the module tree, so raw libraries stay small: promotion of scalar and vector allocas to SSA
//! values (mem2reg, with phi placement on the dominance frontier) and dead instruction elimination. Semantics are
//! unchanged, the GPU compiler optimises again.

use crate::ast::*;
use crate::fx::{HashMap, HashSet};

fn vals_mut(op: &mut Op) -> Vec<&mut Val> {
    match op {
        Op::Alloca(..) | Op::Br(_) | Op::Unreachable => vec![],
        Op::Load(_, p, _, _) => vec![&mut p.1],
        Op::Store(a, b, _) => vec![&mut a.1, &mut b.1],
        Op::Gep(_, _, p, idx) => std::iter::once(&mut p.1)
            .chain(idx.iter_mut().map(|x| &mut x.1))
            .collect(),
        Op::Cast(_, v, _) => vec![&mut v.1],
        Op::Bin(_, _, _, a, b) | Op::Cmp(_, _, _, _, a, b) => vec![a, b],
        Op::FNeg(_, _, a) | Op::Freeze(_, a) => vec![a],
        Op::Select(_, a, b, c) | Op::InsertElt(a, b, c) | Op::Shuffle(a, b, c) => {
            vec![&mut a.1, &mut b.1, &mut c.1]
        }
        Op::Phi(_, _, ins) => ins.iter_mut().map(|x| &mut x.0).collect(),
        Op::CondBr(c, ..) => vec![c],
        Op::Switch(_, v, _, cases) => std::iter::once(v)
            .chain(cases.iter_mut().map(|x| &mut x.0))
            .collect(),
        Op::Ret(v) => v.iter_mut().map(|x| &mut x.1).collect(),
        Op::Call(_, _, _, _, args, _) => args.iter_mut().map(|x| &mut x.2).collect(),
        Op::ExtractElt(a, b) => vec![&mut a.1, &mut b.1],
        Op::ExtractVal(a, _) => vec![&mut a.1],
        Op::InsertVal(a, b, _) => vec![&mut a.1, &mut b.1],
    }
}

fn vals(op: &Op) -> Vec<&Val> {
    match op {
        Op::Alloca(..) | Op::Br(_) | Op::Unreachable => vec![],
        Op::Load(_, p, _, _) => vec![&p.1],
        Op::Store(a, b, _) => vec![&a.1, &b.1],
        Op::Gep(_, _, p, idx) => std::iter::once(&p.1)
            .chain(idx.iter().map(|x| &x.1))
            .collect(),
        Op::Cast(_, v, _) => vec![&v.1],
        Op::Bin(_, _, _, a, b) | Op::Cmp(_, _, _, _, a, b) => vec![a, b],
        Op::FNeg(_, _, a) | Op::Freeze(_, a) => vec![a],
        Op::Select(_, a, b, c) | Op::InsertElt(a, b, c) | Op::Shuffle(a, b, c) => {
            vec![&a.1, &b.1, &c.1]
        }
        Op::Phi(_, _, ins) => ins.iter().map(|x| &x.0).collect(),
        Op::CondBr(c, ..) => vec![c],
        Op::Switch(_, v, _, cases) => std::iter::once(v)
            .chain(cases.iter().map(|x| &x.0))
            .collect(),
        Op::Ret(v) => v.iter().map(|x| &x.1).collect(),
        Op::Call(_, _, _, _, args, _) => args.iter().map(|x| &x.2).collect(),
        Op::ExtractElt(a, b) => vec![&a.1, &b.1],
        Op::ExtractVal(a, _) => vec![&a.1],
        Op::InsertVal(a, b, _) => vec![&a.1, &b.1],
    }
}

fn locals(op: &Op, out: &mut Vec<String>) {
    for v in vals(op) {
        collect(v, out);
    }
}

fn collect(v: &Val, out: &mut Vec<String>) {
    match v {
        Val::Local(n) => out.push(n.clone()),
        Val::Aggregate(items) => items.iter().for_each(|(_, x)| collect(x, out)),
        Val::Cast(_, inner, _) => collect(&inner.1, out),
        _ => {}
    }
}

fn successors(insts: &[Inst]) -> Vec<String> {
    match insts.last().map(|i| &i.op) {
        Some(Op::Br(l)) => vec![l.clone()],
        Some(Op::CondBr(_, a, b)) => vec![a.clone(), b.clone()],
        Some(Op::Switch(_, _, d, cases)) => std::iter::once(d.clone())
            .chain(cases.iter().map(|c| c.1.clone()))
            .collect(),
        _ => vec![],
    }
}

fn promotable(t: &Ty) -> bool {
    matches!(
        t,
        Ty::Int(_) | Ty::Half | Ty::Float | Ty::Double | Ty::Vector(..) | Ty::Ptr(..)
    )
}

fn is_lifetime(op: &Op) -> bool {
    matches!(op, Op::Call(_, _, _, c, _, _) if c.starts_with("llvm.lifetime."))
}

/// mem2reg and dead code elimination on every defined function.
pub fn optimize(m: &mut Module) {
    for f in &mut m.functions {
        if !f.blocks.is_empty() {
            mem2reg(f);
            trivial_phis(f);
            cse(f);
            dce(f);
        }
    }
}

fn mem2reg(f: &mut Function) {
    // candidates: allocas of first-class types used only by whole loads and stores (as the address), plus bitcasts
    // whose only uses are lifetime markers
    let mut allocas: HashMap<String, Ty> = HashMap::default();
    for (_, insts) in &f.blocks {
        for i in insts {
            if let (Some(r), Op::Alloca(t, _)) = (&i.res, &i.op) {
                if promotable(t) {
                    allocas.insert(r.clone(), t.clone());
                }
            }
        }
    }
    if allocas.is_empty() {
        return;
    }
    let mut marker_casts: HashMap<String, String> = HashMap::default();
    for (_, insts) in &f.blocks {
        for i in insts {
            if let (Some(r), Op::Cast(k, (_, Val::Local(src)), _)) = (&i.res, &i.op) {
                if k == "bitcast" && allocas.contains_key(src) {
                    marker_casts.insert(r.clone(), src.clone());
                }
            }
        }
    }
    let mut bad: HashSet<String> = HashSet::default();
    for (_, insts) in &f.blocks {
        for i in insts {
            match &i.op {
                Op::Load(t, (_, Val::Local(p)), _, volatile) if allocas.contains_key(p) => {
                    if *volatile || &allocas[p] != t {
                        bad.insert(p.clone());
                    }
                }
                Op::Store((t, v), (_, Val::Local(p)), _) if allocas.contains_key(p) => {
                    if &allocas[p] != t {
                        bad.insert(p.clone());
                    }
                    let mut u = Vec::new();
                    collect(v, &mut u);
                    bad.extend(
                        u.into_iter()
                            .filter(|x| allocas.contains_key(x) || marker_casts.contains_key(x))
                            .map(|x| marker_casts.get(&x).cloned().unwrap_or(x)),
                    );
                }
                Op::Cast(k, (_, Val::Local(src)), _)
                    if k == "bitcast" && allocas.contains_key(src) => {}
                op if is_lifetime(op) => {}
                op => {
                    let mut u = Vec::new();
                    locals(op, &mut u);
                    for x in u {
                        if allocas.contains_key(&x) {
                            bad.insert(x);
                        } else if let Some(a) = marker_casts.get(&x) {
                            bad.insert(a.clone());
                        }
                    }
                }
            }
        }
    }
    // a cast of a candidate used by anything but lifetime markers disqualifies it (checked above via `op` arm)
    allocas.retain(|k, _| !bad.contains(k));
    if allocas.is_empty() {
        return;
    }
    // CFG, reverse post order, dominators (Cooper, Harvey, Kennedy)
    let n = f.blocks.len();
    let idx: HashMap<String, usize> = f
        .blocks
        .iter()
        .enumerate()
        .map(|(k, b)| (b.0.clone(), k))
        .collect();
    let succ: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|b| {
            successors(&b.1)
                .iter()
                .filter_map(|l| idx.get(l).copied())
                .collect()
        })
        .collect();
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in succ.iter().enumerate() {
        for &s in ss {
            if !pred[s].contains(&b) {
                pred[s].push(b);
            }
        }
    }
    let mut order = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some((b, k)) = stack.pop() {
        if k < succ[b].len() {
            stack.push((b, k + 1));
            let s = succ[b][k];
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    order.reverse();
    let mut rpo = vec![usize::MAX; n];
    for (k, &b) in order.iter().enumerate() {
        rpo[b] = k;
    }
    let mut idom = vec![usize::MAX; n];
    idom[0] = 0;
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new = usize::MAX;
            for &p in &pred[b] {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX {
                    p
                } else {
                    let (mut x, mut y) = (p, new);
                    while x != y {
                        while rpo[x] > rpo[y] {
                            x = idom[x];
                        }
                        while rpo[y] > rpo[x] {
                            y = idom[y];
                        }
                    }
                    x
                };
            }
            if new != usize::MAX && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    // dominance frontiers
    let mut df: Vec<HashSet<usize>> = vec![HashSet::default(); n];
    for b in 0..n {
        if pred[b].len() >= 2 && idom[b] != usize::MAX {
            for &p in &pred[b] {
                let mut r = p;
                while r != idom[b] && idom[r] != usize::MAX {
                    df[r].insert(b);
                    if r == idom[r] {
                        break;
                    }
                    r = idom[r];
                }
            }
        }
    }
    // phi placement, allocas numbered in name order
    let names: Vec<String> = {
        let mut v: Vec<String> = allocas.keys().cloned().collect();
        v.sort();
        v
    };
    let slot: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(k, a)| (a.as_str(), k))
        .collect();
    // blocks that store to each alloca, ascending
    let mut defs: Vec<Vec<usize>> = vec![Vec::new(); names.len()];
    for (b, (_, insts)) in f.blocks.iter().enumerate() {
        for i in insts {
            if let Op::Store(_, (_, Val::Local(p)), _) = &i.op {
                if let Some(&k) = slot.get(p.as_str()) {
                    if defs[k].last() != Some(&b) {
                        defs[k].push(b);
                    }
                }
            }
        }
    }
    let mut phis: Vec<Vec<(usize, String)>> = vec![Vec::new(); n]; // block -> (alloca slot, phi name)
    let mut counter = 0usize;
    for (a, work) in defs.iter_mut().enumerate() {
        let mut has: HashSet<usize> = HashSet::default();
        while let Some(b) = work.pop() {
            for &d in &df[b] {
                if has.insert(d) {
                    counter += 1;
                    phis[d].push((a, format!("m2r.{counter}")));
                    work.push(d);
                }
            }
        }
    }
    // renaming over the dominator tree
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 1..n {
        if idom[b] != usize::MAX && idom[b] != b {
            kids[idom[b]].push(b);
        }
    }
    let mut subst: HashMap<String, Val> = HashMap::default();
    let mut phi_in: Vec<Vec<Vec<(Val, String)>>> =
        phis.iter().map(|p| vec![Vec::new(); p.len()]).collect();
    let mut remove: Vec<Vec<bool>> = f.blocks.iter().map(|b| vec![false; b.1.len()]).collect();
    // depth first over the dominator tree, current values per slot, restored from an undo log on leaving a block
    enum Step {
        Enter(usize),
        Leave(usize),
    }
    let mut cur: Vec<Val> = vec![Val::Undef; names.len()];
    let mut undo: Vec<(usize, Val)> = Vec::new();
    let mut steps = vec![Step::Enter(0)];
    while let Some(step) = steps.pop() {
        let b = match step {
            Step::Leave(mark) => {
                while undo.len() > mark {
                    if let Some((k, v)) = undo.pop() {
                        cur[k] = v;
                    }
                }
                continue;
            }
            Step::Enter(b) => b,
        };
        let mark = undo.len();
        for (a, p) in &phis[b] {
            let old = std::mem::replace(&mut cur[*a], Val::Local(p.clone()));
            undo.push((*a, old));
        }
        for (k, i) in f.blocks[b].1.iter().enumerate() {
            match classify(i, &allocas, &slot, &marker_casts) {
                Use::Load(a) => {
                    if let Some(r) = &i.res {
                        subst.insert(r.clone(), cur[a].clone());
                    }
                    remove[b][k] = true;
                }
                Use::Store(a, v) => {
                    let old = std::mem::replace(&mut cur[a], v.clone());
                    undo.push((a, old));
                    remove[b][k] = true;
                }
                Use::Drop => remove[b][k] = true,
                Use::Keep => {}
            }
        }
        let label = &f.blocks[b].0;
        for &s in &succ[b] {
            for (j, (a, _)) in phis[s].iter().enumerate() {
                phi_in[s][j].push((cur[*a].clone(), label.clone()));
            }
        }
        steps.push(Step::Leave(mark));
        for &c in &kids[b] {
            steps.push(Step::Enter(c));
        }
    }
    // unreachable blocks are not renamed: their promoted loads read undef, their stores and markers go with the alloca
    for b in (0..n).filter(|&b| !seen[b]) {
        for (k, i) in f.blocks[b].1.iter().enumerate() {
            match classify(i, &allocas, &slot, &marker_casts) {
                Use::Load(_) => {
                    if let Some(r) = &i.res {
                        subst.insert(r.clone(), Val::Undef);
                    }
                    remove[b][k] = true;
                }
                Use::Store(..) | Use::Drop => remove[b][k] = true,
                Use::Keep => {}
            }
        }
    }
    // edges from unreachable predecessors carry undef
    for s in 0..n {
        for &p in &pred[s] {
            if !seen[p] {
                for (j, _) in phis[s].iter().enumerate() {
                    for _ in succ[p].iter().filter(|&&x| x == s) {
                        phi_in[s][j].push((Val::Undef, f.blocks[p].0.clone()));
                    }
                }
            }
        }
    }
    // rewrite: drop promoted instructions, insert phis, substitute loaded values (chains resolved)
    for (b, (_, insts)) in f.blocks.iter_mut().enumerate() {
        let mut out: Vec<Inst> = Vec::with_capacity(insts.len());
        if seen[b] {
            for (j, (a, p)) in phis[b].iter().enumerate() {
                out.push(Inst {
                    res: Some(p.clone()),
                    op: Op::Phi(
                        Vec::new(),
                        allocas[&names[*a]].clone(),
                        std::mem::take(&mut phi_in[b][j]),
                    ),
                    md: Vec::new(),
                });
            }
        }
        for (k, i) in insts.drain(..).enumerate() {
            if !remove[b][k] {
                out.push(i);
            }
        }
        *insts = out;
    }
    substitute(f, &subst);
}

/// What renaming does with an instruction.
enum Use<'a> {
    /// A load of the promoted alloca with this slot.
    Load(usize),
    /// A store of this value to the promoted alloca with this slot.
    Store(usize, &'a Val),
    /// A promoted alloca, a cast of one or a lifetime marker of one: removed.
    Drop,
    /// Anything else.
    Keep,
}

fn classify<'a>(
    i: &'a Inst,
    allocas: &HashMap<String, Ty>,
    slot: &HashMap<&str, usize>,
    marker_casts: &HashMap<String, String>,
) -> Use<'a> {
    match &i.op {
        Op::Alloca(..) if i.res.as_ref().is_some_and(|r| allocas.contains_key(r)) => Use::Drop,
        Op::Load(_, (_, Val::Local(p)), _, false) => match slot.get(p.as_str()) {
            Some(&a) => Use::Load(a),
            None => Use::Keep,
        },
        Op::Store((_, v), (_, Val::Local(p)), _) => match slot.get(p.as_str()) {
            Some(&a) => Use::Store(a, v),
            None => Use::Keep,
        },
        Op::Cast(_, (_, Val::Local(src)), _) if allocas.contains_key(src) => Use::Drop,
        op if is_lifetime(op) => {
            let mut u = Vec::new();
            locals(op, &mut u);
            if u.iter()
                .any(|x| marker_casts.get(x).is_some_and(|a| allocas.contains_key(a)))
            {
                Use::Drop
            } else {
                Use::Keep
            }
        }
        _ => Use::Keep,
    }
}

/// Follows a chain of substitutions from a local, at most 10 000 steps.
fn resolve(n: &str, subst: &HashMap<String, Val>) -> Option<Val> {
    let mut v = subst.get(n)?;
    let mut guard = 1;
    while let Val::Local(m) = v {
        match subst.get(m) {
            Some(x) if guard < 10_000 => {
                v = x;
                guard += 1;
            }
            _ => break,
        }
    }
    Some(v.clone())
}

/// Replaces substituted locals in place, also inside aggregates and constant casts.
fn subst_in_place(v: &mut Val, subst: &HashMap<String, Val>) {
    match v {
        Val::Local(n) => {
            if let Some(x) = resolve(n, subst) {
                *v = x;
            }
        }
        Val::Aggregate(items) => items.iter_mut().for_each(|(_, x)| subst_in_place(x, subst)),
        Val::Cast(_, inner, _) => subst_in_place(&mut inner.1, subst),
        _ => {}
    }
}

fn side_effect(op: &Op) -> bool {
    match op {
        Op::Load(_, _, _, true)
        | Op::Store(..)
        | Op::Br(_)
        | Op::CondBr(..)
        | Op::Switch(..)
        | Op::Ret(_)
        | Op::Unreachable => true,
        // calls: keep unless pure intrinsic math, air.* reads and samples have no side effect either, but keep them
        // (convergent, and some write), llvm.lifetime of dead allocas goes with the alloca
        Op::Call(_, _, _, c, _, _) => {
            !(c.starts_with("air.convert")
                || c.starts_with("llvm.fmuladd")
                || c.starts_with("air.fast_")
                || c.starts_with("air.fma"))
        }
        _ => false,
    }
}

fn collect_ref<'a>(v: &'a Val, out: &mut Vec<&'a str>) {
    match v {
        Val::Local(n) => out.push(n),
        Val::Aggregate(items) => items.iter().for_each(|(_, x)| collect_ref(x, out)),
        Val::Cast(_, inner, _) => collect_ref(&inner.1, out),
        _ => {}
    }
}

fn local_refs<'a>(op: &'a Op, out: &mut Vec<&'a str>) {
    for v in vals(op) {
        collect_ref(v, out);
    }
}

/// Removes every result that no side effect needs (mark and sweep): the instructions with a side effect or without a
/// result are live, and so is every instruction a live one uses. The rest goes. Unlike removing what has no uses, this
/// also removes cycles of dead phis. mem2reg places a phi for every promoted variable at every join it reaches, also
/// where the variable is dead, so a translated shader with its many function-scope temporaries and a loop got
/// thousands of phis that only fed each other, and the GPU compiler kept them: the forward lighting shader of one
/// title had 5444 phis (231 in Apple's library) and twice the GPU code of Apple's library.
fn dce(f: &mut Function) {
    let removable = |i: &Inst| i.res.is_some() && !side_effect(&i.op);
    let mut live: Vec<Vec<bool>> = f.blocks.iter().map(|b| vec![false; b.1.len()]).collect();
    {
        let mut def: HashMap<&str, (usize, usize)> = HashMap::default();
        let mut work: Vec<(usize, usize)> = Vec::new();
        for (b, (_, insts)) in f.blocks.iter().enumerate() {
            for (k, i) in insts.iter().enumerate() {
                if let Some(r) = &i.res {
                    def.insert(r, (b, k));
                }
                if !removable(i) {
                    live[b][k] = true;
                    work.push((b, k));
                }
            }
        }
        let mut buf: Vec<&str> = Vec::new();
        while let Some((b, k)) = work.pop() {
            buf.clear();
            local_refs(&f.blocks[b].1[k].op, &mut buf);
            for x in &buf {
                if let Some(&(b2, k2)) = def.get(x) {
                    if !live[b2][k2] {
                        live[b2][k2] = true;
                        work.push((b2, k2));
                    }
                }
            }
        }
    }
    for (b, (_, insts)) in f.blocks.iter_mut().enumerate() {
        let mut k = 0;
        insts.retain(|_| {
            k += 1;
            live[b][k - 1]
        });
    }
}

fn substitute(f: &mut Function, subst: &HashMap<String, Val>) {
    if subst.is_empty() {
        return;
    }
    for (_, insts) in &mut f.blocks {
        for i in insts.iter_mut() {
            for v in vals_mut(&mut i.op) {
                subst_in_place(v, subst);
            }
        }
    }
}

/// Phis whose incoming values are all the same value (or the phi itself) become that value.
fn trivial_phis(f: &mut Function) {
    loop {
        let mut subst: HashMap<String, Val> = HashMap::default();
        for (_, insts) in &f.blocks {
            for i in insts {
                if let (Some(r), Op::Phi(_, _, ins)) = (&i.res, &i.op) {
                    let mut uniq: Option<&Val> = None;
                    let mut ok = true;
                    for (v, _) in ins {
                        if matches!(v, Val::Local(n) if n == r) {
                            continue;
                        }
                        match uniq {
                            None => uniq = Some(v),
                            Some(u) if u == v => {}
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        subst.insert(r.clone(), uniq.cloned().unwrap_or(Val::Undef));
                    }
                }
            }
        }
        if subst.is_empty() {
            return;
        }
        for (_, insts) in &mut f.blocks {
            insts.retain(|i| !i.res.as_ref().is_some_and(|r| subst.contains_key(r)));
        }
        substitute(f, &subst);
    }
}

/// Block-local common subexpressions: pure operations and loads from the constant address space.
fn cse(f: &mut Function) {
    let mut subst: HashMap<String, Val> = HashMap::default();
    for (_, insts) in &mut f.blocks {
        let mut seen: HashMap<(Op, Vec<(String, u32)>), String> = HashMap::default();
        let mut drop: HashSet<usize> = HashSet::default();
        for (k, i) in insts.iter().enumerate() {
            let Some(r) = &i.res else { continue };
            let pure = matches!(
                &i.op,
                Op::Gep(..)
                    | Op::Cast(..)
                    | Op::Bin(..)
                    | Op::Cmp(..)
                    | Op::Select(..)
                    | Op::ExtractElt(..)
                    | Op::InsertElt(..)
                    | Op::Shuffle(..)
                    | Op::ExtractVal(..)
                    | Op::InsertVal(..)
                    | Op::FNeg(..)
                    | Op::Load(_, (Ty::Ptr(_, 2), _), _, false)
            );
            if !pure {
                continue;
            }
            // operands already rewritten by earlier substitutions of this block
            let mut op = i.op.clone();
            for v in vals_mut(&mut op) {
                if let Val::Local(n) = v {
                    if let Some(x) = subst.get(n) {
                        *v = x.clone();
                    }
                }
            }
            let key = (op, i.md.clone());
            match seen.get(&key) {
                Some(prev) => {
                    subst.insert(r.clone(), Val::Local(prev.clone()));
                    drop.insert(k);
                }
                None => {
                    seen.insert(key, r.clone());
                }
            }
        }
        let mut k = 0;
        insts.retain(|_| {
            k += 1;
            !drop.contains(&(k - 1))
        });
    }
    substitute(f, &subst);
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEAD_STORE: &str = r#"; ModuleID = '-'
source_filename = "-"
target triple = "air64-apple-macosx14.0.0"

define i32 @f(i32 %a) #0 {
entry:
  %r = alloca i32, align 4
  store i32 %a, i32* %r, align 4
  br label %ret
dead:
  store i32 0, i32* %r, align 4
  %x = load i32, i32* %r, align 4
  %y = add nsw i32 %x, 1
  store i32 %y, i32* %r, align 4
  br label %ret
ret:
  %v = load i32, i32* %r, align 4
  ret i32 %v
}

attributes #0 = { nounwind }
"#;

    #[test]
    fn unreachable_block_loses_its_promoted_accesses() {
        let mut m = crate::parse::parse(DEAD_STORE).expect("subset");
        optimize(&mut m);
        let f = &m.functions[0];
        let mut used = Vec::new();
        for (_, insts) in &f.blocks {
            for i in insts {
                assert!(!matches!(
                    i.op,
                    Op::Alloca(..) | Op::Load(..) | Op::Store(..)
                ));
                locals(&i.op, &mut used);
            }
        }
        assert!(!used.iter().any(|x| x == "r" || x == "x"));
        crate::writer::write(&m).expect("no dangling value");
    }

    /// A loop with a variable that nothing after it reads: its phi and its add only use each other.
    const DEAD_LOOP_VAR: &str = r#"; ModuleID = '-'
source_filename = "-"
target triple = "air64-apple-macosx14.0.0"

define i32 @g(i32 %n) #0 {
entry:
  %acc = alloca i32, align 4
  %dead = alloca i32, align 4
  %i = alloca i32, align 4
  store i32 0, i32* %acc, align 4
  store i32 0, i32* %dead, align 4
  store i32 0, i32* %i, align 4
  br label %loop
loop:
  %iv = load i32, i32* %i, align 4
  %d = load i32, i32* %dead, align 4
  %d2 = add nsw i32 %d, 1
  store i32 %d2, i32* %dead, align 4
  %a = load i32, i32* %acc, align 4
  %a2 = add nsw i32 %a, %iv
  store i32 %a2, i32* %acc, align 4
  %iv2 = add nsw i32 %iv, 1
  store i32 %iv2, i32* %i, align 4
  %c = icmp slt i32 %iv2, %n
  br i1 %c, label %loop, label %exit
exit:
  %r = load i32, i32* %acc, align 4
  ret i32 %r
}

attributes #0 = { nounwind }
"#;

    #[test]
    fn dead_phi_cycles_go() {
        let mut m = crate::parse::parse(DEAD_LOOP_VAR).expect("subset");
        optimize(&mut m);
        let f = &m.functions[0];
        let insts: Vec<&Inst> = f.blocks.iter().flat_map(|b| b.1.iter()).collect();
        let phis = insts.iter().filter(|i| matches!(i.op, Op::Phi(..))).count();
        // the phis of acc and i stay, the phi of dead and its add (a cycle with no other use) go
        assert_eq!(phis, 2);
        assert!(!insts.iter().any(|i| i.res.as_deref() == Some("d2")));
        assert!(insts.iter().any(|i| i.res.as_deref() == Some("a2")));
        crate::writer::write(&m).expect("no dangling value");
    }
}
