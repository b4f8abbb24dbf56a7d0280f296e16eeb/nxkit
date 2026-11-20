//! Vertex inputs that the shader only uses as integers (`int_inputs`).
//!
//! The answer table declares every vertex attribute a float unless the job says otherwise. A shader that only bit
//! casts such an input to an integer reads an integer attribute. The input is declared `uint4` then, and the emitter
//! runs again. The `_bv` library keeps the float and other sign views of the attribute for a host that feeds another
//! format.

use super::super::usage::const_of;
use crate::ir::OperandType;
use crate::ir::agg::AggType;
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::props::Props;
use crate::structured::StructInfo;
use crate::structured::ast::{A, Kind};
use crate::structured::walk::for_each_node;
use std::collections::{BTreeMap, BTreeSet};

/// One conversion the emitter made: the node, the requested type and the type the node has.
pub type Use = (A, AggType, AggType);

/// The attribute a load node reads, and whether the access is a plain constant component.
fn attr_load(info: &StructInfo, a: A) -> Option<(i32, bool)> {
    let Kind::Operation(o) = info.ast.kind(a) else {
        return None;
    };
    if Inst(o.inst.0 & Inst::MASK) != Inst::Load || o.storage_kind != StorageKind::Input {
        return None;
    }
    let k = |i: usize| o.sources.get(i).and_then(|s| const_of(&info.ast, *s));
    if k(0) != Some(IoVariable::UserDefined.0 as i32) {
        return None;
    }
    let loc = k(1)?;
    Some((loc, o.sources.len() == 3 && k(2).is_some()))
}

/// The locations of the float attributes that the shader only uses as integers. `declared` holds the locations the
/// job names. `float` tells whether a location is declared a float.
pub fn find(
    info: &StructInfo,
    uses: &[Use],
    declared: &BTreeSet<u32>,
    float: &dyn Fn(u32) -> bool,
) -> Vec<u32> {
    let mut bad: BTreeSet<i32> = BTreeSet::new();
    let mut decl_loads: BTreeMap<A, i32> = BTreeMap::new();
    let mut alias: BTreeMap<A, i32> = BTreeMap::new();
    let mut assigns: BTreeMap<A, u32> = BTreeMap::new();
    let mut copies: BTreeMap<A, i64> = BTreeMap::new();
    let mut seen: BTreeSet<i32> = BTreeSet::new();
    for_each_node(info, &mut |a, k| {
        if let Some((l, plain)) = attr_load(info, a) {
            seen.insert(l);
            if !plain {
                bad.insert(l);
            }
        }
        let Kind::Assign { dest, src } = k else {
            return;
        };
        *assigns.entry(*dest).or_default() += 1;
        if let Some((l, _)) = attr_load(info, *src) {
            decl_loads.insert(*src, l);
            alias.insert(*dest, l);
        } else if matches!(
            info.ast.kind(*src),
            Kind::Operand {
                ty: OperandType::LocalVariable,
                ..
            }
        ) {
            *copies.entry(*src).or_default() += 1;
        }
    });
    for (v, l) in &alias {
        if assigns.get(v).copied().unwrap_or(0) != 1 {
            bad.insert(*l);
        }
    }
    let (mut ints, mut floats): (BTreeMap<i32, i64>, BTreeMap<i32, i64>) = Default::default();
    for (a, req, have) in uses {
        let (loc, decl) = match (decl_loads.get(a), alias.get(a), attr_load(info, *a)) {
            (Some(l), _, _) => (*l, true),
            (_, Some(l), _) => (*l, false),
            (_, _, Some((l, _))) => {
                if *have == AggType::FP32 && *req == AggType::FP32 {
                    bad.insert(l);
                }
                (l, false)
            }
            _ => continue,
        };
        if *have != AggType::FP32 {
            continue;
        }
        if *req == AggType::S32 || *req == AggType::U32 || *req == AggType::BOOL {
            *ints.entry(loc).or_default() += 1;
        } else if *req == AggType::FP32 {
            if !decl {
                *floats.entry(loc).or_default() += 1;
            }
        } else {
            bad.insert(loc);
        }
    }
    for (v, n) in &copies {
        if let Some(l) = alias.get(v) {
            *floats.entry(*l).or_default() -= n;
        }
    }
    let mut out = Vec::new();
    for l in seen {
        let Ok(u) = u32::try_from(l) else { continue };
        if !declared.contains(&u)
            && float(u)
            && !bad.contains(&l)
            && ints.get(&l).copied().unwrap_or(0) > 0
            && floats.get(&l).copied().unwrap_or(0) <= 0
        {
            out.push(u);
        }
    }
    out
}

/// The properties with the inputs at `locs` declared `uint`.
pub fn declare(props: &Props, locs: &[u32]) -> Props {
    let mut p = props.clone();
    for l in locs {
        if let Some(t) = p.definitions.attribute_types.get_mut(*l as usize) {
            *t = 2;
        }
    }
    p
}
