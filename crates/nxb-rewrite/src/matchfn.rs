//! Conservative function matching for opt-in patches across game updates (`dm/tools/nso_match.py`).

use crate::branch::{relative, signed, unhex};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use nxb_a64::decode;
use nxb_model::sha256;

use crate::patches::{FnRule, Rule};
use crate::scan::fde_ranges;

/// A matching failure (the script's `MatchError`).
pub(crate) type MErr = String;

/// The word some compilers leave after a call that does not return. It is no AArch64 instruction, and it is read as `brk #0`.
const TRAP_WORD: u32 = 0xe7ff_defe;
const BRK: u32 = 0xd420_0000;

/// The module image with function boundaries and fingerprints.
pub(crate) struct MatchImage<'a> {
    img: &'a [u8],
    text_end: i64,
    order: Vec<(i64, i64)>,
    functions: HashMap<i64, i64>,
    bodies: HashMap<i64, (String, Vec<(i64, i64)>)>,
    fps: HashMap<i64, Result<String, MErr>>,
    imports: Option<HashMap<i64, String>>,
}

enum Callee {
    Import(String),
    Svc(u32),
    Function(i64, String),
}

fn le32(img: &[u8], o: i64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    img.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le64(img: &[u8], o: i64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    img.get(o..o + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn hex(d: &[u8]) -> String {
    d.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

impl<'a> MatchImage<'a> {
    /// Builds the image view, an error is the script's `MatchError` text.
    pub(crate) fn new(img: &'a [u8], text_end: i64) -> Result<Self, MErr> {
        let ranges =
            fde_ranges(img).map_err(|e| format!("cannot read function boundaries: {e}"))?;
        let mut functions = HashMap::new();
        let mut order = Vec::new();
        for (start, end) in ranges {
            if !(0 <= start && start < end && end <= text_end.min(img.len() as i64))
                || start % 4 != 0
                || end % 4 != 0
            {
                continue;
            }
            if let Some(e) = functions.get(&start) {
                if *e != end {
                    return Err(format!("conflicting function boundaries at {start:#x}"));
                }
            } else {
                functions.insert(start, end);
                order.push((start, end));
            }
        }
        Ok(MatchImage {
            img,
            text_end,
            order,
            functions,
            bodies: HashMap::new(),
            fps: HashMap::new(),
            imports: None,
        })
    }

    fn body(&mut self, start: i64) -> Result<(String, Vec<(i64, i64)>), MErr> {
        if let Some(b) = self.bodies.get(&start) {
            return Ok(b.clone());
        }
        let end = self.functions[&start];
        let n = ((end - start) / 4) as usize;
        let words: Vec<u32> = (0..n)
            .map(|i| le32(self.img, start + 4 * i as i64).unwrap_or(0))
            .collect();
        let mut ins = Vec::with_capacity(n);
        for (i, w) in words.iter().enumerate() {
            // the digest below still covers the word itself
            let read = if *w == TRAP_WORD { BRK } else { *w };
            match decode(read, (start + 4 * i as i64) as u64) {
                Some(x) => ins.push(x),
                None => return Err(format!("undecodable function at {start:#x}")),
            }
        }
        let mut norm = words.clone();
        let mut branches = Vec::new();
        for i in 0..n {
            let w = words[i];
            let addr = start + 4 * i as i64;
            if let Some((mask, delta)) = relative(w) {
                let target = addr + delta;
                if !(start <= target && target < end) {
                    norm[i] &= !mask;
                    branches.push((i as i64 * 4, target));
                }
            } else if w & 0x1f00_0000 == 0x1000_0000 {
                let delta = signed(
                    i64::from(((w >> 29) & 3) | (((w >> 5) & 0x7_ffff) << 2)),
                    21,
                );
                if w & 0x8000_0000 != 0 || !(start <= addr + delta && addr + delta < end) {
                    norm[i] &= 0x9f00_001f;
                }
            } else if w & 0x3b00_0000 == 0x1800_0000 {
                let target = addr + signed(i64::from((w >> 5) & 0x7_ffff), 19) * 4;
                if !(start <= target && target < end) {
                    norm[i] &= 0xff00_001f;
                }
            }
            if w & 0x9f00_0000 != 0x9000_0000 {
                continue;
            }
            let rd = w & 31;
            for j in i + 1..(i + 9).min(n) {
                let u = words[j];
                if (u & 0xffc0_0000 == 0x9100_0000 || u & 0x3b00_0000 == 0x3900_0000)
                    && (u >> 5) & 31 == rd
                {
                    norm[j] &= !0x3f_fc00;
                    break;
                }
                if relative(u).is_some() || matches!(ins[j].mn, "br" | "blr" | "ret") {
                    break;
                }
                let names = [
                    format!("x{rd}"),
                    format!("w{rd}"),
                    match rd {
                        29 => "fp".into(),
                        30 => "lr".into(),
                        _ => String::new(),
                    },
                ];
                if ins[j].writes.iter().any(|r| names.contains(&r.name())) {
                    break;
                }
            }
        }
        let bytes: Vec<u8> = norm.iter().flat_map(|w| w.to_le_bytes()).collect();
        let digest = hex(&sha256(&bytes).0);
        self.bodies
            .insert(start, (digest.clone(), branches.clone()));
        Ok((digest, branches))
    }

    fn import_slots(&mut self) -> &HashMap<i64, String> {
        if self.imports.is_none() {
            self.imports = Some(self.read_imports().unwrap_or_default());
        }
        self.imports.get_or_insert_with(HashMap::new)
    }

    fn read_imports(&self) -> Option<HashMap<i64, String>> {
        let img = self.img;
        let mod0 = i64::from(le32(img, 4)?);
        let dyn_ = mod0 + i64::from(le32(img, mod0 + 4)? as i32);
        let mut tags: HashMap<i64, u64> = HashMap::new();
        let mut pos = dyn_;
        let lim = (img.len() as i64 - 15).min(dyn_ + 4096 * 16);
        while pos < lim {
            let tag = le64(img, pos)? as i64;
            let value = le64(img, pos + 8)?;
            if tag == 0 {
                break;
            }
            tags.insert(tag, value);
            pos += 16;
        }
        let mut out = HashMap::new();
        if tags.get(&20) != Some(&7) {
            return Some(out);
        }
        let (rela, sz) = (*tags.get(&23)? as i64, *tags.get(&2)? as i64);
        let (symtab, strtab) = (*tags.get(&6)? as i64, *tags.get(&5)? as i64);
        let mut p = rela;
        while p < rela + sz {
            let slot = le64(img, p)? as i64;
            let info = le64(img, p + 8)?;
            if info & 0xffff_ffff == 1026 {
                let name = i64::from(le32(img, symtab + (info >> 32) as i64 * 24)?) + strtab;
                let tail = img.get(name as usize..)?;
                let z = tail.iter().position(|c| *c == 0)?;
                out.insert(slot, String::from_utf8(tail[..z].to_vec()).ok()?);
            }
            p += 24;
        }
        Some(out)
    }

    fn callee(&mut self, target: i64) -> Result<Callee, MErr> {
        let lim = self.text_end.min(self.img.len() as i64) - 16;
        if 0 <= target && target <= lim {
            let (a, b, c, d) = (
                le32(self.img, target).unwrap_or(0),
                le32(self.img, target + 4).unwrap_or(0),
                le32(self.img, target + 8).unwrap_or(0),
                le32(self.img, target + 12).unwrap_or(0),
            );
            if a & 0x9f00_001f == 0x9000_0010
                && b & 0xffc0_03ff == 0xf940_0211
                && c & 0xffc0_03ff == 0x9100_0210
                && d == 0xd61f_0220
            {
                let page = (target & !0xfff)
                    + (signed(
                        i64::from(((a >> 29) & 3) | (((a >> 5) & 0x7_ffff) << 2)),
                        21,
                    ) << 12);
                let lo = i64::from((b >> 10) & 0xfff) * 8;
                if i64::from((c >> 10) & 0xfff) == lo
                    && let Some(name) = self.import_slots().get(&(page + lo))
                    && !name.is_empty()
                {
                    return Ok(Callee::Import(name.clone()));
                }
            }
            if a & 0xffe0_001f == 0xd400_0001
                && b == 0xd65f_03c0
                && !self.functions.contains_key(&target)
            {
                return Ok(Callee::Svc((a >> 5) & 0xffff));
            }
        }
        let Some(end) = self.functions.get(&target).copied() else {
            return Err(format!(
                "callee has no verified boundary or import at {target:#x}"
            ));
        };
        Ok(Callee::Function(end - target, self.body(target)?.0))
    }

    pub(crate) fn fingerprint(&mut self, start: i64) -> Result<String, MErr> {
        if let Some(f) = self.fps.get(&start) {
            return f.clone();
        }
        let r = (|| {
            let (body, branches) = self.body(start)?;
            let mut s = String::new();
            let _ = write!(s, "[\"{body}\",[");
            for (k, (off, target)) in branches.iter().enumerate() {
                if k > 0 {
                    s.push(',');
                }
                let _ = write!(s, "[{off},");
                match self.callee(*target)? {
                    Callee::Import(n) => {
                        s.push_str("[\"import\",");
                        json_str(&n, &mut s);
                        s.push(']');
                    }
                    Callee::Svc(n) => {
                        let _ = write!(s, "[\"svc\",{n}]");
                    }
                    Callee::Function(sz, d) => {
                        let _ = write!(s, "[\"function\",{sz},\"{d}\"]");
                    }
                }
                s.push(']');
            }
            s.push_str("]]");
            Ok(hex(&sha256(s.as_bytes()).0))
        })();
        self.fps.insert(start, r.clone());
        r
    }

    fn match_fn(&mut self, f: &FnRule) -> Result<Vec<(i64, String, String, String)>, MErr> {
        let mut matches = Vec::new();
        for (start, end) in self.order.clone() {
            if (end - start) as usize != f.size {
                continue;
            }
            let Ok(d) = self.fingerprint(start) else {
                continue;
            };
            if f.fps.contains(&d) {
                matches.push(start);
            }
        }
        if matches.len() != 1 {
            return Err(format!(
                "{} matching functions (need exactly one)",
                matches.len()
            ));
        }
        let start = matches[0];
        let mut cuts = Vec::new();
        for (delta, old, new, reason) in &f.cuts {
            let off = start + *delta as i64;
            let ob = unhex(old);
            let got = self.img.get(off as usize..off as usize + ob.len());
            if got != Some(&ob[..]) {
                return Err(format!("original bytes differ at {off:#x}"));
            }
            cuts.push((
                off,
                old.clone(),
                new.clone(),
                format!("{reason} [function match]"),
            ));
        }
        Ok(cuts)
    }

    fn match_patch(&mut self, rule: &Rule) -> Result<Vec<(i64, String, String, String)>, MErr> {
        if !rule.multi {
            return self.match_fn(&rule.fns[0]);
        }
        let (mut cuts, mut starts, mut misses) = (Vec::new(), HashSet::new(), Vec::new());
        let n = rule.fns.len();
        for (i, f) in rule.fns.iter().enumerate() {
            match self.match_fn(f) {
                Err(e) => misses.push(format!("function {} of {n} ({} bytes): {e}", i + 1, f.size)),
                Ok(got) => {
                    let start = got[0].0 - f.cuts[0].0 as i64;
                    if !starts.insert(start) {
                        misses.push(format!(
                            "function {} of {n} matched a function another one did",
                            i + 1
                        ));
                    }
                    cuts.extend(got);
                }
            }
        }
        if !misses.is_empty() {
            let tail = if misses.len() < n {
                format!("; the other {} matched", n - misses.len())
            } else {
                String::new()
            };
            return Err(format!("{}{tail}", misses.join("; ")));
        }
        Ok(cuts)
    }
}

/// One resolved cut `(offset, old hex, new hex, reason)`.
pub(crate) type CutRow = (i64, String, String, String);

/// `(name, cut offsets or none, why)` of a patch.
pub(crate) type Outcome = (String, Option<Vec<i64>>, String);

/// Verified cuts of a rule list without changing the image.
pub(crate) fn resolve(
    img: &[u8],
    text_end: i64,
    rules: &[Rule],
    log: &mut Vec<String>,
    outcomes: &mut Vec<Outcome>,
    red: bool,
) -> Result<Vec<CutRow>, MErr> {
    if rules.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups: Vec<(String, Vec<&Rule>)> = Vec::new();
    for r in rules {
        match groups.iter_mut().find(|g| g.0 == r.name) {
            Some(g) => g.1.push(r),
            None => groups.push((r.name.clone(), vec![r])),
        }
    }
    let red_tag = if red { "[RED] " } else { "" };
    let mut module = match MatchImage::new(img, text_end) {
        Ok(m) => m,
        Err(e) => {
            for (name, _) in &groups {
                log.push(format!("{red_tag}function patch '{name}' skipped: {e}"));
                outcomes.push((name.clone(), None, e.clone()));
            }
            return Ok(Vec::new());
        }
    };
    for r in rules {
        crate::matchrule::validate(r)?;
    }
    let mut cuts: Vec<CutRow> = Vec::new();
    for (name, alts) in &groups {
        let (mut found, mut misses) = (Vec::new(), Vec::new());
        for (i, r) in alts.iter().enumerate() {
            match module.match_patch(r) {
                Ok(c) => found.push((i + 1, c)),
                Err(e) => misses.push(e),
            }
        }
        if found.len() != 1 {
            let why = if found.is_empty() {
                misses.join("; ")
            } else {
                format!("{} implementations match (need exactly one)", found.len())
            };
            log.push(format!("{red_tag}function patch '{name}' skipped: {why}"));
            outcomes.push((name.clone(), None, why));
            continue;
        }
        let (i, got) = found.remove(0);
        let offs: Vec<String> = got.iter().map(|c| format!("{:#x}", c.0)).collect();
        let suffix = if alts.len() > 1 {
            format!(" (implementation {i} of {})", alts.len())
        } else {
            String::new()
        };
        log.push(format!(
            "function patch '{name}': matched at {}{suffix}",
            offs.join(", ")
        ));
        outcomes.push((
            name.clone(),
            Some(got.iter().map(|c| c.0).collect()),
            format!("implementation {i} of {}", alts.len()),
        ));
        cuts.extend(got);
    }
    let mut occupied = HashSet::new();
    for (off, old, _, _) in &cuts {
        for b in *off..*off + (old.len() / 2) as i64 {
            if !occupied.insert(b) {
                return Err("function patches overlap".into());
            }
        }
    }
    Ok(cuts)
}
