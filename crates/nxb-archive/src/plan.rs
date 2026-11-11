//! The planning half of `pso_offline.py`: shader pairs to pipeline bases as the host's prebuild makes them
//! (`pre_desc` in `nvn_mtl.m`), the Metal 4 pipelines script, the prebuild stamp and the side tables.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use nxb_pyjson::Value;

const LAYER: &[u8] = b"render_target_array_index";
const COLOUR: [&str; 9] = [
    "pixel_format",
    "write_mask",
    "blending_state",
    "alpha_blend_operation",
    "rgb_blend_operation",
    "destination_alpha_blend_factor",
    "destination_rgb_blend_factor",
    "source_alpha_blend_factor",
    "source_rgb_blend_factor",
];

/// `pair_text`: the pair lists' bytes one after the other (missing files skipped).
pub fn pair_text(paths: &[PathBuf]) -> Vec<u8> {
    paths
        .iter()
        .filter(|p| p.is_file())
        .filter_map(|p| fs::read(p).ok())
        .flatten()
        .collect()
}

/// `read_pairs`: (vertex key, pixel key) per line, in order, no empty lines, comments or repeated lines.
pub fn read_pairs(text: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(text);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() || line.starts_with('#') || !seen.insert(line.to_string()) {
            continue;
        }
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() >= 2 {
            out.push((c[0].to_string(), c[1].to_string()));
        }
    }
    out
}

/// `stamp`: the host's prebuild stamp (`prebuilt.tsv`).
pub fn stamp(chain: &str, zero: bool, text: &[u8]) -> String {
    let h = nxb_model::sha256(text).to_hex();
    format!(
        "chain\t{chain}\ndepth\t{}\npairs\t{}\t{}\n",
        if zero { "near=0" } else { "near=-w" },
        text.len(),
        &h[..16]
    )
}

fn u32le(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64le(b: &[u8], o: usize) -> Option<u64> {
    let s = b.get(o..o + 8)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

fn tags(b: &[u8], mut p: usize, end: usize) -> Option<HashMap<[u8; 4], &[u8]>> {
    let mut t = HashMap::new();
    while p + 4 <= end {
        let tag: [u8; 4] = b.get(p..p + 4)?.try_into().ok()?;
        p += 4;
        if &tag == b"ENDT" {
            return Some(t);
        }
        let n = b
            .get(p..p + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)?;
        p += 2;
        // Python slices: a short read gives a short value, not an error
        t.insert(tag, b.get(p..(p + n).min(b.len())).unwrap_or(&[]));
        p += n;
    }
    None
}

/// `functions`: {entry name: declares function constants} from a `.metallib`'s first MiB; `None` when unreadable.
pub fn functions(path: &Path) -> Option<Vec<(String, bool)>> {
    let mut b = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(1 << 20)
        .read_to_end(&mut b)
        .ok()?;
    if b.get(..4) != Some(b"MTLB") || b.len() < 0x58 {
        return None;
    }
    let (fo, po, ps) = (
        u64le(&b, 0x18)? as usize,
        u64le(&b, 0x28)? as usize,
        u64le(&b, 0x30)? as usize,
    );
    let count = u32le(&b, fo)? as usize;
    let mut p = fo + 4;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let size = u32le(&b, p)? as usize;
        let t = tags(&b, p + 4, p + size)?;
        let name = t.get(b"NAME")?;
        let end = name.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1);
        names.push(String::from_utf8(name[..end].to_vec()).ok()?);
        p += size;
    }
    let mut out = Vec::with_capacity(count);
    let mut p = po;
    for name in names {
        let size = u32le(&b, p)? as usize;
        let t = tags(&b, p + 4, p + 4 + size)?;
        out.push((name, t.contains_key(b"CNST")));
        p += 4 + size;
    }
    if p != po + ps {
        return None;
    }
    Some(out)
}

/// One base: (vertex library, vertex entry, pixel library, pixel entry).
pub type PlannedBase = (String, String, String, String);

/// What [`plan`] found.
#[derive(Debug, Default)]
pub struct Plan {
    /// The bases, in pair order, no repeats.
    pub bases: Vec<PlannedBase>,
    /// Pairs left to the device (function constants).
    pub rest: Vec<(String, String)>,
    /// Pairs left out: reflection, mesh, stage inputs, library, layer, primitive ID, entry.
    pub left: [usize; 7],
}

/// `plan`: the bases of the archive, one per pair, as the host asks for them, and the host looks a base up by exactly
/// those functions (`nxkit-metal` `cache::specialize`). A vertex shader whose reflection has a `pull` field at slot 16
/// is drawn through its `_bp` library and the field's `entry` (`main0` without one): it reads its inputs itself, so its
/// pipelines have no vertex layout (`nxkit-gpu` `pull`, on with `NXKIT_PULL=1`). Every other vertex shader is drawn
/// with its `_b` library and its `entry`. A vertex shader with stage inputs and no `pull` field needs the vertex layout
/// of the draw, which a base cannot have, and is left out (`left[2]`). `entry_z` is not used.
/// `_zero` (depth mode `zero`) stays for the callers; the stamp records it.
pub fn plan(lib_dir: &Path, chain: &str, pairs: &[(String, String)], zero: bool) -> Plan {
    plan_with(lib_dir, chain, pairs, zero, false)
}

/// [`plan`], and with `with_inputs` a vertex shader with stage inputs and no `pull` field is accepted as well (its
/// base is the `_b` library and the `entry`). The bases of recorded states with a vertex layout ([`crate::seen`]) are
/// checked this way: the checks are the same as for a plain base, only the missing vertex layout is not held against
/// the pair.
pub fn plan_with(
    lib_dir: &Path,
    chain: &str,
    pairs: &[(String, String)],
    _zero: bool,
    with_inputs: bool,
) -> Plan {
    let mut js: HashMap<String, Option<Value>> = HashMap::new();
    let mut fns: HashMap<String, Option<Vec<(String, bool)>>> = HashMap::new();
    let mut layer: HashMap<String, bool> = HashMap::new();
    let mut out = Plan::default();
    let mut seen = HashSet::new();
    let refl = |js: &mut HashMap<String, Option<Value>>, key: &str| -> Option<Value> {
        js.entry(key.to_string())
            .or_insert_with(|| {
                let t = fs::read(lib_dir.join(format!("{key}_b.json"))).ok()?;
                nxb_pyjson::parse(&String::from_utf8_lossy(&t)).ok()
            })
            .clone()
    };
    for (vk, fk) in pairs {
        let (vj, fj) = (refl(&mut js, vk), refl(&mut js, fk));
        let chain_of = |j: &Value| j.get("chain").and_then(Value::as_str).map(str::to_string);
        let (Some(vj), Some(fj)) = (vj, fj) else {
            out.left[0] += 1;
            continue;
        };
        if chain_of(&vj).as_deref() != Some(chain) || chain_of(&fj).as_deref() != Some(chain) {
            out.left[0] += 1;
            continue;
        }
        // A mesh shader's pipelines are built at the draw, not from archive bases.
        if matches!(vj.get("mesh_stage"), Some(Value::Object(m)) if !m.is_empty()) {
            out.left[1] += 1;
            continue;
        }
        let truthy_str = |v: Option<&Value>| {
            v.and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let ventry = vj.get("entry").and_then(Value::as_str).map(str::to_string);
        let fentry = truthy_str(fj.get("entry")).unwrap_or_else(|| "main0".into());
        let has_inputs = match vj.get("vertex_inputs") {
            Some(Value::List(l)) => !l.is_empty(),
            Some(Value::Object(m)) => !m.is_empty(),
            Some(Value::Str(s)) => !s.is_empty(),
            Some(Value::Int(i)) => *i != 0,
            Some(Value::Bool(b)) => *b,
            _ => false,
        };
        let pull = pull_entry(&vj);
        if has_inputs && pull.is_none() && !with_inputs {
            out.left[2] += 1;
            continue;
        }
        let prim = match fj.get("raster") {
            Some(r @ Value::Object(_)) => match r.get("primitive_id") {
                Some(Value::Bool(b)) => *b,
                Some(Value::Int(i)) => *i != 0,
                Some(Value::Null) | None => false,
                Some(_) => true,
            },
            _ => false,
        };
        if prim {
            out.left[5] += 1;
            continue;
        }
        let (vl, ventry) = match pull {
            Some(e) => (format!("{vk}_bp"), Some(e)),
            None => (format!("{vk}_b"), ventry),
        };
        let fl = format!("{fk}_b");
        let vf = fns
            .entry(vl.clone())
            .or_insert_with(|| functions(&lib_dir.join(format!("{vl}.metallib"))))
            .clone();
        let ff = fns
            .entry(fl.clone())
            .or_insert_with(|| functions(&lib_dir.join(format!("{fl}.metallib"))))
            .clone();
        let (Some(vf), Some(ff)) = (vf, ff) else {
            out.left[3] += 1;
            continue;
        };
        let writes = *layer.entry(vl.clone()).or_insert_with(|| {
            fs::read(lib_dir.join(format!("{vl}.metallib")))
                .is_ok_and(|b| b.windows(LAYER.len()).any(|w| w == LAYER))
        });
        if writes {
            out.left[4] += 1;
            continue;
        }
        let find =
            |f: &[(String, bool)], e: &str| f.iter().rev().find(|(n, _)| n == e).map(|x| x.1);
        let ve = ventry.unwrap_or_default();
        let (Some(vc), Some(fc)) = (find(&vf, &ve), find(&ff, &fentry)) else {
            out.left[6] += 1;
            continue;
        };
        if vc || fc {
            out.rest.push((vk.clone(), fk.clone()));
            continue;
        }
        let b = (vl, ve, fl, fentry);
        if seen.insert(b.clone()) {
            out.bases.push(b);
        }
    }
    out
}

/// The entry of the `_bp` library that the host draws a vertex shader with: the `entry` of its `pull` field, `main0`
/// when the field has none, `None` without a field or for a field at another slot than 16 (`nxkit-gpu`
/// `programs::pull_entry`).
pub fn pull_entry(vj: &Value) -> Option<String> {
    let p = match vj.get("pull") {
        Some(p @ Value::Object(_)) => p,
        _ => return None,
    };
    if !matches!(p.get("slot"), Some(Value::Int(16))) {
        return None;
    }
    Some(
        p.get("entry")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
            .unwrap_or("main0")
            .to_string(),
    )
}

/// `script`: the Metal 4 pipelines script of the bases, written as `json.dump(indent=1)` does.
pub fn script_text(lib_dir: &Path, bases: &[PlannedBase]) -> String {
    script_text_with(lib_dir, bases, &[])
}

/// `script` with compute pipelines: one compute descriptor per (library, entry), naming the function alone. The
/// `compute_pipeline_descriptors` key is written only when there are compute pipelines.
pub fn script_text_with(
    lib_dir: &Path,
    bases: &[PlannedBase],
    computes: &[crate::compute::ComputePipe],
) -> String {
    script_text_all(lib_dir, bases, &[], computes)
}

/// [`script_text_with`] and the bases of recorded states ([`crate::seen`]) after the plain bases. With no states the
/// text is the same as [`script_text_with`] gives.
pub fn script_text_all(
    lib_dir: &Path,
    bases: &[PlannedBase],
    states: &[crate::seen::StateBase],
    computes: &[crate::compute::ComputePipe],
) -> String {
    let dir = fs::canonicalize(lib_dir).unwrap_or_else(|_| lib_dir.to_path_buf());
    let (mut libs, mut fns, mut pipes) = (Vec::new(), Vec::new(), Vec::new());
    let (mut seen_l, mut seen_f) = (HashSet::new(), HashSet::new());
    let mut fd = |lib: &str, entry: &str, libs: &mut Vec<Value>, fns: &mut Vec<Value>| {
        if seen_l.insert(lib.to_string()) {
            libs.push(Value::Object(vec![
                ("label".into(), Value::Str(lib.into())),
                (
                    "path".into(),
                    Value::Str(
                        dir.join(format!("{lib}.metallib"))
                            .to_string_lossy()
                            .into_owned(),
                    ),
                ),
            ]));
        }
        let label = format!("{lib}.{entry}");
        if seen_f.insert(label.clone()) {
            fns.push(Value::Object(vec![
                ("label".into(), Value::Str(label.clone())),
                ("library".into(), Value::Str(format!("lib:{lib}"))),
                ("name".into(), Value::Str(entry.into())),
            ]));
        }
        Value::Str(format!("fnd:{label}"))
    };
    let colour = Value::Object(
        COLOUR
            .iter()
            .map(|k| (k.to_string(), Value::Str("Unspecialized".into())))
            .collect(),
    );
    for (vl, ve, fl, fe) in bases {
        let v = fd(vl, ve, &mut libs, &mut fns);
        let f = fd(fl, fe, &mut libs, &mut fns);
        pipes.push(Value::Object(vec![
            ("vertex_function_descriptor".into(), v),
            ("fragment_function_descriptor".into(), f),
            (
                "color_attachments".into(),
                Value::List(vec![colour.clone(); 8]),
            ),
        ]));
    }
    for st in states {
        let v = fd(&st.vertex.0, &st.vertex.1, &mut libs, &mut fns);
        let f = fd(&st.fragment.0, &st.fragment.1, &mut libs, &mut fns);
        pipes.push(st.descriptor(v, f, &colour));
    }
    let mut descriptors = vec![("render_pipeline_descriptors".into(), Value::List(pipes))];
    if !computes.is_empty() {
        let list = computes
            .iter()
            .map(|(lib, entry)| {
                let f = fd(lib, entry, &mut libs, &mut fns);
                Value::Object(vec![("compute_function_descriptor".into(), f)])
            })
            .collect();
        descriptors.push(("compute_pipeline_descriptors".into(), Value::List(list)));
    }
    let v = Value::Object(vec![
        ("libraries".into(), Value::List(libs)),
        (
            "function_descriptors".into(),
            Value::Object(vec![(
                "library_function_descriptors".into(),
                Value::List(fns),
            )]),
        ),
        ("pipeline_descriptors".into(), Value::Object(descriptors)),
    ]);
    nxb_pyjson::dumps_indent(&v, 1)
}

/// `archive.tsv`.
pub fn archive_tsv(
    archs: &[String],
    platform: &str,
    osmin: &str,
    toolchain: &str,
    bases: usize,
    rest: usize,
    computes: usize,
) -> String {
    format!(
        "# the pipeline archive made offline (dm/gfx/tools/pso_offline.py); the host reads prebuilt.tsv, not this file\narch\t{}\nplatform\t{platform}\t{osmin}\ntoolchain\t{toolchain}\nbases\t{bases}\nrest\t{rest}\ncompute\t{computes}\n",
        archs.join(" ")
    )
}

/// `rest_pairs.tsv` (None when there are no such pairs: the file is removed).
pub fn rest_tsv(rest: &[(String, String)]) -> Option<String> {
    if rest.is_empty() {
        return None;
    }
    Some(rest.iter().map(|(v, f)| format!("{v}\t{f}\n")).collect())
}

/// `arch_name`: `g18p` → `applegpu_g18p`.
pub fn arch_name(a: &str) -> String {
    let a = a.trim();
    if a.starts_with("applegpu_") {
        a.to_string()
    } else {
        format!("applegpu_{a}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_and_stamp() {
        let t = b"# x\na\tb\n\na\tb\nc\td\textra\ne\n";
        assert_eq!(
            read_pairs(t),
            vec![("a".into(), "b".into()), ("c".into(), "d".into())]
        );
        let s = stamp("0ad559cdbb28", false, t);
        assert!(s.starts_with("chain\t0ad559cdbb28\ndepth\tnear=-w\npairs\t25\t"));
        assert_eq!(arch_name(" g14s"), "applegpu_g14s");
        assert_eq!(rest_tsv(&[]), None);
    }
}
