//! The relocations of an NSO image (R47a).
//!
//! The images of the build still hold module offsets in their pointers, and `base` is only a preferred address,
//! because dyld slides the library. So the host adds the real base itself, the same as `relocate()` of nxnative does.
//! The plan is computed from the bytes of the image file and from the base, never from the memory of the module. So
//! applying it twice gives the same result.
//!
//! Handled are `R_AARCH64_RELATIVE`, `DT_RELR` and the symbol relocations `ABS64`, `GLOB_DAT` and `JUMP_SLOT` of
//! symbols that the module defines. Undefined symbols are filled from the import list of the module. Any other
//! relocation type stops the load with its number (R15).

use crate::error::{HostError, Result};
use crate::module::Module;

const DT_NULL: u64 = 0;
const DT_PLTRELSZ: u64 = 2;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_JMPREL: u64 = 23;
const DT_RELRSZ: u64 = 35;
const DT_RELR: u64 = 36;

const R_NONE: u32 = 0;
const R_ABS64: u32 = 257;
const R_GLOB_DAT: u32 = 1025;
const R_JUMP_SLOT: u32 = 1026;
const R_RELATIVE: u32 = 1027;

const RELA_SIZE: u64 = 24;
const SYM_SIZE: u64 = 24;
const MAX_DYNAMIC: u64 = 4096;
const MAX_NAME: u64 = 4096;

/// One 64-bit store into the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Write {
    /// The offset from the module base.
    pub offset: u64,
    /// The value.
    pub value: u64,
}

/// What the plan holds, for the log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// `R_AARCH64_RELATIVE` entries.
    pub relative: usize,
    /// Slots of `DT_RELR`.
    pub relr: usize,
    /// Symbol relocations of defined symbols.
    pub symbols: usize,
    /// Relocations of undefined symbols. The import list of the module fills these.
    pub undefined: usize,
}

/// The stores that relocate one module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The stores, in the order of the tables.
    pub writes: Vec<Write>,
    /// How many of each kind.
    pub counts: Counts,
}

fn err(m: &Module, what: impl std::fmt::Display) -> HostError {
    HostError::Load(format!("module {}: relocation: {what}", m.name))
}

/// The bytes of an image file seen through the segments of its module.
struct Image<'a> {
    m: &'a Module,
    bytes: &'a [u8],
}

impl Image<'_> {
    /// `len` bytes at a module offset. The bytes of a segment after `file_size` are zero.
    fn read(&self, off: u64, len: u64) -> Option<Vec<u8>> {
        let s = self.m.segment_at(off)?;
        let rel = u64::try_from(i64::try_from(off).ok()?.checked_sub(s.offset)?).ok()?;
        let end = rel.checked_add(len)?;
        if end > s.size {
            return None;
        }
        let mut out = vec![0u8; usize::try_from(len).ok()?];
        if rel < s.file_size {
            let have = (s.file_size.min(end) - rel) as usize;
            let at = usize::try_from(s.file_offset.checked_add(rel)?).ok()?;
            out[..have].copy_from_slice(self.bytes.get(at..at.checked_add(have)?)?);
        }
        Some(out)
    }

    fn u64_at(&self, off: u64) -> Result<u64> {
        let b = self.read(off, 8).ok_or_else(|| err(self.m, format!("offset {off:#x} is outside the image")))?;
        Ok(u64::from_le_bytes(b.try_into().unwrap_or_default()))
    }

    fn u32_at(&self, off: u64) -> Result<u32> {
        let b = self.read(off, 4).ok_or_else(|| err(self.m, format!("offset {off:#x} is outside the image")))?;
        Ok(u32::from_le_bytes(b.try_into().unwrap_or_default()))
    }

    fn name_at(&self, off: u64) -> Result<String> {
        let mut out = Vec::new();
        for i in 0..MAX_NAME {
            let b = self.read(off + i, 1).ok_or_else(|| err(self.m, "a symbol name leaves the image"))?;
            let c = b.first().copied().unwrap_or(0);
            if c == 0 {
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            out.push(c);
        }
        Err(err(self.m, "a symbol name has no end"))
    }
}

#[derive(Default)]
struct Dynamic {
    rela: u64,
    relasz: u64,
    jmprel: u64,
    pltrelsz: u64,
    relr: u64,
    relrsz: u64,
    symtab: u64,
    strtab: u64,
}

/// Finds the dynamic section through the `MOD0` header. A module without one has nothing to relocate.
fn dynamic(img: &Image) -> Result<Option<Dynamic>> {
    let Some(head) = img.read(4, 4) else {
        return Ok(None);
    };
    let mod0 = u64::from(u32::from_le_bytes(head.try_into().unwrap_or_default()));
    match img.read(mod0, 4) {
        Some(magic) if magic == b"MOD0" => {}
        _ => return Ok(None),
    }
    let rel = i64::from(img.u32_at(mod0 + 4)? as i32);
    let start = u64::try_from(i64::try_from(mod0).map_err(|_| err(img.m, "bad MOD0"))? + rel)
        .map_err(|_| err(img.m, "the dynamic section is before the module"))?;
    let mut d = Dynamic::default();
    for i in 0..MAX_DYNAMIC {
        let tag = img.u64_at(start + 16 * i)?;
        let val = img.u64_at(start + 16 * i + 8)?;
        match tag {
            DT_NULL => return Ok(Some(d)),
            DT_RELA => d.rela = val,
            DT_RELASZ => d.relasz = val,
            DT_JMPREL => d.jmprel = val,
            DT_PLTRELSZ => d.pltrelsz = val,
            DT_RELR => d.relr = val,
            DT_RELRSZ => d.relrsz = val,
            DT_SYMTAB => d.symtab = val,
            DT_STRTAB => d.strtab = val,
            _ => {}
        }
    }
    Err(err(img.m, "the dynamic section has no end"))
}

fn relr(img: &Image, d: &Dynamic, base: u64, plan: &mut Plan) -> Result<()> {
    let mut next: Option<u64> = None;
    for i in 0..d.relrsz / 8 {
        let e = img.u64_at(d.relr + 8 * i)?;
        if e & 1 == 0 {
            plan.writes.push(Write { offset: e, value: img.u64_at(e)?.wrapping_add(base) });
            plan.counts.relr += 1;
            next = Some(e + 8);
            continue;
        }
        let Some(at) = next else {
            return Err(err(img.m, "a DT_RELR bitmap comes before any address"));
        };
        for bit in 0..63u64 {
            if (e >> (bit + 1)) & 1 == 1 {
                let off = at + 8 * bit;
                plan.writes.push(Write { offset: off, value: img.u64_at(off)?.wrapping_add(base) });
                plan.counts.relr += 1;
            }
        }
        next = Some(at + 63 * 8);
    }
    Ok(())
}

fn symbols(
    img: &Image,
    d: &Dynamic,
    table: (u64, u64),
    base: u64,
    lookup: &mut dyn FnMut(&str) -> Result<Option<u64>>,
    plan: &mut Plan,
) -> Result<()> {
    for i in 0..table.1 / RELA_SIZE {
        let at = table.0 + RELA_SIZE * i;
        let offset = img.u64_at(at)?;
        let info = img.u64_at(at + 8)?;
        let addend = img.u64_at(at + 16)?;
        let (sym, kind) = (info >> 32, (info & 0xffff_ffff) as u32);
        let value = match kind {
            R_NONE => continue,
            R_RELATIVE => {
                plan.counts.relative += 1;
                base.wrapping_add(addend)
            }
            R_ABS64 | R_GLOB_DAT | R_JUMP_SLOT => {
                let s = d.symtab + SYM_SIZE * sym;
                let (name_at, info_byte, other, shndx) = (
                    img.u32_at(s)?,
                    img.read(s + 4, 1).and_then(|b| b.first().copied()).unwrap_or(0),
                    img.read(s + 5, 1).and_then(|b| b.first().copied()).unwrap_or(0),
                    img.read(s + 6, 2).map_or(0, |b| u16::from_le_bytes(b.try_into().unwrap_or_default())),
                );
                if sym != 0 && shndx == 0 {
                    plan.counts.undefined += 1;
                    continue;
                }
                let own = base.wrapping_add(img.u64_at(s + 8)?);
                let v = if sym == 0 {
                    0
                } else if info_byte >> 4 == 0 || other & 3 != 0 {
                    own
                } else {
                    lookup(&img.name_at(d.strtab + u64::from(name_at))?)?.unwrap_or(own)
                };
                plan.counts.symbols += 1;
                if kind == R_JUMP_SLOT { v } else { v.wrapping_add(addend) }
            }
            other => return Err(err(img.m, format!("type {other} at offset {offset:#x} is not supported"))),
        };
        plan.writes.push(Write { offset, value });
    }
    Ok(())
}

/// Plans the relocations of a module whose base is `m.base`.
///
/// `image` is the file of the module. `lookup` gives the address that a symbol that the module defines binds to, or
/// `None` when the module's own definition applies. Every target must lie in a writable segment.
pub fn plan(m: &Module, image: &[u8], lookup: &mut dyn FnMut(&str) -> Result<Option<u64>>) -> Result<Plan> {
    let img = Image { m, bytes: image };
    let mut p = Plan::default();
    let Some(d) = dynamic(&img)? else {
        return Ok(p);
    };
    if d.relr != 0 {
        relr(&img, &d, m.base, &mut p)?;
    }
    if d.rela != 0 {
        symbols(&img, &d, (d.rela, d.relasz), m.base, lookup, &mut p)?;
    }
    if d.jmprel != 0 {
        symbols(&img, &d, (d.jmprel, d.pltrelsz), m.base, lookup, &mut p)?;
    }
    for w in &p.writes {
        if w.offset % 8 != 0 || !m.writable(w.offset, 8) {
            return Err(err(m, format!("the target {:#x} is not an aligned slot in a writable segment", w.offset)));
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::module::{parse, samples::NEAR};

    fn put(b: &mut [u8], at: usize, v: u64) {
        b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }

    fn put32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// An image for `NEAR`: text 0 to 0x8000 (MOD0 at 0x100, dynamic section at 0x200, tables at 0x400),
    /// data from 0x8000 with the slots.
    fn image() -> Vec<u8> {
        let mut b = vec![0u8; 0xc000];
        put32(&mut b, 4, 0x100);
        b[0x100..0x104].copy_from_slice(b"MOD0");
        put32(&mut b, 0x104, 0x100);
        let dynamic = [
            (DT_RELA, 0x400u64),
            (DT_RELASZ, 3 * 24),
            (DT_JMPREL, 0x500),
            (DT_PLTRELSZ, 24),
            (DT_RELR, 0x600),
            (DT_RELRSZ, 16),
            (DT_SYMTAB, 0x700),
            (DT_STRTAB, 0x800),
            (DT_NULL, 0),
        ];
        for (i, (t, v)) in dynamic.iter().enumerate() {
            put(&mut b, 0x200 + 16 * i, *t);
            put(&mut b, 0x208 + 16 * i, *v);
        }
        // RELA: RELATIVE with addend 0x9000, GLOB_DAT of the defined symbol 1, ABS64 of the undefined symbol 2.
        let rela = [(0x8000u64, u64::from(R_RELATIVE), 0x9000u64), (0x8008, (1 << 32) | u64::from(R_GLOB_DAT), 0x10), (0x8010, (2 << 32) | u64::from(R_ABS64), 0)];
        for (i, (o, info, a)) in rela.iter().enumerate() {
            put(&mut b, 0x400 + 24 * i, *o);
            put(&mut b, 0x408 + 24 * i, *info);
            put(&mut b, 0x410 + 24 * i, *a);
        }
        // JMPREL: JUMP_SLOT of symbol 1.
        put(&mut b, 0x500, 0x8018);
        put(&mut b, 0x508, (1 << 32) | u64::from(R_JUMP_SLOT));
        // RELR: the slot 0x8020 and a bitmap that adds the slots 0x8028 and 0x8038.
        put(&mut b, 0x600, 0x8020);
        put(&mut b, 0x608, (1 << 1) | (1 << 3) | 1);
        put(&mut b, 0x8020, 0x1234);
        put(&mut b, 0x8028, 0x20);
        put(&mut b, 0x8038, 0x30);
        // Symbols: 1 is defined and global (name "f", value 0x40), 2 is undefined.
        put32(&mut b, 0x700 + 24, 1);
        b[0x700 + 24 + 4] = 0x12;
        b[0x700 + 24 + 6] = 1;
        put(&mut b, 0x700 + 24 + 8, 0x40);
        put32(&mut b, 0x700 + 48, 3);
        b[0x700 + 48 + 4] = 0x12;
        b[0x801] = b'f';
        b
    }

    fn module(base: u64) -> Option<Module> {
        let m = json::parse(NEAR).ok().and_then(|v| parse(&v, 0).ok())?;
        m.moved(base as i64 - m.base as i64)
    }

    #[test]
    fn plans_every_kind() {
        let m = module(0x1_0000_0000);
        assert!(m.is_some());
        let Some(m) = m else { return };
        let p = plan(&m, &image(), &mut |n| Ok((n == "f").then_some(0x5000)));
        assert!(p.is_ok(), "{:?}", p.as_ref().err());
        let Ok(p) = p else { return };
        let at = |o: u64| p.writes.iter().find(|w| w.offset == o).map(|w| w.value);
        assert_eq!(at(0x8000), Some(0x1_0000_9000));
        assert_eq!(at(0x8008), Some(0x5010));
        assert_eq!(at(0x8010), None);
        assert_eq!(at(0x8018), Some(0x5000));
        assert_eq!(at(0x8020), Some(0x1234 + 0x1_0000_0000));
        assert_eq!(at(0x8028), Some(0x20 + 0x1_0000_0000));
        assert_eq!(at(0x8030), None);
        assert_eq!(at(0x8038), Some(0x30 + 0x1_0000_0000));
        assert_eq!(p.counts, Counts { relative: 1, relr: 3, symbols: 2, undefined: 1 });
    }

    #[test]
    fn a_slide_moves_every_value_and_nothing_else() {
        let (a, b) = (module(0x1_0000_0000), module(0x1_0000_8000));
        let (Some(a), Some(b)) = (a, b) else { return };
        let img = image();
        let (pa, pb) = (plan(&a, &img, &mut |_| Ok(None)), plan(&b, &img, &mut |_| Ok(None)));
        let (Ok(pa), Ok(pb)) = (pa, pb) else { return };
        assert_eq!(pa.writes.len(), pb.writes.len());
        for (x, y) in pa.writes.iter().zip(&pb.writes) {
            assert_eq!(x.offset, y.offset);
            assert_eq!(y.value - x.value, 0x8000);
        }
    }

    #[test]
    fn an_unknown_type_stops_with_its_number() {
        let m = module(0x1_0000_0000);
        let Some(m) = m else { return };
        let mut img = image();
        put(&mut img, 0x408, 1030);
        let e = plan(&m, &img, &mut |_| Ok(None));
        assert!(e.is_err_and(|e| e.to_string().contains("1030")));
    }

    #[test]
    fn a_target_in_the_code_is_refused() {
        let m = module(0x1_0000_0000);
        let Some(m) = m else { return };
        let mut img = image();
        put(&mut img, 0x400, 0x40);
        assert!(plan(&m, &img, &mut |_| Ok(None)).is_err());
    }

    #[test]
    fn an_image_without_mod0_has_nothing_to_do() {
        let m = module(0x1_0000_0000);
        let Some(m) = m else { return };
        let p = plan(&m, &vec![0u8; 0xc000], &mut |_| Ok(None));
        assert!(p.is_ok_and(|p| p.writes.is_empty()));
    }
}
