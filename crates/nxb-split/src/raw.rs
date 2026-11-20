//! `raw_shaders`: control sections in main that no GLSLC output or BNSH file holds, with their code areas found by the
//! relocations or the `adrp` + `add` pairs that build their addresses (`rel_targets`, `adrp_targets`).

use std::collections::{HashMap, HashSet};

use crate::SplitError;
use crate::glslc::{layout, stage};
use crate::prim::{CTL_MAGIC, Found, check, find_all};
use crate::rd::{u32le, u64le};

type R<T> = Result<T, SplitError>;

/// Ordered map: Python dicts keep the first insertion's place and the last value.
#[derive(Default)]
struct Rel {
    keys: Vec<u64>,
    val: HashMap<u64, u64>,
}

impl Rel {
    fn put(&mut self, k: u64, v: u64) {
        if self.val.insert(k, v).is_none() {
            self.keys.push(k);
        }
    }
}

/// `rel_targets`: a module image's relative relocations (slot to target).
fn rel_targets(img: &[u8]) -> R<Rel> {
    let m = u32le(img, 4)?;
    let o = (m as i64 + i64::from(u32le(img, m + 4)? as u32 as i32)) as usize;
    let mut t: HashMap<i64, u64> = HashMap::new();
    let mut o = o;
    while o + 16 <= img.len() {
        let (tag, v) = (u64le(img, o)? as i64, u64le(img, o + 8)?);
        o += 16;
        if tag == 0 {
            break;
        }
        t.entry(tag).or_insert(v);
    }
    let mut out = Rel::default();
    if let Some(&base) = t.get(&7) {
        for i in (0..t.get(&8).copied().unwrap_or(0) as usize).step_by(24) {
            let at = base as usize + i;
            let (off, info, add) = (u64le(img, at)?, u64le(img, at + 8)?, u64le(img, at + 16)?);
            if info & 0xffff_ffff == 0x403 {
                out.put(off, add);
            }
        }
    }
    let mut nxt = 0u64;
    if let Some(&base) = t.get(&36) {
        for i in (0..t.get(&35).copied().unwrap_or(0) as usize).step_by(8) {
            let e = u64le(img, base as usize + i)?;
            if e & 1 == 0 {
                out.put(e, u64le(img, e as usize)?);
                nxt = e + 8;
                continue;
            }
            for b in 0..63 {
                if e >> (b + 1) & 1 == 1 {
                    let s = nxt + 8 * b;
                    out.put(s, u64le(img, s as usize)?);
                }
            }
            nxt += 8 * 63;
        }
    }
    Ok(out)
}

/// `adrp_targets`: addresses in `[lo, hi)` that the code builds with `adrp` + `add` on one register, with the offsets of the adds.
fn adrp_targets(d: &[u8], lo: i64, hi: i64) -> HashMap<i64, Vec<i64>> {
    let w: Vec<u32> = d
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    for (i, &x) in w.iter().enumerate() {
        if x & 0x9f00_0000 != 0x9000_0000 {
            continue;
        }
        let rd = x & 31;
        let imm = i64::from(((x >> 5) & 0x7ffff) << 2 | (x >> 29) & 3);
        let page = ((i as i64 * 4) & !0xfff) + (imm << 12);
        if !(lo - 0x1000 <= page && page < hi) {
            continue;
        }
        for (j, &y) in w.iter().skip(i + 1).take(8).enumerate() {
            if y & 0xffc0_0000 == 0x9100_0000 && (y >> 5) & 31 == rd {
                let t = page + i64::from((y >> 10) & 0xfff);
                if lo <= t && t < hi {
                    out.entry(t)
                        .or_default()
                        .push((i as i64 + 1 + j as i64) * 4);
                }
                break;
            }
        }
    }
    out
}

/// `raw_shaders`. `known`: control sections the other readers took from this main, left out.
pub fn raw_shaders(d: &[u8], label: &str, known: &HashSet<&[u8]>) -> R<Vec<Found>> {
    let mut skip: Vec<(usize, usize)> = Vec::new();
    for h in find_all(d, &[0x91, 0x68, 0x86, 0x19]) {
        let (size, _, first, _) = layout(d, h);
        let min = if first == 0xa8 { 0 } else { 0x760 };
        if min < size && size <= (d.len() - h) as u64 {
            skip.push((h, h + size as usize));
        }
    }
    for h in find_all(d, b"BNSH\0\0\0\0") {
        skip.push((h, h + u32le(d, h + 0x1c)?));
    }
    let mut ctls = Vec::new();
    for c in find_all(d, &CTL_MAGIC) {
        if c & 3 != 0 || skip.iter().any(|&(a, b)| a <= c && c < b) {
            continue;
        }
        let n = u32le(d, c + 0x20)?;
        if !known.contains(d.get(c..c + n).unwrap_or(&d[c.min(d.len())..])) {
            ctls.push(c);
        }
    }
    if ctls.is_empty() {
        return Ok(Vec::new());
    }
    let rel = rel_targets(d)?;
    let cset: HashSet<usize> = ctls.iter().copied().collect();
    let mut code_of: HashMap<usize, u64> = HashMap::new();
    for &s in &rel.keys {
        let c = rel.val[&s];
        if cset.contains(&(c as usize))
            && s >= 8
            && rel.val.contains_key(&(s - 8))
            && !code_of.contains_key(&(c as usize))
            && u32le(d, s as usize + 8)? == u32le(d, c as usize + 0x704)?
        {
            code_of.insert(c as usize, rel.val[&(s - 8)]);
        }
    }
    let built = if ctls.iter().any(|c| !code_of.contains_key(c)) {
        let (mn, mx) = (
            ctls.iter().min().copied().unwrap_or(0),
            ctls.iter().max().copied().unwrap_or(0),
        );
        adrp_targets(d, mn as i64 - 0x10_0000, mx as i64 + 0x10_0000)
    } else {
        HashMap::new()
    };
    let mut out = Vec::new();
    for &c in &ctls {
        let (nctl, n) = (u32le(d, c + 0x20)?, u32le(d, c + 0x704)?);
        let ctl = d.get(c..c + nctl).unwrap_or(&d[c.min(d.len())..]);
        let near = |k: i64| -> i64 {
            let (a, b) = (&built[&k], &built[&(c as i64)]);
            a.iter()
                .flat_map(|x| b.iter().map(move |y| (x - y).abs()))
                .min()
                .unwrap_or(i64::MAX)
        };
        let cands: Vec<i64> = if let Some(&k) = code_of.get(&c) {
            vec![k as i64]
        } else {
            let mut v: Vec<i64> = [((c + nctl + 15) & !15) as i64, c as i64 - n as i64]
                .into_iter()
                .filter(|k| {
                    built.contains_key(k) && built.contains_key(&(c as i64)) && near(*k) < 0x400
                })
                .collect();
            v.sort_by_key(|&k| near(k));
            v
        };
        let mut why = Some(
            "no code area: no data record points at one, and the code builds the address of none right after or right before it",
        );
        for k in cands {
            let code = if k >= 0 {
                d.get(k as usize..k as usize + n)
                    .unwrap_or(&d[(k as usize).min(d.len())..])
            } else {
                &[][..]
            };
            let st = if k >= 0 && !code.windows(4).any(|w| w == CTL_MAGIC) {
                stage(ctl, code)
            } else {
                None
            };
            if let Some(st) = st
                && check(st, ctl, code).is_none()
            {
                out.push(Found {
                    place: format!("{label}#RAW@{c:#x}"),
                    stage: st,
                    ctl: ctl.to_vec(),
                    code: code.to_vec(),
                });
                why = None;
                break;
            }
        }
        if let Some(w) = why {
            println!("[RED] {label} control section @{c:#x}: {w}; left out");
        }
    }
    Ok(out)
}
