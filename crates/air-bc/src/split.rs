//! Per-entry split of a module, the part of `metallib`'s work a library needs (REA notes, passes.md section 3):
//! one module per entry point with only what that entry reaches, and the function constant machinery pruned to the
//! constants the entry uses. Also derives the container tags of each entry (`TYPE`, `VERS`, `CNST`, `VATT`, `VATY`).

use crate::ast::*;
use crate::fx::{HashMap, HashSet};
use std::collections::BTreeSet;

/// Stage of an entry point (`TYPE` tag value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// `!air.vertex`
    Vertex = 0,
    /// `!air.fragment`
    Fragment = 1,
    /// `!air.kernel`
    Kernel = 2,
}

/// A function constant an entry uses: name, `MTLDataType`, index, required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constant {
    /// Constant name.
    pub name: String,
    /// `MTLDataType` of its type.
    pub data_type: u8,
    /// `[[function_constant(N)]]`.
    pub index: u16,
    /// Used without `is_function_constant_defined`.
    pub required: bool,
}

/// One entry point's module and tags.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Function name.
    pub name: String,
    /// Stage.
    pub stage: Stage,
    /// The pruned module.
    pub module: Module,
    /// Function constants it uses, in `!air.function_constants` order.
    pub constants: Vec<Constant>,
    /// Vertex attributes (name, attribute index, `MTLDataType`), vertex stage only.
    pub attributes: Vec<(String, u16, u8)>,
}

/// `MTLDataType` of an MSL type name as AIR metadata spells it.
pub fn data_type(t: &str) -> Option<u8> {
    let base = |s: &str| -> Option<u8> {
        Some(match s {
            "float" => 3,
            "half" => 16,
            "int" => 29,
            "uint" => 33,
            "short" => 37,
            "ushort" => 41,
            "char" => 45,
            "uchar" => 49,
            "bool" => 53,
            "long" => 81,
            "ulong" => 85,
            _ => return None,
        })
    };
    if let Some(b) = base(t) {
        return Some(b);
    }
    let n = t.chars().last()?.to_digit(10)?;
    if !(2..=4).contains(&n) {
        return None;
    }
    // vector types follow their scalar: float2 4, float3 5, float4 6 ..., bool vectors 54..56
    Some(base(&t[..t.len() - 1])? + n as u8 - 1)
}

fn global_refs_val(v: &Val, out: &mut Vec<String>) {
    match v {
        Val::Global(g) => out.push(g.clone()),
        Val::Aggregate(items) => items.iter().for_each(|(_, x)| global_refs_val(x, out)),
        Val::Cast(_, inner, _) => global_refs_val(&inner.1, out),
        _ => {}
    }
}

fn op_vals(op: &Op) -> Vec<&Val> {
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

/// Globals and functions a function body names, and the metadata nodes it attaches.
fn body_refs(f: &Function) -> (Vec<String>, Vec<u32>) {
    let (mut g, mut md) = (Vec::new(), Vec::new());
    for (_, insts) in &f.blocks {
        for i in insts {
            if let Op::Call(_, _, _, callee, _, _) = &i.op {
                g.push(callee.clone());
            }
            for v in op_vals(&i.op) {
                global_refs_val(v, &mut g);
            }
            md.extend(i.md.iter().map(|x| x.1));
        }
    }
    (g, md)
}

fn md_refs(ops: &[MdOp], g: &mut Vec<String>, n: &mut Vec<u32>) {
    for o in ops {
        match o {
            MdOp::Node(x) => n.push(*x),
            MdOp::Value(_, v) => global_refs_val(v, g),
            MdOp::Str(_) => {}
        }
    }
}

const STATIC_INIT: &str = "air.static_init";

/// Splits `m` into one module per entry point (order of the stage lists: vertex, fragment, kernel).
pub fn split(m: &Module) -> Result<Vec<Entry>, String> {
    let md: HashMap<u32, &(u32, bool, Vec<MdOp>)> = m.md.iter().map(|x| (x.0, x)).collect();
    let funcs: HashMap<&str, &Function> =
        m.functions.iter().map(|f| (f.name.as_str(), f)).collect();
    let named = |k: &str| {
        m.named_md
            .iter()
            .find(|x| x.0 == k)
            .map(|x| x.1.clone())
            .unwrap_or_default()
    };
    // static init: global stored -> globals loaded in the same init function
    let mut deps: HashMap<String, Vec<String>> = HashMap::default();
    let mut init_of: HashMap<String, String> = HashMap::default();
    for f in &m.functions {
        if f.section.as_deref() != Some(STATIC_INIT) || f.name == "_GLOBAL__sub_I__" {
            continue;
        }
        let (mut loads, mut stores) = (Vec::new(), Vec::new());
        for (_, insts) in &f.blocks {
            for i in insts {
                match &i.op {
                    Op::Load(_, p, _, _) => global_refs_val(&p.1, &mut loads),
                    Op::Store(_, p, _) => global_refs_val(&p.1, &mut stores),
                    _ => {}
                }
            }
        }
        for s in stores {
            deps.entry(s.clone())
                .or_default()
                .extend(loads.iter().cloned());
            init_of.insert(s, f.name.clone());
        }
    }
    // function constants: FC_INIT global -> (name, type, index)
    let mut fcs: Vec<(String, String, String, u16)> = Vec::new();
    for n in named("air.function_constants") {
        let ops = &md.get(&n).ok_or("function constant node")?.2;
        let mut g = Vec::new();
        md_refs(ops, &mut g, &mut Vec::new());
        let s: Vec<String> = ops
            .iter()
            .filter_map(|o| match o {
                MdOp::Str(s) => Some(String::from_utf8_lossy(s).to_string()),
                _ => None,
            })
            .collect();
        let idx = ops.iter().find_map(|o| match o {
            MdOp::Value(Ty::Int(32), Val::Int(i)) => Some(*i as u16),
            _ => None,
        });
        if let (Some(gl), Some(ty), Some(name), Some(idx)) = (g.first(), s.first(), s.get(1), idx) {
            fcs.push((gl.clone(), ty.clone(), name.clone(), idx));
        }
    }
    let fc_globals: HashSet<&str> = fcs.iter().map(|x| x.0.as_str()).collect();
    let mut out = Vec::new();
    for (stage, key) in [
        (Stage::Vertex, "air.vertex"),
        (Stage::Fragment, "air.fragment"),
        (Stage::Kernel, "air.kernel"),
    ] {
        for node in named(key) {
            let ops = &md.get(&node).ok_or("entry node")?.2;
            let mut g = Vec::new();
            md_refs(&ops[..1], &mut g, &mut Vec::new());
            let name = g.first().ok_or("entry without function")?.clone();
            out.push(entry(
                m,
                &md,
                &funcs,
                &deps,
                &init_of,
                &fcs,
                &fc_globals,
                stage,
                key,
                node,
                &name,
            )?);
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn entry(
    m: &Module,
    md: &HashMap<u32, &(u32, bool, Vec<MdOp>)>,
    funcs: &HashMap<&str, &Function>,
    deps: &HashMap<String, Vec<String>>,
    init_of: &HashMap<String, String>,
    fcs: &[(String, String, String, u16)],
    fc_globals: &HashSet<&str>,
    stage: Stage,
    key: &str,
    node: u32,
    name: &str,
) -> Result<Entry, String> {
    // reachability over functions, globals and metadata from the entry function and its stage node
    let (mut gl, mut nodes): (BTreeSet<String>, BTreeSet<u32>) = (BTreeSet::new(), BTreeSet::new());
    let (mut gq, mut nq) = (vec![name.to_string()], vec![node]);
    while !gq.is_empty() || !nq.is_empty() {
        while let Some(g) = gq.pop() {
            if !gl.insert(g.clone()) {
                continue;
            }
            if let Some(f) = funcs.get(g.as_str()) {
                let (gs, ns) = body_refs(f);
                gq.extend(gs);
                nq.extend(ns);
            } else if let Some(v) = m.globals.iter().find(|x| x.name == g) {
                let mut r = Vec::new();
                global_refs_val(&v.init, &mut r);
                gq.extend(r);
            }
            // a global stored by the static initialiser depends on what that initialiser loads
            if let Some(d) = deps.get(&g) {
                gq.extend(d.iter().cloned());
                if let Some(i) = init_of.get(&g) {
                    gq.push(i.clone());
                }
            }
        }
        while let Some(n) = nq.pop() {
            if !nodes.insert(n) {
                continue;
            }
            let (mut g, mut ns) = (Vec::new(), Vec::new());
            md_refs(
                &md.get(&n).ok_or_else(|| format!("metadata !{n}"))?.2,
                &mut g,
                &mut ns,
            );
            gq.extend(g);
            nq.extend(ns);
        }
    }
    let used_fc: Vec<&(String, String, String, u16)> =
        fcs.iter().filter(|f| gl.contains(&f.0)).collect();
    let inits: Vec<String> = gl
        .iter()
        .filter(|g| {
            funcs
                .get(g.as_str())
                .is_some_and(|f| f.section.as_deref() == Some(STATIC_INIT))
        })
        .cloned()
        .collect();
    // build the module
    let mut e = Module {
        source: m.source.clone(),
        layout: m.layout.clone(),
        triple: m.triple.clone(),
        types: m.types.clone(),
        attr_groups: m.attr_groups.clone(),
        ..Module::default()
    };
    for f in &m.functions {
        if f.name == "_GLOBAL__sub_I__" {
            if inits.is_empty() {
                continue;
            }
            let mut f = f.clone();
            for (_, insts) in &mut f.blocks {
                insts.retain(|i| match &i.op {
                    Op::Call(_, _, _, c, _, _) => inits.contains(c),
                    _ => true,
                });
            }
            e.functions.push(f);
            gl.insert(f_name(&e));
            continue;
        }
        if gl.contains(&f.name) {
            e.functions.push(f.clone());
        }
    }
    for g in &m.globals {
        match g.name.as_str() {
            "llvm.global_ctors" => {
                if !inits.is_empty() {
                    e.globals.push(g.clone());
                }
            }
            "llvm.compiler.used" => {
                let mut g = g.clone();
                if let Val::Aggregate(items) = &mut g.init {
                    items.retain(|(_, v)| {
                        let mut r = Vec::new();
                        global_refs_val(v, &mut r);
                        r.iter().all(|x| gl.contains(x))
                    });
                    if items.is_empty() {
                        continue;
                    }
                    let n = items.len() as u64;
                    if let Ty::Array(len, _) = &mut g.ty {
                        *len = n;
                    }
                }
                e.globals.push(g);
            }
            _ => {
                if gl.contains(&g.name) {
                    e.globals.push(g.clone());
                }
            }
        }
    }
    // named metadata: the entry's stage list, the kept constants, everything else unchanged
    let fc_nodes: Vec<u32> = m
        .named_md
        .iter()
        .find(|x| x.0 == "air.function_constants")
        .map(|x| x.1.clone())
        .unwrap_or_default();
    for (k, list) in &m.named_md {
        let list: Vec<u32> = match k.as_str() {
            "air.vertex" | "air.fragment" | "air.kernel" => {
                if k != key {
                    continue;
                }
                vec![node]
            }
            "air.function_constants" => {
                let kept: Vec<u32> = fc_nodes
                    .iter()
                    .zip(fcs)
                    .filter(|(_, f)| gl.contains(&f.0))
                    .map(|(n, _)| *n)
                    .collect();
                if kept.is_empty() {
                    continue;
                }
                kept
            }
            _ => list.clone(),
        };
        e.named_md.push((k.clone(), list));
    }
    // metadata nodes reachable from the kept named metadata and bodies
    let mut keep: BTreeSet<u32> = BTreeSet::new();
    let mut q: Vec<u32> = e
        .named_md
        .iter()
        .flat_map(|x| x.1.iter().copied())
        .collect();
    for f in &e.functions {
        q.extend(body_refs(f).1);
    }
    while let Some(n) = q.pop() {
        if keep.insert(n) {
            if let Some(x) = md.get(&n) {
                md_refs(&x.2, &mut Vec::new(), &mut q);
            }
        }
    }
    e.md =
        m.md.iter()
            .filter(|x| keep.contains(&x.0))
            .cloned()
            .collect();
    let _ = fc_globals;
    // tags
    let constants = used_fc
        .iter()
        .map(|(_, ty, n, idx)| Constant {
            name: n.clone(),
            data_type: data_type(ty).unwrap_or(0),
            index: *idx,
            required: true,
        })
        .collect();
    let mut attributes = Vec::new();
    if stage == Stage::Vertex {
        for o in &md.get(&node).ok_or("entry node")?.2 {
            let MdOp::Node(args) = o else { continue };
            for a in &md.get(args).map(|x| x.2.clone()).unwrap_or_default() {
                let MdOp::Node(arg) = a else { continue };
                let ops = &md.get(arg).map(|x| x.2.clone()).unwrap_or_default();
                let strs: Vec<String> = ops
                    .iter()
                    .filter_map(|o| match o {
                        MdOp::Str(s) => Some(String::from_utf8_lossy(s).to_string()),
                        _ => None,
                    })
                    .collect();
                if strs.first().map(String::as_str) != Some("air.vertex_input") {
                    continue;
                }
                let loc = ops
                    .iter()
                    .position(|o| matches!(o, MdOp::Str(s) if s == b"air.location_index"));
                let idx = loc.and_then(|p| match ops.get(p + 1) {
                    Some(MdOp::Value(_, Val::Int(i))) => Some(*i as u16),
                    _ => None,
                });
                let after = |k: &str| {
                    strs.iter()
                        .position(|s| s == k)
                        .and_then(|p| strs.get(p + 1))
                        .cloned()
                };
                if let (Some(idx), Some(ty), Some(n)) =
                    (idx, after("air.arg_type_name"), after("air.arg_name"))
                {
                    let flag = if strs.iter().any(|s| s == "air.arg_unused") {
                        0
                    } else {
                        0x8000
                    };
                    attributes.push((n, idx | flag, data_type(&ty).unwrap_or(0)));
                }
            }
        }
    }
    Ok(Entry {
        name: name.to_string(),
        stage,
        module: e,
        constants,
        attributes,
    })
}

fn f_name(e: &Module) -> String {
    e.functions
        .last()
        .map(|f| f.name.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::data_type;

    #[test]
    fn data_types() {
        assert_eq!(data_type("float4"), Some(6));
        assert_eq!(data_type("int"), Some(29));
        assert_eq!(data_type("bool"), Some(53));
        assert_eq!(data_type("half2"), Some(17));
        assert_eq!(data_type("uint3"), Some(35));
    }
}
