//! Engine finder, part three: the port of `ue_region.py`. It finds the store of the size of Unreal's engine address
//! region right before `nninitStartup` reserves it. The gate and the `mov x1, xS` go in at that place
//! (see `Rw::symbol_gates`).

use std::collections::{BTreeMap, BTreeSet};

use nxb_a64::{Insn, Op, decode};

use crate::dynsym::defined;
use crate::pat::{memop, preg};

const ALLOCATE: &[u8] = b"_ZN2nn2os21AllocateAddressRegionEPmm";
const BR_X17: u32 = 0xD61F_0220;

/// What `ue_region.find` returns when its chain holds.
pub(crate) struct UeRegion {
    /// The store of the size: the gate goes here.
    pub(crate) off: usize,
    /// The register the store writes (`S`).
    pub(crate) reg: u8,
    /// The instruction that sets x1 to the same constant after the store, made `mov x1, xS`.
    pub(crate) x1: Option<usize>,
    pub(crate) why: String,
}

/// A value the size scan knows a register holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Val {
    Const(u64),
    Page,
    Lo(i64),
}

fn hx(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", v.unsigned_abs())
    } else {
        format!("{v:#x}")
    }
}

/// A word of the image where Python's `struct.unpack_from` would raise: an error, as the script stops there.
fn wd(img: &[u8], a: i64) -> Result<u32, String> {
    usize::try_from(a)
        .ok()
        .and_then(|a| img.get(a..a + 4))
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "struct.error".to_string())
}

fn target(w: u32, a: i64) -> i64 {
    a + ((i64::from(w & 0x3FF_FFFF) ^ 0x200_0000) - 0x200_0000) * 4
}

fn is_branch(i: &Insn) -> bool {
    matches!(
        i.mn,
        "b" | "bl" | "br" | "blr" | "ret" | "cbz" | "cbnz" | "tbz" | "tbnz"
    ) || i.mn.starts_with("b.")
}

fn dec(img: &[u8], a: i64) -> Result<Option<Insn>, String> {
    Ok(decode(wd(img, a)?, a as u64))
}

enum Store {
    Found(usize, u8, Option<usize>),
    No(String),
}

/// `size_store(img, call, start, md)`: the size store for the `bl` at `call`, or the reason it is not the reservation.
fn size_store(img: &[u8], call: i64, start: i64) -> Result<Store, String> {
    let mut a = call;
    while a - 4 >= start && a - call > -64 {
        let Some(i) = dec(img, a - 4)? else {
            return Err("StopIteration".into());
        };
        if is_branch(&i) {
            break;
        }
        a -= 4;
    }
    let mut vals: BTreeMap<String, Val> = BTreeMap::new();
    let mut last: BTreeMap<String, i64> = BTreeMap::new();
    // (address, S, displacement, the value S held)
    let mut stores: Vec<(i64, Option<u8>, i64, Option<Val>)> = Vec::new();
    let mut p = a;
    while p < call {
        let Some(i) = dec(img, p)? else { break };
        p += 4;
        let name0 = match i.ops.first() {
            Some(Op::Reg(r, _)) => r.name(),
            _ => String::new(),
        };
        if i.mn == "str"
            && name0.starts_with('x')
            && !i.wb
            && let Some(m) = memop(&i, 1).filter(|m| m.index.is_none())
        {
            let s = if name0 == "xzr" {
                None
            } else {
                name0[1..].parse().ok()
            };
            stores.push((i.addr as i64, s, m.disp, vals.get(&name0).copied()));
            continue;
        }
        let imm_op = matches!(i.ops.get(1), Some(Op::Imm(..) | Op::Hex(..) | Op::Post(..)));
        let v = if !name0.starts_with('x') {
            None
        } else if i.mn == "mov" && imm_op {
            i.imm(1).map(|v| Val::Const(v as u64))
        } else if i.mn == "mov" && preg(&i, 1).is_some() {
            preg(&i, 1).and_then(|r| vals.get(&r.name()).copied())
        } else if i.mn == "adrp" {
            Some(Val::Page)
        } else if i.mn == "add"
            && i.ops.len() == 3
            && matches!(i.ops.get(2), Some(Op::Imm(..) | Op::Hex(..) | Op::Post(..)))
            && preg(&i, 1).and_then(|r| vals.get(&r.name()).copied()) == Some(Val::Page)
        {
            i.imm(2).map(Val::Lo)
        } else {
            None
        };
        for w in &i.writes {
            let n = w.name().replacen('w', "x", 1);
            vals.remove(&n);
            last.insert(n, i.addr as i64);
        }
        if let Some(v) = v {
            vals.insert(name0, v);
        }
    }
    let Some(Val::Lo(lo)) = vals.get("x0").copied() else {
        return Ok(Store::No("x0 is no base variable".into()));
    };
    let Some(&(off, s, _, val)) = stores.iter().rfind(|s| s.2 == lo + 8) else {
        return Ok(Store::No(
            "no store of the size beside the base variable".into(),
        ));
    };
    let Some(s) = s else {
        return Ok(Store::No("the size stored is xzr".into()));
    };
    let is_const = matches!(val, Some(Val::Const(_)));
    if let Some(x1_at) = last.get("x1").copied().filter(|x| *x > off) {
        // the same constant set again after the store, with S still holding it
        if is_const
            && vals.get("x1").copied() == val
            && last.get(&format!("x{s}")).copied().unwrap_or(off) <= off
        {
            return Ok(Store::Found(off as usize, s, Some(x1_at as usize)));
        }
        return Ok(Store::No(format!(
            "x1 written after the size store at {}",
            hx(off)
        )));
    }
    if s != 1 && (!is_const || vals.get("x1").copied() != val) {
        return Ok(Store::No(format!(
            "x1 is not the x{s} stored at {}",
            hx(off)
        )));
    }
    Ok(Store::Found(off as usize, s, None))
}

/// `plt_stubs(img, text_end, name)`: the offsets of the PLT stubs of the import `name`, empty where the script gets an error it catches.
fn plt_stubs(img: &[u8], text_end: usize, name: &[u8]) -> BTreeSet<i64> {
    slots_of(img, name)
        .map(|slots| stubs_of(img, text_end, &slots))
        .unwrap_or_default()
}

fn u64at(img: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    let s = img.get(o..o.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

fn u32at(img: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    let s = img.get(o..o.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The GOT slots of the `R_AARCH64_JUMP_SLOT` relocations that name `name`.
fn slots_of(img: &[u8], name: &[u8]) -> Option<BTreeSet<i64>> {
    let m = u64::from(u32at(img, 4)?);
    if img.get(m as usize..m as usize + 4)? != b"MOD0" {
        return None;
    }
    let dyn_ = m.wrapping_add_signed(i64::from(u32at(img, m + 4)? as i32));
    let mut tags: BTreeMap<i64, u64> = BTreeMap::new();
    for k in 0..4096u64 {
        let t = u64at(img, dyn_ + 16 * k)? as i64;
        let v = u64at(img, dyn_ + 16 * k + 8)?;
        if t == 0 {
            break;
        }
        tags.entry(t).or_insert(v);
    }
    if tags.get(&20) != Some(&7) {
        return None;
    }
    let (rela, size) = (*tags.get(&23)?, *tags.get(&2)?);
    let (strtab, symtab) = (*tags.get(&5)?, *tags.get(&6)?);
    let mut slots = BTreeSet::new();
    let mut r = rela;
    while r < rela.checked_add(size)? {
        let (slot, info, _) = (u64at(img, r)?, u64at(img, r + 8)?, u64at(img, r + 16)?);
        r += 24;
        if info & 0xFFFF_FFFF != 1026 {
            continue;
        }
        let n = strtab + u64::from(u32at(img, symtab + (info >> 32) * 24)?);
        let tail = img.get(usize::try_from(n).ok()?..)?;
        let z = tail.iter().position(|c| *c == 0)?;
        if &tail[..z] == name {
            slots.insert(slot as i64);
        }
    }
    Some(slots)
}

/// The stubs `adrp x16`, `ldr x17, [x16, #lo]`, `add x16, x16, #lo`, `br x17` that go through one of the slots.
fn stubs_of(img: &[u8], text_end: usize, slots: &BTreeSet<i64>) -> BTreeSet<i64> {
    let mut out = BTreeSet::new();
    if slots.is_empty() {
        return out;
    }
    let pat = BR_X17.to_le_bytes();
    let end = text_end.min(img.len());
    let mut from = 0usize;
    while from + 4 <= end {
        let Some(at) = img[from..end].windows(4).position(|w| w == pat) else {
            break;
        };
        let a = from + at;
        from = a + 1;
        if !a.is_multiple_of(4) || a < 12 {
            continue;
        }
        let (p, l, d) = (
            crate::finders::word(img, a - 12),
            crate::finders::word(img, a - 8),
            crate::finders::word(img, a - 4),
        );
        if p & 0x9F00_001F != 0x9000_0010
            || l & 0xFFC0_03FF != 0xF940_0211
            || d & 0xFFC0_03FF != 0x9100_0210
        {
            continue;
        }
        let mut page = (i64::from((p >> 5) & 0x7FFFF) << 2 | i64::from((p >> 29) & 3)) << 12;
        if page & (1 << 32) != 0 {
            page -= 1 << 33;
        }
        let lo = i64::from((l >> 10) & 0xFFF) * 8;
        let base = (a as i64 - 12) & !0xFFF;
        if i64::from((d >> 10) & 0xFFF) == lo && slots.contains(&(base + page + lo)) {
            out.insert(a as i64 - 12);
        }
    }
    out
}

/// `ue_region.find(img, text_end, ro_off, data_off)`: `Ok(Ok(found))`, or `Ok(Err(why))` (`[RED] ` first when the
/// option is there and the reservation is not found).
pub(crate) fn ue_region_find(
    img: &[u8],
    text_end: usize,
    ro_off: usize,
    data_off: usize,
) -> Result<Result<UeRegion, String>, String> {
    if !crate::finders2::has_region_option(img, ro_off, data_off) {
        return Ok(Err("no engine region option".into()));
    }
    let syms = defined(img);
    let Some(&sym) = syms.get("nninitStartup") else {
        return Ok(Err(
            "[RED] Unreal's engine region option but no nninitStartup symbol".into(),
        ));
    };
    let stubs = plt_stubs(img, text_end, ALLOCATE);
    if stubs.is_empty() {
        return Ok(Err(
            "[RED] Unreal's engine region option but no nn::os::AllocateAddressRegion import"
                .into(),
        ));
    }
    let te = text_end as i64;
    // function starts: the targets of every bl and the defined symbols
    let mut starts: BTreeSet<i64> = syms
        .values()
        .filter(|(o, n)| *n != 0 && *o < te)
        .map(|(o, _)| *o)
        .collect();
    for a in (0..text_end.saturating_sub(3)).step_by(4) {
        if matches!(img[a + 3], 0x94..=0x97) {
            starts.insert(target(wd(img, a as i64)?, a as i64));
        }
    }
    let starts: Vec<i64> = starts.into_iter().collect();
    let end = |a: i64| {
        let k = starts.partition_point(|s| *s <= a);
        starts.get(k).copied().unwrap_or(te)
    };
    let entry = sym.0;
    let fns: Vec<i64> = if wd(img, entry)? >> 26 == 0x05 {
        vec![target(wd(img, entry)?, entry)]
    } else {
        let mut v = vec![entry];
        let mut a = entry;
        while a < entry + sym.1 {
            let w = wd(img, a)?;
            if w >> 26 == 0x25 {
                v.push(target(w, a));
            }
            a += 4;
        }
        v
    };
    let (mut found, mut why) = (Vec::new(), Vec::new());
    for &start in &fns {
        let mut p = start;
        while p < end(start) {
            let w = wd(img, p)?;
            if w >> 26 == 0x25 && stubs.contains(&target(w, p)) {
                match size_store(img, p, start)? {
                    Store::Found(off, s, x1) => found.push((p, start, off, s, x1)),
                    Store::No(no) => why.push(format!("{}: {no}", hx(p))),
                }
            }
            p += 4;
        }
    }
    if found.len() != 1 {
        let l: Vec<String> = fns.iter().map(|f| hx(*f)).collect();
        let tail: String = why.iter().map(|x| format!("; {x}")).collect();
        return Ok(Err(format!(
            "[RED] Unreal's engine region: {} reservations in nninitStartup ({}){tail}",
            found.len(),
            l.join(", ")
        )));
    }
    let (call, start, off, reg, x1) = found[0];
    let mut why = format!(
        "Unreal's engine region: reserved at {} in nninitStartup's {}, its size stored from x{reg} at {off:#x}",
        hx(call),
        hx(start)
    );
    if let Some(x) = x1 {
        why.push_str(&format!(", x1 set from it at {x:#x}"));
    }
    Ok(Ok(UeRegion { off, reg, x1, why }))
}
