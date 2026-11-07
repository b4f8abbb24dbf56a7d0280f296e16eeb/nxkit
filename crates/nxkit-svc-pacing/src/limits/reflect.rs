//! A frame limit found by the name of a reflected class in the main module.
//!
//! Some engines reflect their settings classes. A title module names the class, an int field and a float field, and gives
//! the layout of the records (`ReflectSpec`). The two fields cap the simulation's ticks a second. The host writes the
//! target into both.
//!
//! A static constructor registers the class. It stores the record's address into the class's type info (`adrp` and `add`
//! of the record, then `str` into the type info made by `adrp` and `add` just before). The code that reads the settings asks
//! the settings registry for the instance with that type info in `x1` and a cache word in `x2`. The cache word keeps the
//! address of the instance, and the instance holds its type info at a fixed offset. This is the shape of the registration
//! that the search expects. A module with another shape gives `Missing::TypeInfo` or `Missing::Lookup`.
//!
//! Nothing here is a fixed address, so the search holds across the updates of a game.
//!
//! Source: `fb_find`, `fb_pair` and `fb_tick` in `dm/host/hle_fpslock.c` of nxnative, read on 2026-11-07 at `88c7e03f`.

use nxkit_abi::guest::{self, Fault};

use super::{Module, ReflectSpec};

const MAX_CACHES: usize = 8;

/// Why the settings were not found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// The module has no class of that name.
    NotFound,
    /// The class is there but not its two fields.
    Fields,
    /// The registration that stores the record in the type info was not found.
    TypeInfo,
    /// The code that asks for the settings was not found.
    Lookup,
}

impl Missing {
    /// The text of the notice for the class of `spec`.
    pub fn text(self, spec: &ReflectSpec) -> String {
        let tail = match self {
            Self::NotFound => "not found".to_string(),
            Self::Fields => format!(
                "found, but not its {} and {} fields",
                spec.int_field, spec.float_field
            ),
            Self::TypeInfo => "its type info was not found".to_string(),
            Self::Lookup => "no settings lookup found".to_string(),
        };
        format!(
            "main: reflected class {}: {tail}, so the game's own frame limit stays",
            spec.class
        )
    }
}

/// The places that were found, as offsets into the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reflected {
    /// The class record.
    pub record: u64,
    /// The type info.
    pub type_info: u64,
    /// The cache words that hold the address of the instance.
    pub caches: Vec<u64>,
    /// The offset of the int field in the instance.
    pub off_int: u32,
    /// The offset of the float field in the instance.
    pub off_float: u32,
    /// The offset in an instance of the pointer to its type info.
    pub type_at: u64,
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

fn find_record(
    img: &[u8],
    m: &Module,
    limit: u64,
    spec: &ReflectSpec,
) -> Result<(u64, u32, u32), Missing> {
    let lay = &spec.layout;
    let from = usize::try_from(m.ro_off).map_err(|_| Missing::NotFound)?;
    let to = usize::try_from(limit).unwrap_or(usize::MAX).min(img.len());
    let hay = img.get(from..to).ok_or(Missing::NotFound)?;
    let mut needle = vec![0u8];
    needle.extend_from_slice(spec.class.as_bytes());
    needle.push(0);
    let idx = hay
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
        .ok_or(Missing::NotFound)?;
    let name = m.base + m.ro_off + idx as u64 + 1;
    let (mut off_int, mut off_float) = (0u32, 0u32);
    let mut o = (m.ro_off + 7) & !7;
    while o + lay.record_size <= limit {
        let here = o;
        o += 8;
        if u64_at(img, here) != Some(name) {
            continue;
        }
        let Some(fl) = u64_at(img, here + lay.fields_at) else {
            continue;
        };
        if fl < m.base || fl - m.base + lay.field_stride > limit {
            continue;
        }
        let mut f = fl - m.base;
        while f + lay.field_stride <= limit {
            let Some(field) = u64_at(img, f) else { break };
            if field < m.base || field - m.base >= limit {
                break;
            }
            let off = u32_at(img, f + lay.field_offset_at).unwrap_or(0);
            if name_at(img, m.base, limit, field, spec.int_field) {
                off_int = off;
            } else if name_at(img, m.base, limit, field, spec.float_field) {
                off_float = off;
            }
            f += lay.field_stride;
        }
        if off_int != 0 && off_float != 0 {
            return Ok((here, off_int, off_float));
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
pub fn find(img: &[u8], m: &Module, spec: &ReflectSpec) -> Result<Reflected, Missing> {
    if m.text_end == 0 || m.ro_off == 0 || m.ro_off >= m.size {
        return Err(Missing::NotFound);
    }
    let limit = if m.back_off != 0 { m.back_off } else { m.size };
    let (record, off_int, off_float) = find_record(img, m, limit, spec)?;
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
    Ok(Reflected {
        record,
        type_info: ti,
        caches,
        off_int,
        off_float,
        type_at: spec.layout.instance_type_at,
    })
}

/// The values that a tick replaced.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The int field before.
    pub int: i32,
    /// The float field before.
    pub float: f32,
}

impl Reflected {
    /// Writes the target into the first instance that exists. A game may load its settings again, so it runs every frame.
    /// `None` while no cache holds an instance, or when both fields already have the target. Nothing is allocated.
    pub fn tick(&self, base: u64, target: i32) -> Result<Option<Written>, Fault> {
        let want = target as f32;
        for cache in &self.caches {
            let Ok(inst) = guest::read_u64(base + cache) else {
                continue;
            };
            if inst == 0 || guest::read_u64(inst + self.type_at) != Ok(base + self.type_info) {
                continue;
            }
            let (Ok(int), Ok(float)) = (
                guest::read_i32(inst + u64::from(self.off_int)),
                guest::read_f32(inst + u64::from(self.off_float)),
            ) else {
                continue;
            };
            if int != target {
                guest::write_i32(inst + u64::from(self.off_int), target)?;
            }
            if float != want {
                guest::write_f32(inst + u64::from(self.off_float), want)?;
            }
            return Ok((int != target || float != want).then_some(Written { int, float }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Layout;

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

    fn spec() -> ReflectSpec {
        ReflectSpec {
            class: "TestSettings",
            int_field: "IntField",
            float_field: "FloatField",
            layout: Layout {
                record_size: 0x48,
                fields_at: 0x40,
                field_stride: 0x20,
                field_offset_at: 0x10,
                instance_type_at: 8,
            },
        }
    }

    #[test]
    fn a_name_must_end_in_a_nul_inside_the_module() {
        let img = b"xxIntField\0yy".to_vec();
        assert!(name_at(&img, 0x1000, 13, 0x1002, "IntField"));
        assert!(!name_at(&img, 0x1000, 13, 0x1002, "Int"));
        assert!(
            !name_at(&img, 0x1000, 10, 0x1002, "IntField"),
            "ends past the limit"
        );
        assert!(
            !name_at(&img, 0x1000, 13, 0xfff, "IntField"),
            "before the module"
        );
    }

    #[test]
    fn a_module_without_the_class_is_told_not_found() {
        let img = vec![0u8; 0x1000];
        assert_eq!(find(&img, &module(), &spec()), Err(Missing::NotFound));
        assert!(Missing::NotFound.text(&spec()).contains("TestSettings"));
    }

    #[test]
    fn a_class_without_its_fields_is_reported() {
        let mut img = vec![0u8; 0x1000];
        img[0x500..0x500 + 14].copy_from_slice(b"\0TestSettings\0");
        assert_eq!(find(&img, &module(), &spec()), Err(Missing::Fields));
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
        img[0x500..0x500 + 14].copy_from_slice(b"\0TestSettings\0");
        img[0x600..0x600 + 9].copy_from_slice(b"IntField\0");
        img[0x620..0x620 + 11].copy_from_slice(b"FloatField\0");
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
        let found = find(&img, &m, &spec());
        assert_eq!(
            found,
            Ok(Reflected {
                record: 0x800,
                type_info: 0x1100,
                caches: vec![0x1200],
                off_int: 0x30,
                off_float: 0x34,
                type_at: 8,
            })
        );
    }
}
