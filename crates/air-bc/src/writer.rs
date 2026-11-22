//! LLVM 14 era bitcode writer (typed pointers, relative value ids, string table) for the parsed
//! module. Records are written unabbreviated, local values and blocks have no names.

use crate::fx::{FxHasher, HashMap};
use std::hash::{Hash, Hasher};

use crate::ast::*;
use crate::bits::{signed, Bits};

/// A writer error (an unsupported construct: the caller falls back to text).
pub type R<T> = Result<T, String>;

const MODULE: u32 = 8;
const PARAMATTR: u32 = 9;
const PARAMATTR_GROUP: u32 = 10;
const CONSTANTS: u32 = 11;
const FUNCTION: u32 = 12;
const IDENT: u32 = 13;
const METADATA: u32 = 15;
const MD_ATTACH: u32 = 16;
const TYPES: u32 = 17;
const MD_KIND: u32 = 22;
const STRTAB: u32 = 23;

fn attr_kind(w: &str) -> Option<u64> {
    Some(match w {
        "alwaysinline" => 2,
        "inlinehint" => 4,
        "noalias" => 9,
        "nobuiltin" => 10,
        "nocapture" => 11,
        "noinline" => 14,
        "noreturn" => 17,
        "nounwind" => 18,
        "readnone" => 20,
        "readonly" => 21,
        "zeroext" => 34,
        "signext" => 24,
        "convergent" => 43,
        "argmemonly" => 45,
        "norecurse" => 48,
        "inaccessiblememonly" => 49,
        "writeonly" => 52,
        "speculatable" => 53,
        "immarg" => 60,
        "willreturn" => 61,
        "nofree" => 62,
        "nosync" => 63,
        "noundef" => 68,
        "mustprogress" => 70,
        "nocallback" => 71,
        _ => return None,
    })
}

/// Encodes attribute words into group record operands.
fn attr_ops(words: &[String]) -> R<Vec<u64>> {
    let mut ops = Vec::new();
    for w in words {
        if let Some(s) = w.strip_prefix('"') {
            match s.split_once('=') {
                Some((k, v)) => {
                    ops.push(4);
                    ops.extend(k.bytes().map(|b| b as u64));
                    ops.push(0);
                    ops.extend(v.bytes().map(|b| b as u64));
                    ops.push(0);
                }
                None => {
                    ops.push(3);
                    ops.extend(s.bytes().map(|b| b as u64));
                    ops.push(0);
                }
            }
        } else if let Some(n) = w.strip_prefix("align ") {
            ops.extend([1, 1, n.parse::<u64>().map_err(|_| "align")?]);
        } else if let Some(n) = w.strip_prefix("dereferenceable ") {
            ops.extend([1, 41, n.parse::<u64>().map_err(|_| "deref")?]);
        } else {
            let k = attr_kind(w).ok_or_else(|| format!("attribute {w}"))?;
            ops.extend([0, k]);
        }
    }
    Ok(ops)
}

fn fmf(flags: &[String]) -> u64 {
    let mut f = 0;
    for w in flags {
        f |= match w.as_str() {
            "fast" => 0xfe,
            "nnan" => 1 << 1,
            "ninf" => 1 << 2,
            "nsz" => 1 << 3,
            "arcp" => 1 << 4,
            "contract" => 1 << 5,
            "afn" => 1 << 6,
            "reassoc" => 1 << 7,
            _ => 0,
        };
    }
    f
}

fn align_enc(a: u32) -> u64 {
    if a == 0 {
        0
    } else {
        a.trailing_zeros() as u64 + 1
    }
}

/// Interns a (function attrs, return attrs, param attrs) list to its id.
type AttrListFn<'a> = dyn FnMut(&[String], &[String], &[Vec<String>]) -> R<u64> + 'a;

struct Types {
    ids: HashMap<Ty, u64>,
    recs: Vec<(u32, Vec<u64>, Option<String>)>,
    named: HashMap<String, Option<(bool, Vec<Ty>)>>,
    visiting: Vec<String>,
}

impl Types {
    fn id(&mut self, t: &Ty) -> R<u64> {
        if let Some(i) = self.ids.get(t) {
            return Ok(*i);
        }
        let (code, ops, name): (u32, Vec<u64>, Option<String>) = match t {
            Ty::Void => (2, vec![], None),
            Ty::Float => (3, vec![], None),
            Ty::Double => (4, vec![], None),
            Ty::Label => (5, vec![], None),
            Ty::Half => (10, vec![], None),
            Ty::Metadata => (16, vec![], None),
            Ty::Int(w) => (7, vec![*w as u64], None),
            Ty::Ptr(e, s) => {
                let e = self.id(e)?;
                (8, vec![e, *s as u64], None)
            }
            Ty::Array(n, e) => {
                let e = self.id(e)?;
                (11, vec![*n, e], None)
            }
            Ty::Vector(n, e) => {
                let e = self.id(e)?;
                (12, vec![*n as u64, e], None)
            }
            Ty::Struct(p, es) => {
                let mut ops = vec![*p as u64];
                for e in es {
                    ops.push(self.id(e)?);
                }
                (18, ops, None)
            }
            Ty::Func(r, ps) => {
                let mut ops = vec![0, self.id(r)?];
                for p in ps {
                    ops.push(self.id(p)?);
                }
                (21, ops, None)
            }
            Ty::Named(n) => {
                if self.visiting.contains(n) {
                    return Err(format!("recursive struct {n}"));
                }
                let body = self
                    .named
                    .get(n)
                    .cloned()
                    .ok_or_else(|| format!("unknown type %{n}"))?;
                match body {
                    None => (6, vec![0], Some(n.clone())),
                    Some((p, es)) => {
                        self.visiting.push(n.clone());
                        let mut ops = vec![p as u64];
                        for e in &es {
                            ops.push(self.id(e)?);
                        }
                        self.visiting.pop();
                        (20, ops, Some(n.clone()))
                    }
                }
            }
        };
        if let Some(i) = self.ids.get(t) {
            return Ok(*i);
        }
        let i = self.recs.len() as u64;
        self.recs.push((code, ops, name));
        self.ids.insert(t.clone(), i);
        Ok(i)
    }
}

/// Positions of list entries by hash, for first-match lookups without a linear scan.
#[derive(Default)]
struct Lookup(HashMap<u64, Vec<usize>>);

impl Lookup {
    fn hash<K: Hash + ?Sized>(k: &K) -> u64 {
        let mut h = FxHasher::default();
        k.hash(&mut h);
        h.finish()
    }

    fn of<K: Hash>(list: &[K]) -> Self {
        let mut l = Lookup::default();
        for (k, x) in list.iter().enumerate() {
            l.add(Self::hash(x), k);
        }
        l
    }

    fn add(&mut self, h: u64, k: usize) {
        self.0.entry(h).or_default().push(k);
    }

    fn find(&self, h: u64, mut eq: impl FnMut(usize) -> bool) -> Option<usize> {
        self.0.get(&h)?.iter().copied().find(|&k| eq(k))
    }

    /// First position of `(t, v)` in `list`.
    fn pair(&self, list: &[(Ty, Val)], t: &Ty, v: &Val) -> Option<usize> {
        self.find(Self::hash(&(t, v)), |k| list[k].0 == *t && list[k].1 == *v)
    }
}

/// Value table of a function (or the module): constants interned with ids.
struct Consts {
    base: u64,
    list: Vec<(Ty, Val)>,
    index: Lookup,
}

impl Consts {
    fn new(base: u64) -> Self {
        Consts {
            base,
            list: Vec::new(),
            index: Lookup::default(),
        }
    }

    fn get(&self, t: &Ty, v: &Val) -> Option<u64> {
        self.index
            .pair(&self.list, t, v)
            .map(|k| self.base + k as u64)
    }

    fn id(&mut self, t: &Ty, v: &Val) -> u64 {
        if let Some(k) = self.get(t, v) {
            return k;
        }
        // elements of aggregates and cast operands first
        match v {
            Val::Aggregate(items) => {
                for (et, ev) in items {
                    if is_const(ev) {
                        self.id(et, ev);
                    }
                }
            }
            Val::Cast(_, inner, _) if is_const(&inner.1) => {
                self.id(&inner.0, &inner.1);
            }
            _ => {}
        }
        if let Some(k) = self.get(t, v) {
            return k;
        }
        self.index.add(Lookup::hash(&(t, v)), self.list.len());
        self.list.push((t.clone(), v.clone()));
        self.base + self.list.len() as u64 - 1
    }
}

fn is_const(v: &Val) -> bool {
    !matches!(v, Val::Local(_) | Val::Global(_))
}

/// Writes the module as bitcode.
pub fn write(m: &Module) -> R<Vec<u8>> {
    let mut types = Types {
        ids: HashMap::default(),
        recs: Vec::new(),
        named: m.types.iter().cloned().collect(),
        visiting: Vec::new(),
    };
    // named structs first, in declaration order
    for (n, _) in &m.types {
        types.id(&Ty::Named(n.clone()))?;
    }
    // module value ids: globals, then functions
    let mut gids: HashMap<String, u64> = HashMap::default();
    let mut gtys: HashMap<String, Ty> = HashMap::default();
    for g in &m.globals {
        gids.insert(g.name.clone(), gids.len() as u64);
        gtys.insert(g.name.clone(), Ty::Ptr(Box::new(g.ty.clone()), g.space));
    }
    for f in &m.functions {
        gids.insert(f.name.clone(), gids.len() as u64);
        let ft = Ty::Func(
            Box::new(f.ret.clone()),
            f.params.iter().map(|p| p.ty.clone()).collect(),
        );
        gtys.insert(f.name.clone(), Ty::Ptr(Box::new(ft), 0));
    }
    let nglobals = gids.len() as u64;
    let mut mconsts = Consts::new(nglobals);
    let groups: HashMap<u32, Vec<String>> = m.attr_groups.iter().cloned().collect();

    // attribute groups and lists
    let mut grp_list: Vec<(u64, Vec<u64>)> = Vec::new(); // (index, ops) -> id = pos+1
    let mut attr_lists: Vec<Vec<u64>> = Vec::new();
    let group_id =
        |idx: u64, words: &[String], grp_list: &mut Vec<(u64, Vec<u64>)>| -> R<Option<u64>> {
            if words.is_empty() {
                return Ok(None);
            }
            let ops = attr_ops(words)?;
            if let Some(k) = grp_list.iter().position(|(i, o)| *i == idx && *o == ops) {
                return Ok(Some(k as u64 + 1));
            }
            grp_list.push((idx, ops));
            Ok(Some(grp_list.len() as u64))
        };
    let list_id = |fnw: &[String],
                   ret: &[String],
                   params: &[Vec<String>],
                   grp_list: &mut Vec<(u64, Vec<u64>)>,
                   attr_lists: &mut Vec<Vec<u64>>|
     -> R<u64> {
        let mut gs = Vec::new();
        if let Some(g) = group_id(0xFFFF_FFFF, fnw, grp_list)? {
            gs.push(g);
        }
        if let Some(g) = group_id(0, ret, grp_list)? {
            gs.push(g);
        }
        for (k, p) in params.iter().enumerate() {
            if let Some(g) = group_id(k as u64 + 1, p, grp_list)? {
                gs.push(g);
            }
        }
        if gs.is_empty() {
            return Ok(0);
        }
        if let Some(k) = attr_lists.iter().position(|l| *l == gs) {
            return Ok(k as u64 + 1);
        }
        attr_lists.push(gs);
        Ok(attr_lists.len() as u64)
    };
    // sort group lists by the LLVM rule? (order is free), function attribute lists
    let mut fn_attr = Vec::new();
    for f in &m.functions {
        let fw = f
            .group
            .and_then(|g| groups.get(&g).cloned())
            .unwrap_or_default();
        let ps: Vec<Vec<String>> = f.params.iter().map(|p| p.attrs.clone()).collect();
        fn_attr.push(list_id(
            &fw,
            &f.ret_attrs,
            &ps,
            &mut grp_list,
            &mut attr_lists,
        )?);
    }

    // sections
    let mut sections: Vec<String> = Vec::new();
    let sec_id = |s: &Option<String>, sections: &mut Vec<String>| -> u64 {
        match s {
            None => 0,
            Some(s) => {
                if let Some(k) = sections.iter().position(|x| x == s) {
                    return k as u64 + 1;
                }
                sections.push(s.clone());
                sections.len() as u64
            }
        }
    };
    let mut strtab: Vec<u8> = Vec::new();
    let add_str = |s: &str, strtab: &mut Vec<u8>| -> (u64, u64) {
        let off = strtab.len() as u64;
        strtab.extend_from_slice(s.as_bytes());
        (off, s.len() as u64)
    };
    let linkage = |l: &str| -> u64 {
        match l {
            "internal" => 3,
            "linkonce_odr" => 19,
            "appending" => 2,
            "private" => 9,
            _ => 0,
        }
    };
    let mut global_recs = Vec::new();
    for g in &m.globals {
        let (off, len) = add_str(&g.name, &mut strtab);
        let ty = types.id(&g.ty)?;
        let init = const_or_global(&mut mconsts, &gids, &g.ty, &g.init)? + 1;
        let sec = sec_id(&g.section, &mut sections);
        global_recs.push(vec![
            off,
            len,
            ty,
            ((g.space as u64) << 2) | 2 | g.constant as u64,
            init,
            linkage(&g.linkage),
            align_enc(g.align),
            sec,
            g.hidden as u64,
            0,
            0,
            g.ext_init as u64,
            0,
            0,
            0,
            0,
        ]);
    }
    let mut func_recs = Vec::new();
    for (k, f) in m.functions.iter().enumerate() {
        let (off, len) = add_str(&f.name, &mut strtab);
        let ft = Ty::Func(
            Box::new(f.ret.clone()),
            f.params.iter().map(|p| p.ty.clone()).collect(),
        );
        let fty = types.id(&ft)?;
        types.id(&Ty::Ptr(Box::new(ft.clone()), 0))?;
        let sec = sec_id(&f.section, &mut sections);
        let hidden = 0;
        func_recs.push(vec![
            off,
            len,
            fty,
            0,
            f.blocks.is_empty() as u64,
            linkage(&f.linkage),
            fn_attr[k],
            0,
            sec,
            hidden,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
    }

    // metadata: strings, values, nodes
    let mut md_strings: Vec<Vec<u8>> = Vec::new();
    let mut md_values: Vec<(Ty, Val)> = Vec::new();
    let mut str_index = Lookup::default();
    let mut val_index = Lookup::default();
    for (_, _, ops) in &m.md {
        for o in ops {
            match o {
                MdOp::Str(s) => {
                    let h = Lookup::hash(s);
                    if str_index.find(h, |k| md_strings[k] == *s).is_none() {
                        str_index.add(h, md_strings.len());
                        md_strings.push(s.clone());
                    }
                }
                MdOp::Value(t, v) => {
                    if val_index.pair(&md_values, t, v).is_none() {
                        val_index.add(Lookup::hash(&(t, v)), md_values.len());
                        md_values.push((t.clone(), v.clone()));
                    }
                }
                MdOp::Node(_) => {}
            }
        }
    }
    let nstr = md_strings.len() as u64;
    let nval = md_values.len() as u64;
    let node_index: HashMap<u32, u64> =
        m.md.iter()
            .enumerate()
            .map(|(k, (n, _, _))| (*n, nstr + nval + k as u64))
            .collect();
    let mut md_value_recs = Vec::new();
    for (t, v) in &md_values {
        let ty = types.id(t)?;
        let id = const_or_global(&mut mconsts, &gids, t, v)?;
        md_value_recs.push(vec![ty, id]);
    }
    let mut md_node_recs = Vec::new();
    for (_, distinct, ops) in &m.md {
        let mut r = Vec::new();
        for o in ops {
            r.push(match o {
                MdOp::Str(s) => {
                    str_index
                        .find(Lookup::hash(s), |k| md_strings[k] == *s)
                        .unwrap_or(0) as u64
                        + 1
                }
                MdOp::Value(t, v) => {
                    nstr + val_index.pair(&md_values, t, v).unwrap_or(0) as u64 + 1
                }
                MdOp::Node(n) => {
                    node_index
                        .get(n)
                        .copied()
                        .ok_or_else(|| format!("metadata !{n}"))?
                        + 1
                }
            });
        }
        md_node_recs.push((*distinct, r));
    }
    let kinds: [(u64, &str); 4] = [
        (1, "tbaa"),
        (4, "range"),
        (5, "tbaa.struct"),
        (18, "llvm.loop"),
    ];

    // function bodies (written later, types interned now)
    let mut body_bits: Vec<FuncCode> = Vec::new();
    for f in &m.functions {
        if f.blocks.is_empty() {
            continue;
        }
        body_bits.push(function_code(
            f,
            &mut types,
            &gids,
            &gtys,
            nglobals + mconsts.list.len() as u64,
            &node_index,
            &kinds,
            &groups,
            &mut |fw, r, p| list_id(fw, r, p, &mut grp_list, &mut attr_lists),
        )?);
    }
    // module constants may have grown? (function constants are local), intern their types
    let mconst_list = mconsts.list.clone();
    for (t, v) in &mconst_list {
        intern_const_types(&mut types, t, v)?;
    }
    for fc in &body_bits {
        for (t, v) in &fc.consts {
            intern_const_types(&mut types, t, v)?;
        }
    }

    // ---- emit
    let mut b = Bits::new();
    for c in *b"BC" {
        b.emit(c as u64, 8);
    }
    b.emit(0x0, 4);
    b.emit(0xC, 4);
    b.emit(0xE, 4);
    b.emit(0xD, 4);
    b.enter(IDENT, 5);
    b.record_str(1, &[], "LLVM14.0.0");
    b.record(2, &[0]);
    b.exit();
    b.enter(MODULE, 3);
    b.record(1, &[2]);
    // types
    b.enter(TYPES, 4);
    b.record(1, &[types.recs.len() as u64]);
    for (code, ops, name) in &types.recs {
        if let Some(n) = name {
            b.record_str(19, &[], n);
        }
        b.record(*code, ops);
    }
    b.exit();
    if !grp_list.is_empty() {
        b.enter(PARAMATTR_GROUP, 3);
        for (k, (idx, ops)) in grp_list.iter().enumerate() {
            let mut r = vec![k as u64 + 1, *idx];
            r.extend_from_slice(ops);
            b.record(3, &r);
        }
        b.exit();
        b.enter(PARAMATTR, 3);
        for l in &attr_lists {
            b.record(2, l);
        }
        b.exit();
    }
    b.record_str(2, &[], &m.triple);
    b.record_str(3, &[], &m.layout);
    for s in &sections {
        b.record_str(5, &[], s);
    }
    b.record_str(16, &[], &m.source);
    for r in &global_recs {
        b.record(7, r);
    }
    for r in &func_recs {
        b.record(8, r);
    }
    write_consts(&mut b, &mconst_list, &mut types, &gids, nglobals)?;
    b.enter(MD_KIND, 3);
    for (k, n) in kinds {
        b.record_str(6, &[k], n);
    }
    b.exit();
    b.enter(METADATA, 4);
    for s in &md_strings {
        let ops: Vec<u64> = s.iter().map(|x| *x as u64).collect();
        b.record(1, &ops);
    }
    for r in &md_value_recs {
        b.record(2, r);
    }
    for (d, r) in &md_node_recs {
        b.record(if *d { 5 } else { 3 }, r);
    }
    for (name, nodes) in &m.named_md {
        b.record_str(4, &[], name);
        let ids: Vec<u64> = nodes
            .iter()
            .map(|n| node_index.get(n).copied().unwrap_or(0))
            .collect();
        b.record(10, &ids);
    }
    b.exit();
    for fc in &body_bits {
        emit_function(&mut b, fc, &mut types, &gids)?;
    }
    b.exit();
    b.enter(STRTAB, 3);
    b.blob_record(1, &strtab);
    b.exit();
    let body = b.finish();
    // wrapper header like air-as
    let mut out = Vec::with_capacity(body.len() + 20);
    out.extend_from_slice(&0x0B17_C0DEu32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn intern_const_types(types: &mut Types, t: &Ty, v: &Val) -> R<()> {
    types.id(t)?;
    match v {
        Val::Aggregate(items) => {
            for (et, ev) in items {
                intern_const_types(types, et, ev)?;
            }
        }
        Val::Cast(_, inner, to) => {
            intern_const_types(types, &inner.0, &inner.1)?;
            types.id(to)?;
        }
        _ => {}
    }
    Ok(())
}

fn const_or_global(c: &mut Consts, gids: &HashMap<String, u64>, t: &Ty, v: &Val) -> R<u64> {
    match v {
        Val::Global(g) => gids.get(g).copied().ok_or_else(|| format!("unknown @{g}")),
        Val::Local(l) => Err(format!("local %{l} in a constant")),
        v => Ok(c.id(t, v)),
    }
}

fn write_consts(
    b: &mut Bits,
    list: &[(Ty, Val)],
    types: &mut Types,
    gids: &HashMap<String, u64>,
    base: u64,
) -> R<()> {
    if list.is_empty() {
        return Ok(());
    }
    let index = Lookup::of(list);
    let find = |t: &Ty, v: &Val| -> R<u64> {
        if let Val::Global(g) = v {
            return gids.get(g).copied().ok_or_else(|| format!("@{g}"));
        }
        index
            .pair(list, t, v)
            .map(|k| base + k as u64)
            .ok_or_else(|| "constant id".to_string())
    };
    b.enter(CONSTANTS, 4);
    let mut cur: Option<u64> = None;
    for (t, v) in list {
        let tid = types.id(t)?;
        if cur != Some(tid) {
            b.record(1, &[tid]);
            cur = Some(tid);
        }
        match v {
            Val::Zero | Val::Null => b.record(2, &[]),
            Val::Undef => b.record(3, &[]),
            Val::Poison => b.record(26, &[]),
            Val::Int(i) => {
                let w = match t {
                    Ty::Int(w) => *w,
                    _ => return Err("int constant type".into()),
                };
                let x = if w >= 64 {
                    *i as i64
                } else {
                    ((*i as i64) << (64 - w)) >> (64 - w)
                };
                b.record(4, &[signed(x)]);
            }
            Val::Float(bits) => {
                let raw = match t {
                    Ty::Float => (f64::from_bits(*bits) as f32).to_bits() as u64,
                    Ty::Double => *bits,
                    Ty::Half => *bits,
                    _ => return Err("float constant type".into()),
                };
                b.record(6, &[raw]);
            }
            Val::Aggregate(items) => {
                let mut ids = Vec::new();
                for (et, ev) in items {
                    ids.push(find(et, ev)?);
                }
                b.record(7, &ids);
            }
            Val::Cast(op, inner, _) => {
                let opc = match op.as_str() {
                    "bitcast" => 11,
                    "addrspacecast" => 12,
                    _ => return Err(format!("cast {op}")),
                };
                let ity = types.id(&inner.0)?;
                b.record(11, &[opc, ity, find(&inner.0, &inner.1)?]);
            }
            Val::Global(_) | Val::Local(_) => return Err("value in constants".into()),
        }
    }
    b.exit();
    Ok(())
}

/// A function body prepared for emission.
struct FuncCode {
    nvals_before: u64,
    nargs: u64,
    consts: Vec<(Ty, Val)>,
    nblocks: u64,
    recs: Vec<(u32, Vec<u64>)>,
    attach: Vec<Vec<u64>>,
}

#[allow(clippy::too_many_arguments)]
fn function_code(
    f: &Function,
    types: &mut Types,
    gids: &HashMap<String, u64>,
    gtys: &HashMap<String, Ty>,
    module_vals: u64,
    md_index: &HashMap<u32, u64>,
    kinds: &[(u64, &str)],
    groups: &HashMap<u32, Vec<String>>,
    attr_list: &mut AttrListFn<'_>,
) -> R<FuncCode> {
    // value numbering: module values, args, function constants, instructions
    let nargs = f.params.len() as u64;
    let mut locals: HashMap<String, (u64, Ty)> = HashMap::default();
    for (k, p) in f.params.iter().enumerate() {
        if let Some(n) = &p.name {
            locals.insert(n.clone(), (module_vals + k as u64, p.ty.clone()));
        }
    }
    let bbs: HashMap<String, u64> = f
        .blocks
        .iter()
        .enumerate()
        .map(|(k, (l, _))| (l.clone(), k as u64))
        .collect();
    // first pass: collect constants used by instructions
    let mut consts = Consts::new(module_vals + nargs);
    let visit = |t: &Ty, v: &Val, consts: &mut Consts| {
        if is_const(v) {
            consts.id(t, v);
        }
    };
    for (_, insts) in &f.blocks {
        for i in insts {
            for (t, v) in operands(&i.op) {
                visit(&t, &v, &mut consts);
            }
            if let Op::Alloca(..) = i.op {
                visit(&Ty::Int(32), &Val::Int(1), &mut consts);
            }
        }
    }
    let first_inst = consts.base + consts.list.len() as u64;
    // result types: assign ids
    let mut next = first_inst;
    let mut res_ty: Vec<Option<Ty>> = Vec::new();
    for (_, insts) in &f.blocks {
        for i in insts {
            let rt = result_type(&i.op, types, gtys)?;
            if let (Some(n), Some(t)) = (&i.res, &rt) {
                locals.insert(n.clone(), (next, t.clone()));
            }
            if rt.is_some() {
                next += 1;
            }
            res_ty.push(rt);
        }
    }
    let val_id = |t: &Ty, v: &Val, consts: &Consts| -> R<u64> {
        match v {
            Val::Local(n) => locals
                .get(n)
                .map(|x| x.0)
                .ok_or_else(|| format!("unknown %{n}")),
            Val::Global(g) => gids.get(g).copied().ok_or_else(|| format!("unknown @{g}")),
            v => consts.get(t, v).ok_or_else(|| "const".to_string()),
        }
    };
    let mut recs = Vec::new();
    let mut attach = Vec::new();
    let mut inst_id = first_inst;
    let mut inst_index = 0u64;
    for (_, insts) in &f.blocks {
        for i in insts {
            let rel = |id: u64, inst_id: u64| inst_id.wrapping_sub(id);
            let mut ops: Vec<u64> = Vec::new();
            // push value (relative) and its type when it is a forward reference
            let pvt = |t: &Ty, v: &Val, ops: &mut Vec<u64>, types: &mut Types| -> R<()> {
                let id = val_id(t, v, &consts)?;
                ops.push(rel(id, inst_id));
                if id >= inst_id {
                    ops.push(types.id(t)?);
                }
                Ok(())
            };
            let pv = |t: &Ty, v: &Val, ops: &mut Vec<u64>| -> R<()> {
                let id = val_id(t, v, &consts)?;
                ops.push(rel(id, inst_id));
                Ok(())
            };
            let code: u32 = match &i.op {
                Op::Alloca(t, a) => {
                    ops.push(types.id(t)?);
                    ops.push(types.id(&Ty::Int(32))?);
                    ops.push(val_id(&Ty::Int(32), &Val::Int(1), &consts)?);
                    ops.push(align_enc(*a) | (1 << 6));
                    19
                }
                Op::Load(t, (pt, p), a, volatile) => {
                    pvt(pt, p, &mut ops, types)?;
                    ops.push(types.id(t)?);
                    ops.push(align_enc(*a));
                    ops.push(u64::from(*volatile));
                    20
                }
                Op::Store((vt, v), (pt, p), a) => {
                    pvt(pt, p, &mut ops, types)?;
                    pvt(vt, v, &mut ops, types)?;
                    ops.push(align_enc(*a));
                    ops.push(0);
                    44
                }
                Op::Gep(ib, t, (pt, p), idx) => {
                    ops.push(*ib as u64);
                    ops.push(types.id(t)?);
                    pvt(pt, p, &mut ops, types)?;
                    for (it, iv) in idx {
                        pvt(it, iv, &mut ops, types)?;
                    }
                    43
                }
                Op::Cast(op, (t, v), to) => {
                    pvt(t, v, &mut ops, types)?;
                    ops.push(types.id(to)?);
                    ops.push(match op.as_str() {
                        "trunc" => 0,
                        "zext" => 1,
                        "sext" => 2,
                        "fptrunc" => 7,
                        "fpext" => 8,
                        "inttoptr" => 10,
                        "bitcast" => 11,
                        "addrspacecast" => 12,
                        o => return Err(format!("cast {o}")),
                    });
                    3
                }
                Op::Bin(op, fl, t, a, b2) => {
                    pvt(t, a, &mut ops, types)?;
                    pv(t, b2, &mut ops)?;
                    ops.push(match op.as_str() {
                        "add" | "fadd" => 0,
                        "sub" | "fsub" => 1,
                        "mul" | "fmul" => 2,
                        "udiv" => 3,
                        "sdiv" | "fdiv" => 4,
                        "urem" => 5,
                        "srem" | "frem" => 6,
                        "shl" => 7,
                        "lshr" => 8,
                        "ashr" => 9,
                        "and" => 10,
                        "or" => 11,
                        "xor" => 12,
                        o => return Err(format!("binop {o}")),
                    });
                    let fl_v = if op.starts_with('f') {
                        fmf(fl)
                    } else {
                        (fl.iter().any(|x| x == "nuw") as u64)
                            | ((fl.iter().any(|x| x == "nsw") as u64) << 1)
                    };
                    if fl_v != 0 {
                        ops.push(fl_v);
                    }
                    2
                }
                Op::FNeg(fl, t, v) => {
                    pvt(t, v, &mut ops, types)?;
                    ops.push(0);
                    let f2 = fmf(fl);
                    if f2 != 0 {
                        ops.push(f2);
                    }
                    56
                }
                Op::Freeze(t, v) => {
                    pvt(t, v, &mut ops, types)?;
                    58
                }
                Op::Cmp(isf, fl, pred, t, a, b2) => {
                    pvt(t, a, &mut ops, types)?;
                    pv(t, b2, &mut ops)?;
                    ops.push(cmp_pred(*isf, pred)?);
                    let f2 = fmf(fl);
                    if *isf && f2 != 0 {
                        ops.push(f2);
                    }
                    28
                }
                Op::Select(fl, (ct, c), (at, a), (bt, b2)) => {
                    pvt(at, a, &mut ops, types)?;
                    pv(bt, b2, &mut ops)?;
                    pvt(ct, c, &mut ops, types)?;
                    let f2 = fmf(fl);
                    if f2 != 0 {
                        ops.push(f2);
                    }
                    29
                }
                Op::Phi(fl, t, inc) => {
                    ops.push(types.id(t)?);
                    for (v, l) in inc {
                        let id = val_id(t, v, &consts)?;
                        ops.push(signed(inst_id as i64 - id as i64));
                        ops.push(*bbs.get(l).ok_or_else(|| format!("block {l}"))?);
                    }
                    let f2 = fmf(fl);
                    if f2 != 0 {
                        ops.push(f2);
                    }
                    16
                }
                Op::Br(l) => {
                    ops.push(*bbs.get(l).ok_or_else(|| format!("block {l}"))?);
                    11
                }
                Op::CondBr(c, a, b2) => {
                    ops.push(*bbs.get(a).ok_or_else(|| format!("block {a}"))?);
                    ops.push(*bbs.get(b2).ok_or_else(|| format!("block {b2}"))?);
                    pv(&Ty::Int(1), c, &mut ops)?;
                    11
                }
                Op::Switch(t, v, d, cases) => {
                    ops.push(types.id(t)?);
                    pv(t, v, &mut ops)?;
                    ops.push(*bbs.get(d).ok_or_else(|| format!("block {d}"))?);
                    for (k, l) in cases {
                        ops.push(val_id(t, k, &consts)?);
                        ops.push(*bbs.get(l).ok_or_else(|| format!("block {l}"))?);
                    }
                    12
                }
                Op::Ret(v) => {
                    if let Some((t, v)) = v {
                        pvt(t, v, &mut ops, types)?;
                    }
                    10
                }
                Op::Unreachable => 15,
                Op::Call(fl, ra, rt, callee, args, g) => {
                    let fw = g.and_then(|g| groups.get(&g).cloned()).unwrap_or_default();
                    let ps: Vec<Vec<String>> = args.iter().map(|(_, a, _)| a.clone()).collect();
                    ops.push(attr_list(&fw, ra, &ps)?);
                    let f2 = fmf(fl);
                    ops.push((1 << 15) | if f2 != 0 { 1 << 17 } else { 0 });
                    if f2 != 0 {
                        ops.push(f2);
                    }
                    let fty = Ty::Func(
                        Box::new(rt.clone()),
                        args.iter().map(|(t, _, _)| t.clone()).collect(),
                    );
                    ops.push(types.id(&fty)?);
                    let cty = Ty::Ptr(Box::new(fty), 0);
                    pvt(&cty, &Val::Global(callee.clone()), &mut ops, types)?;
                    for (t, _, v) in args {
                        pv(t, v, &mut ops)?;
                    }
                    34
                }
                Op::ExtractElt((vt, v), (it, ix)) => {
                    pvt(vt, v, &mut ops, types)?;
                    pvt(it, ix, &mut ops, types)?;
                    6
                }
                Op::InsertElt((vt, v), (et, e), (it, ix)) => {
                    pvt(vt, v, &mut ops, types)?;
                    pv(et, e, &mut ops)?;
                    pvt(it, ix, &mut ops, types)?;
                    7
                }
                Op::Shuffle((at, a), (bt, b2), (mt, mk)) => {
                    pvt(at, a, &mut ops, types)?;
                    pv(bt, b2, &mut ops)?;
                    pv(mt, mk, &mut ops)?;
                    8
                }
                Op::ExtractVal((t, v), idx) => {
                    pvt(t, v, &mut ops, types)?;
                    ops.extend(idx.iter().map(|x| *x as u64));
                    26
                }
                Op::InsertVal((t, v), (et, e), idx) => {
                    pvt(t, v, &mut ops, types)?;
                    pvt(et, e, &mut ops, types)?;
                    ops.extend(idx.iter().map(|x| *x as u64));
                    27
                }
            };
            recs.push((code, ops));
            if !i.md.is_empty() {
                let mut a = vec![inst_index];
                for (k, n) in &i.md {
                    let kid = kinds
                        .iter()
                        .find(|(_, name)| name == k)
                        .map(|(id, _)| *id)
                        .ok_or_else(|| format!("metadata kind {k}"))?;
                    a.push(kid);
                    a.push(*md_index.get(n).ok_or_else(|| format!("metadata !{n}"))?);
                }
                attach.push(a);
            }
            if res_ty[inst_index as usize].is_some() {
                inst_id += 1;
            }
            inst_index += 1;
        }
    }
    Ok(FuncCode {
        nvals_before: module_vals,
        nargs,
        consts: consts.list,
        nblocks: f.blocks.len() as u64,
        recs,
        attach,
    })
}

fn emit_function(
    b: &mut Bits,
    fc: &FuncCode,
    types: &mut Types,
    gids: &HashMap<String, u64>,
) -> R<()> {
    b.enter(FUNCTION, 4);
    b.record(1, &[fc.nblocks]);
    if !fc.consts.is_empty() {
        let cbase = fc.consts_base();
        write_consts(b, &fc.consts, types, gids, cbase)?;
    }
    for (code, ops) in &fc.recs {
        b.record(*code, ops);
    }
    if !fc.attach.is_empty() {
        b.enter(MD_ATTACH, 3);
        for a in &fc.attach {
            b.record(11, a);
        }
        b.exit();
    }
    b.exit();
    Ok(())
}

impl FuncCode {
    fn consts_base(&self) -> u64 {
        self.nvals_before + self.nargs
    }
}

fn cmp_pred(isf: bool, p: &str) -> R<u64> {
    Ok(if isf {
        match p {
            "false" => 0,
            "oeq" => 1,
            "ogt" => 2,
            "oge" => 3,
            "olt" => 4,
            "ole" => 5,
            "one" => 6,
            "ord" => 7,
            "uno" => 8,
            "ueq" => 9,
            "ugt" => 10,
            "uge" => 11,
            "ult" => 12,
            "ule" => 13,
            "une" => 14,
            "true" => 15,
            p => return Err(format!("fcmp {p}")),
        }
    } else {
        match p {
            "eq" => 32,
            "ne" => 33,
            "ugt" => 34,
            "uge" => 35,
            "ult" => 36,
            "ule" => 37,
            "sgt" => 38,
            "sge" => 39,
            "slt" => 40,
            "sle" => 41,
            p => return Err(format!("icmp {p}")),
        }
    })
}

/// (type, value) operands of an instruction (for constant collection).
fn operands(op: &Op) -> Vec<(Ty, Val)> {
    let mut v: Vec<(Ty, Val)> = Vec::new();
    match op {
        Op::Alloca(..) | Op::Br(_) | Op::Unreachable => {}
        Op::Load(_, p, _, _) => v.push(p.clone()),
        Op::Store(a, p, _) => {
            v.push(p.clone());
            v.push(a.clone());
        }
        Op::Gep(_, _, p, idx) => {
            v.push(p.clone());
            v.extend(idx.iter().cloned());
        }
        Op::Cast(_, a, _) => v.push(a.clone()),
        Op::Bin(_, _, t, a, b) | Op::Cmp(_, _, _, t, a, b) => {
            v.push((t.clone(), a.clone()));
            v.push((t.clone(), b.clone()));
        }
        Op::FNeg(_, t, a) | Op::Freeze(t, a) => v.push((t.clone(), a.clone())),
        Op::Select(_, c, a, b) => {
            v.push(a.clone());
            v.push(b.clone());
            v.push(c.clone());
        }
        Op::Phi(_, t, inc) => v.extend(inc.iter().map(|(x, _)| (t.clone(), x.clone()))),
        Op::CondBr(c, _, _) => v.push((Ty::Int(1), c.clone())),
        Op::Switch(t, c, _, cases) => {
            v.push((t.clone(), c.clone()));
            v.extend(cases.iter().map(|(k, _)| (t.clone(), k.clone())));
        }
        Op::Ret(Some(a)) => v.push(a.clone()),
        Op::Ret(None) => {}
        Op::Call(_, _, _, _, args, _) => {
            v.extend(args.iter().map(|(t, _, x)| (t.clone(), x.clone())))
        }
        Op::ExtractElt(a, b) => {
            v.push(a.clone());
            v.push(b.clone());
        }
        Op::InsertElt(a, b, c) | Op::Shuffle(a, b, c) => {
            v.push(a.clone());
            v.push(b.clone());
            v.push(c.clone());
        }
        Op::ExtractVal(a, _) => v.push(a.clone()),
        Op::InsertVal(a, b, _) => {
            v.push(a.clone());
            v.push(b.clone());
        }
    }
    v
}

fn named_body(types: &Types, t: &Ty) -> Option<Vec<Ty>> {
    match t {
        Ty::Named(n) => types.named.get(n).cloned().flatten().map(|x| x.1),
        Ty::Struct(_, es) => Some(es.clone()),
        _ => None,
    }
}

fn index_type(types: &Types, t: &Ty, idx: u32) -> R<Ty> {
    match t {
        Ty::Array(_, e) | Ty::Vector(_, e) => Ok((**e).clone()),
        _ => named_body(types, t)
            .and_then(|b| b.get(idx as usize).cloned())
            .ok_or_else(|| "aggregate index".to_string()),
    }
}

fn result_type(op: &Op, types: &mut Types, gtys: &HashMap<String, Ty>) -> R<Option<Ty>> {
    let _ = gtys;
    Ok(match op {
        Op::Alloca(t, _) => Some(Ty::Ptr(Box::new(t.clone()), 0)),
        Op::Load(t, _, _, _) => Some(t.clone()),
        Op::Store(..)
        | Op::Br(_)
        | Op::CondBr(..)
        | Op::Switch(..)
        | Op::Ret(_)
        | Op::Unreachable => None,
        Op::Gep(_, t, (pt, _), idx) => {
            let sp = match pt {
                Ty::Ptr(_, s) => *s,
                _ => 0,
            };
            let mut cur = t.clone();
            for (k, (_, iv)) in idx.iter().enumerate().skip(1) {
                let _ = k;
                let n = match iv {
                    Val::Int(n) => *n as u32,
                    _ => 0,
                };
                cur = index_type(types, &cur, n)?;
            }
            Some(Ty::Ptr(Box::new(cur), sp))
        }
        Op::Cast(_, _, to) => Some(to.clone()),
        Op::Bin(_, _, t, _, _) | Op::FNeg(_, t, _) | Op::Freeze(t, _) => Some(t.clone()),
        Op::Cmp(_, _, _, t, _, _) => Some(match t {
            Ty::Vector(n, _) => Ty::Vector(*n, Box::new(Ty::Int(1))),
            _ => Ty::Int(1),
        }),
        Op::Select(_, _, (t, _), _) => Some(t.clone()),
        Op::Phi(_, t, _) => Some(t.clone()),
        Op::Call(_, _, rt, _, _, _) => {
            if *rt == Ty::Void {
                None
            } else {
                Some(rt.clone())
            }
        }
        Op::ExtractElt((t, _), _) => match t {
            Ty::Vector(_, e) => Some((**e).clone()),
            _ => return Err("extractelement type".into()),
        },
        Op::InsertElt((t, _), _, _) => Some(t.clone()),
        Op::Shuffle((t, _), _, (mt, _)) => match (t, mt) {
            (Ty::Vector(_, e), Ty::Vector(n, _)) => Some(Ty::Vector(*n, e.clone())),
            _ => return Err("shuffle type".into()),
        },
        Op::ExtractVal((t, _), idx) => {
            let mut cur = t.clone();
            for k in idx {
                cur = index_type(types, &cur, *k)?;
            }
            Some(cur)
        }
        Op::InsertVal((t, _), _, _) => Some(t.clone()),
    })
}
