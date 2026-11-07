//! Frostbite's simulation rate, found by name in the main module.
//!
//! Frostbite reflects its settings classes. A class record holds its name, and at +0x40 its field list. A field record is
//! 0x20 bytes: the name, a hash, the offset at +0x10 and the type. The fields `ForceSimRate` (int32) and `MaxSimFps` (float)
//! of `SimulationTimeSettings` cap the simulation's ticks a second. EA SPORTS FC 27 has 30 there, so its frames cannot go
//! faster than 30 whatever paces them.
//!
//! A static constructor registers the class. It stores the record's address into the class's type info (`adrp` and `add`
//! of the record, then `str` into the type info made by `adrp` and `add` just before). The code that reads the settings asks
//! the settings registry for the instance with that type info in `x1` and a cache word in `x2`. The cache word keeps the
//! address of the instance, and the instance holds its type info at +8.
//!
//! Nothing here is a fixed address, so the search holds across the updates of a game.
//!
//! Source: `fb_find`, `fb_pair` and `fb_tick` in `dm/host/hle_fpslock.c` of nxnative, read on 2026-11-07 at `88c7e03f`.

use nxkit_abi::guest::{self, Fault};

use super::Module;

const CLASS: &[u8] = b"SimulationTimeSettings";
const MAX_CACHES: usize = 8;

/// Why the settings were not found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The module has no `SimulationTimeSettings`. It is not a Frostbite game.
    NotFrostbite,
    /// The class is there but not its two fields.
    Fields,
    /// The registration that stores the record in the type info was not found.
    TypeInfo,
    /// The code that asks for the settings was not found.
    Lookup,
}

impl Missing {
    /// The text of the notice. `None` for a module that is not Frostbite.
    pub fn text(self) -> Option<String> {
        let tail = match self {
            Self::NotFrostbite => return None,
            Self::Fields => "found, but not its ForceSimRate and MaxSimFps fields",
            Self::TypeInfo => "its type info was not found",
            Self::Lookup => "no settings lookup found",
        };
        Some(format!(
            "main: Frostbite's SimulationTimeSettings: {tail}, so the simulation rate stays"
        ))
    }
}

/// The places that were found, as offsets into the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frostbite {
    /// The class record.
    pub record: u64,
    /// The type info.
    pub type_info: u64,
    /// The cache words that hold the address of the instance.
    pub caches: Vec<u64>,
    /// The offset of `ForceSimRate` in the instance.
    pub off_rate: u32,
    /// The offset of `MaxSimFps` in the instance.
    pub off_max: u32,
}

fn u64_at(img: &[u8], o: u64) -> Option<u64> {
    let o = usize::try_from(o).ok()?;
    Some(u64::from_le_bytes(
        img.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn u32_at(img: &[u8], o: u64) -> Option<u32> {
    let o = usize::try_from(o).ok()?;
    Some(u32::from_le_bytes(
        img.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Whether the relocated pointer `p` points into the module at the string `want`.
fn name_at(img: &[u8], base: u64, limit: u64, p: u64, want: &str) -> bool {
    let n = want.len() as u64;
    if p < base || p - base + n + 1 > limit {
        return false;
    }
    let o = (p - base) as usize;
    img.get(o..o + want.len() + 1)
        .is_some_and(|b| &b[..want.len()] == want.as_bytes() && b[want.len()] == 0)
}

/// The target of `adrp` and `add` into `reg` at the words `i` and `i + 1` of the text, as a module offset. The text sits at
/// offset 0.
fn pair(t: &[u8], i: u64, reg: u32) -> Option<i64> {
    let w = u32_at(t, i * 4)?;
    let x = u32_at(t, i * 4 + 4)?;
    if w & 0x9F00_0000 != 0x9000_0000
        || w & 31 != reg
        || x & 0xFFC0_0000 != 0x9100_0000
        || x & 31 != reg
        || (x >> 5) & 31 != reg
    {
        return None;
    }
    let mut imm = i64::from(((w >> 29) & 3) | (((w >> 5) & 0x7FFFF) << 2));
    if imm & (1 << 20) != 0 {
        imm -= 1 << 21;
    }
    Some((((i * 4) & !0xfff) as i64) + imm * 4096 + i64::from((x >> 10) & 0xfff))
}

fn find_record(img: &[u8], m: &Module, limit: u64) -> Result<(u64, u32, u32), Missing> {
    let from = usize::try_from(m.ro_off).map_err(|_| Missing::NotFrostbite)?;
    let to = usize::try_from(limit).unwrap_or(usize::MAX).min(img.len());
    let hay = img.get(from..to).ok_or(Missing::NotFrostbite)?;
    let mut needle = vec![0u8];
    needle.extend_from_slice(CLASS);
    needle.push(0);
    let idx = hay
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
        .ok_or(Missing::NotFrostbite)?;
    let name = m.base + m.ro_off + idx as u64 + 1;
    let (mut off_rate, mut off_max) = (0u32, 0u32);
    let mut o = (m.ro_off + 7) & !7;
    while o + 0x48 <= limit {
        let here = o;
        o += 8;
        if u64_at(img, here) != Some(name) {
            continue;
        }
        let Some(fl) = u64_at(img, here + 0x40) else {
            continue;
        };
        if fl < m.base || fl - m.base + 0x20 > limit {
            continue;
        }
        let mut f = fl - m.base;
        while f + 0x20 <= limit {
            let Some(field) = u64_at(img, f) else { break };
            if field < m.base || field - m.base >= limit {
                break;
            }
            let off = u32_at(img, f + 0x10).unwrap_or(0);
            if name_at(img, m.base, limit, field, "ForceSimRate") {
                off_rate = off;
            } else if name_at(img, m.base, limit, field, "MaxSimFps") {
                off_max = off;
            }
            f += 0x20;
        }
        if off_rate != 0 && off_max != 0 {
            return Ok((here, off_rate, off_max));
        }
    }
    Err(Missing::Fields)
}

fn find_type_info(img: &[u8], nw: u64, record: u64) -> Option<u64> {
    let word = |i: u64| u32_at(img, i * 4).unwrap_or(0);
    let want = i64::try_from(record).ok();
    let mut at = 0;
    while at + 3 < nw {
        let r = word(at) & 31;
        let this = at;
        at += 1;
        if pair(img, this, r) != want {
            continue;
        }
        let st = word(this + 2);
        if st & 0xFFFF_FC00 != 0xF900_0000 || st & 31 != r {
            continue;
        }
        let xt = (st >> 5) & 31;
        let mut ti = None;
        for k in this.saturating_sub(6)..this {
            if let Some(t) = pair(img, k, xt).filter(|t| *t > 0) {
                ti = Some(t as u64);
            }
        }
        if ti.is_some() {
            return ti;
        }
    }
    None
}

/// Finds the settings in the image of the main module. `img` holds the module's bytes from its base.
pub fn find(img: &[u8], m: &Module) -> Result<Frostbite, Missing> {
    if m.text_end == 0 || m.ro_off == 0 || m.ro_off >= m.size {
        return Err(Missing::NotFrostbite);
    }
    let limit = if m.back_off != 0 { m.back_off } else { m.size };
    let (record, off_rate, off_max) = find_record(img, m, limit)?;
    let nw = m.text_end / 4;
    let ti = find_type_info(img, nw, record).ok_or(Missing::TypeInfo)?;
    let want = i64::try_from(ti).ok();
    let mut caches: Vec<u64> = Vec::new();
    let mut i = 8;
    while i + 12 < nw && caches.len() < MAX_CACHES {
        let at = i;
        i += 1;
        if pair(img, at, 1) != want {
            continue;
        }
        for k in at - 8..at + 10 {
            let Some(c) = pair(img, k, 2)
                .and_then(|c| u64::try_from(c).ok())
                .filter(|c| *c > 0 && *c + 8 <= m.size)
            else {
                continue;
            };
            if !caches.contains(&c) {
                caches.push(c);
            }
            break;
        }
    }
    if caches.is_empty() {
        return Err(Missing::Lookup);
    }
    Ok(Frostbite {
        record,
        type_info: ti,
        caches,
        off_rate,
        off_max,
    })
}

/// The values that a tick replaced.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// `ForceSimRate` before.
    pub rate: i32,
    /// `MaxSimFps` before.
    pub max: f32,
}

impl Frostbite {
    /// Writes the target into the first instance that exists. A game may load its settings again, so it runs every frame.
    /// `None` while no cache holds an instance, or when both fields already have the target.
    pub fn tick(&self, base: u64, target: i32) -> Result<Option<Written>, Fault> {
        let want = target as f32;
        for cache in &self.caches {
            let Ok(inst) = guest::read_u64(base + cache) else {
                continue;
            };
            if inst == 0 || guest::read_u64(inst + 8) != Ok(base + self.type_info) {
                continue;
            }
            let (Ok(rate), Ok(max)) = (
                guest::read_i32(inst + u64::from(self.off_rate)),
                guest::read_f32(inst + u64::from(self.off_max)),
            ) else {
                continue;
            };
            if rate != target {
                guest::write_i32(inst + u64::from(self.off_rate), target)?;
            }
            if max != want {
                guest::write_f32(inst + u64::from(self.off_max), want)?;
            }
            return Ok((rate != target || max != want).then_some(Written { rate, max }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adrp(reg: u32, page: i64) -> u32 {
        let imm = (page & 0x1F_FFFF) as u32;
        0x9000_0000 | ((imm & 3) << 29) | ((imm >> 2) << 5) | reg
    }

    fn add(reg: u32, lo: u32) -> u32 {
        0x9100_0000 | (lo << 10) | (reg << 5) | reg
    }

    fn put(img: &mut [u8], word: u64, v: u32) {
        let o = (word * 4) as usize;
        img[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn module() -> Module {
        Module {
            base: 0x10_0000,
            size: 0x1000,
            text_end: 0x400,
            ro_off: 0x400,
            ..Module::default()
        }
    }

    #[test]
    fn a_pair_gives_the_page_and_the_low_bits() {
        let mut t = vec![0u8; 64];
        put(&mut t, 0, adrp(3, 0x12));
        put(&mut t, 1, add(3, 0x345));
        assert_eq!(pair(&t, 0, 3), Some(0x12 * 4096 + 0x345));
        assert_eq!(pair(&t, 0, 4), None, "another register");
        put(&mut t, 4, adrp(3, 1));
        put(&mut t, 5, add(3, 2));
        assert_eq!(pair(&t, 4, 3), Some(4096 + 2));
    }

    #[test]
    fn a_negative_page_counts_down() {
        let mut t = vec![0u8; 0x4000];
        let i = 0x2000 / 4;
        put(&mut t, i, adrp(1, -1));
        put(&mut t, i + 1, add(1, 0x10));
        assert_eq!(pair(&t, i, 1), Some(0x2000 - 4096 + 0x10));
    }

    #[test]
    fn a_name_must_end_in_a_nul_inside_the_module() {
        let img = b"xxForceSimRate\0yy".to_vec();
        assert!(name_at(&img, 0x1000, 17, 0x1002, "ForceSimRate"));
        assert!(!name_at(&img, 0x1000, 17, 0x1002, "ForceSim"));
        assert!(
            !name_at(&img, 0x1000, 15, 0x1002, "ForceSimRate"),
            "ends past the limit"
        );
        assert!(
            !name_at(&img, 0x1000, 17, 0xfff, "ForceSimRate"),
            "before the module"
        );
    }

    #[test]
    fn a_module_without_the_class_is_not_frostbite() {
        let img = vec![0u8; 0x1000];
        assert_eq!(find(&img, &module()), Err(Missing::NotFrostbite));
        assert!(Missing::NotFrostbite.text().is_none());
        assert!(Missing::Lookup.text().is_some());
    }

    #[test]
    fn a_class_without_its_fields_is_reported() {
        let mut img = vec![0u8; 0x1000];
        img[0x500..0x500 + 24].copy_from_slice(b"\0SimulationTimeSettings\0");
        assert_eq!(find(&img, &module()), Err(Missing::Fields));
    }

    #[test]
    fn a_full_registration_is_found() {
        let mut img = vec![0u8; 0x4000];
        let m = Module {
            base: 0x10_0000,
            size: 0x4000,
            text_end: 0x400,
            ro_off: 0x400,
            ..Module::default()
        };
        img[0x500..0x500 + 24].copy_from_slice(b"\0SimulationTimeSettings\0");
        img[0x600..0x600 + 13].copy_from_slice(b"ForceSimRate\0");
        img[0x620..0x620 + 10].copy_from_slice(b"MaxSimFps\0");
        let put64 =
            |img: &mut Vec<u8>, o: usize, v: u64| img[o..o + 8].copy_from_slice(&v.to_le_bytes());
        // The class record at 0x800 names the class and points at the field list at 0x900.
        put64(&mut img, 0x800, m.base + 0x501);
        put64(&mut img, 0x840, m.base + 0x900);
        put64(&mut img, 0x900, m.base + 0x600);
        img[0x910..0x914].copy_from_slice(&0x30u32.to_le_bytes());
        put64(&mut img, 0x920, m.base + 0x620);
        img[0x930..0x934].copy_from_slice(&0x34u32.to_le_bytes());
        // The registration: adrp/add x5 = record, adrp/add x4 = type info 0x1100 before it, str x5, [x4].
        put(&mut img, 10, adrp(4, 1));
        put(&mut img, 11, add(4, 0x100));
        put(&mut img, 12, adrp(5, 0));
        put(&mut img, 13, add(5, 0x800));
        put(&mut img, 14, 0xF900_0000 | (4 << 5) | 5);
        // The reader: adrp/add x1 = type info, adrp/add x2 = cache word 0x1200.
        put(&mut img, 30, adrp(1, 1));
        put(&mut img, 31, add(1, 0x100));
        put(&mut img, 32, adrp(2, 1));
        put(&mut img, 33, add(2, 0x200));
        let found = find(&img, &m);
        assert_eq!(
            found,
            Ok(Frostbite {
                record: 0x800,
                type_info: 0x1100,
                caches: vec![0x1200],
                off_rate: 0x30,
                off_max: 0x34,
            })
        );
    }
}
