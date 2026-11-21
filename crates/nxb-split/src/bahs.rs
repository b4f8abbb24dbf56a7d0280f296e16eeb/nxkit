//! agl BAHS shader archives (`bahs_shaders`, `bahs_variations`).

use std::collections::BTreeMap;

use crate::SplitError;
use crate::prim::{
    CS_MAGIC, CTL_MAGIC, Found, GFX_MAGIC, check, ctl_len, nvn2_ctl, sph_stage, u32le,
};

/// BAHS entry label to its program variation (`BAHS_VAR`).
pub type BahsVar = BTreeMap<String, String>;

/// `bahs_shaders` (with `bahs_variations` filling `var`).
pub fn bahs_shaders(d: &[u8], label: &str, var: &mut BahsVar) -> Result<Vec<Found>, SplitError> {
    let (rsz, region) = match (u32le(d, 0x18), u32le(d, 0x1c)) {
        (Some(a), Some(b)) => (a as usize, b as usize),
        _ => {
            return Err(SplitError::Red(format!(
                "{label} BAHS header: 0x8 bytes at 0x18 lie outside the {:#x} bytes there are",
                d.len()
            )));
        }
    };
    if region + rsz > d.len() {
        return Err(SplitError::Red(format!(
            "{label} BAHS code region out of bounds"
        )));
    }
    let mut found = Vec::new();
    let mut a = 0usize;
    while a + 16 <= region && a < region.saturating_sub(16) {
        let off = u32le(d, a).unwrap_or(0) as usize;
        let z = u32le(d, a + 4).unwrap_or(0);
        let csz = u32le(d, a + 8).unwrap_or(0) as usize;
        let rel = u32le(d, a + 12).unwrap_or(0) as i32 as i64;
        let c = a as i64 + rel;
        let a0 = a;
        a += 4;
        let in_region = c >= 0 && (c as usize) + csz <= region;
        let nvn2 = (0x48..0x10000).contains(&csz)
            && in_region
            && nvn2_ctl(&d[c as usize..c as usize + csz]);
        if z != 0
            || !(nvn2 || csz == 0x880 || csz == 0x88c || (0x760..0x880).contains(&csz))
            || !off.is_multiple_of(0x100)
        {
            continue;
        }
        if !in_region || d[c as usize..c as usize + 4] != CTL_MAGIC {
            continue;
        }
        let c = c as usize;
        let n = u32le(d, c + if nvn2 { 0x44 } else { 0x704 }).unwrap_or(0) as usize;
        if off + n > rsz {
            continue;
        }
        let code = &d[region + off..region + off + n];
        let ctl = &d[c..c + csz];
        let st = if nvn2 {
            crate::glslc::nvn2_stage(ctl, code)
        } else if code.get(..4) == Some(&CS_MAGIC[..]) {
            Some("cs")
        } else if code.get(..4) == Some(&GFX_MAGIC[..]) {
            sph_stage(code)
        } else {
            None
        };
        let Some(st) = st else { continue };
        if check(st, ctl, code).is_some() {
            continue;
        }
        found.push((
            a0,
            c,
            Found {
                place: format!("{label}#BAHS@{a0:#x}"),
                stage: st,
                ctl: ctl.to_vec(),
                code: code.to_vec(),
            },
        ));
    }
    // A program entry never lies inside a control section. The words +0x38..+0x47 of an NVN2 section (instruction
    // length, c1 start and end, code length) read as an entry whose control is the next program's and whose code is
    // whatever lies at that length. Each section counts with its own length (`ctl_len`), not the length an entry
    // gives it.
    let spans: Vec<(usize, usize)> = found
        .iter()
        .map(|(_, c, f)| (*c, c + ctl_len(&f.ctl)))
        .collect();
    let out: Vec<Found> = found
        .into_iter()
        .filter(|(a, _, _)| !spans.iter().any(|&(lo, hi)| lo <= *a && *a < hi))
        .map(|(_, _, f)| f)
        .collect();
    bahs_variations(d, label, &out, var);
    Ok(out)
}

fn bahs_variations(d: &[u8], label: &str, entries: &[Found], var: &mut BahsVar) {
    if entries.is_empty() {
        return;
    }
    let taken: std::collections::BTreeSet<usize> = entries
        .iter()
        .filter_map(|e| {
            e.place
                .rsplit_once('@')
                .and_then(|(_, h)| usize::from_str_radix(h.trim_start_matches("0x"), 16).ok())
        })
        .collect();
    let Some(&min) = taken.iter().next() else {
        return;
    };
    let Some(arr) = min.checked_sub(0x18) else {
        return;
    };
    if arr < 0x20 {
        return;
    }
    let (Some(asz), Some(acnt)) = (u32le(d, arr), u32le(d, arr + 4)) else {
        return;
    };
    let (asz, acnt) = (asz as usize, acnt as usize);
    let tol = (taken.len() / 100).max(1);
    if arr + asz + 8 > d.len() || acnt + tol < taken.len() {
        return;
    }
    let mut o = arr + 8;
    let mut recs = Vec::new();
    for _ in 0..acnt {
        if o + 0x20 > arr + asz {
            return;
        }
        let (Some(rs), Some(kind)) = (u32le(d, o), u32le(d, o + 4)) else {
            return;
        };
        if rs < 0x20 {
            return;
        }
        recs.push((o + 0x10, kind));
        o += rs as usize;
    }
    let in_recs: std::collections::BTreeSet<usize> = recs.iter().map(|r| r.0).collect();
    if o != arr + asz || taken.difference(&in_recs).count() > tol {
        return;
    }
    let (Some(psz), Some(pcnt)) = (u32le(d, o), u32le(d, o + 4)) else {
        return;
    };
    let (psz, pcnt) = (psz as usize, pcnt as usize);
    if o + psz > d.len() || pcnt == 0 {
        return;
    }
    let mut q = o + 8;
    let mut bases: Vec<usize> = Vec::new();
    for _ in 0..pcnt {
        if q + 16 > o + psz {
            return;
        }
        let (Some(rs), Some(base)) = (u32le(d, q), u32le(d, q + 12)) else {
            return;
        };
        let (rs, base) = (rs as usize, base as usize);
        if rs < 16 || base > acnt || bases.last().is_some_and(|&l| base < l) {
            return;
        }
        bases.push(base);
        q += rs;
    }
    if q != o + psz {
        return;
    }
    bases.push(acnt);
    for p in 0..pcnt {
        let mut seen = [0usize; 2];
        for &(a, k) in &recs[bases[p]..bases[p + 1]] {
            if k > 1 {
                continue;
            }
            if taken.contains(&a) {
                var.insert(
                    format!("{label}#BAHS@{a:#x}"),
                    format!("{label}#BAHS#prog{p}#var{}", seen[k as usize]),
                );
            }
            seen[k as usize] += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION: usize = 0x400;
    const CODE: usize = 0x100;

    fn put(d: &mut [u8], at: usize, words: &[u32]) {
        for (i, w) in words.iter().enumerate() {
            d[at + 4 * i..at + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
    }

    /// An NVN2 control section of 0xe0 bytes: +0x38 instruction length, +0x40 and +0x44 c1 start and end (the
    /// code area's length), +0x56 the stage byte (vertex).
    fn nvn2_section() -> Vec<u8> {
        let mut c = vec![0u8; 0xe0];
        c[..4].copy_from_slice(&CTL_MAGIC);
        c[4] = 0xc9;
        put(&mut c, 0x38, &[CODE as u32, 0, CODE as u32, CODE as u32]);
        c
    }

    /// Two vertex programs with entries at 0x100 and 0x238 and their control sections 0x68 after them. The words of
    /// the first control section at 0x1a0 read (0x100, 0, 0x100, 0x100): an entry whose control is the second
    /// program's and whose code is the second program's.
    fn bahs() -> Vec<u8> {
        let mut d = vec![0u8; REGION + 2 * CODE];
        put(&mut d, 0x18, &[2 * CODE as u32, REGION as u32]);
        for (entry, off) in [(0x100usize, 0usize), (0x238, CODE)] {
            put(&mut d, entry, &[off as u32, 0, 0xe0, 0x68]);
            d[entry + 0x68..entry + 0x68 + 0xe0].copy_from_slice(&nvn2_section());
            put(&mut d, REGION + off, &[0x000c_0481 + off as u32]);
        }
        d
    }

    #[test]
    fn control_words_are_no_entry() {
        let d = bahs();
        assert_eq!(&d[0x1a0..0x1a8], &[0, 1, 0, 0, 0, 0, 0, 0]);
        let mut var = BahsVar::new();
        let got = bahs_shaders(&d, "f.sharcb", &mut var).unwrap();
        let places: Vec<&str> = got.iter().map(|f| f.place.as_str()).collect();
        assert_eq!(places, ["f.sharcb#BAHS@0x100", "f.sharcb#BAHS@0x238"]);
        assert!(got.iter().all(|f| f.stage == "vs"));
        // The scan's own checks take the false entry: the rule is what leaves it out
        let ctl = &d[0x2a0..0x2a0 + CODE];
        assert!(nvn2_ctl(ctl) && check("vs", ctl, &d[REGION + CODE..REGION + 2 * CODE]).is_none());
    }
}
