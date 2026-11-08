//! One module of `game.json`: where its segments lie, which files describe it and where its entry is.

use std::path::PathBuf;

use crate::error::Result;
use crate::json::Value;
use crate::manifest::{bad, field, num, text};

/// The page size of the guest address space and of every segment of the build.
const PAGE: u64 = 0x4000;

/// An area of a module, as an offset from the module base and a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Region {
    /// Offset from the module base.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
}

/// What a segment may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prot {
    /// `r-x`: the code, the read-only data and the patch area.
    Exec,
    /// `rw-`: the data, the bss and the key page.
    Write,
}

/// One segment of a module, as the build put it into `nxgame.dylib` (`__NX_<NAME>_TEXT`, `_DATA`, `_PATCH`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The Mach-O segment name.
    pub name: String,
    /// The offset from the module base. It is negative for the patch area of a far layout.
    pub offset: i64,
    /// The size in memory, a multiple of 16K. The bytes after `file_size` are zero.
    pub size: u64,
    /// Where the bytes start in the image file of the module.
    pub file_offset: u64,
    /// How many bytes the image file gives.
    pub file_size: u64,
    /// The protection after loading.
    pub prot: Prot,
}

/// The patch area of the rewriter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Patch {
    /// The offset from the module base. It is negative for a far layout.
    pub offset: i64,
    /// The size in bytes.
    pub size: u64,
}

/// One module of the game.
#[derive(Debug, Clone)]
pub struct Module {
    /// `rtld`, `main`, `subsdk0` and so on, and `sdk`.
    pub name: String,
    /// The guest address of offset 0. In `game.json` it is the preferred address. After the load it is the address
    /// where the module really sits.
    pub base: u64,
    /// The offset after the last byte of the highest segment.
    pub size: u64,
    /// The offset of the first byte of the lowest segment, 0 or less. It is below 0 for the patch area of a far layout.
    pub low: i64,
    /// The bytes from `low` to `size`: all the memory the module maps, which `size` alone leaves short by `-low`.
    pub span: u64,
    /// The code.
    pub text: Region,
    /// The read-only data.
    pub rodata: Region,
    /// The data, from the first 16K page of the data segment. The bss follows it.
    pub data: Region,
    /// The size of the bss, which starts at the end of `data`.
    pub bss_size: u64,
    /// The rewritten flat image, relative to the run folder.
    pub image: PathBuf,
    /// The import list, relative to the run folder.
    pub imports: PathBuf,
    /// The export list, relative to the run folder.
    pub exports: PathBuf,
    /// The offset of the entry point. Only `main` has one.
    pub entry: Option<u64>,
    /// The offset of the key page, which is 0x4000 bytes (`KEY_PAGE_OFF` of the rewriter).
    pub key_page: u64,
    /// The gate list of the rewriter, relative to the run folder.
    pub gates: PathBuf,
    /// The segments, lowest offset first.
    pub segments: Vec<Segment>,
    /// The patch area. It lies in one of the segments.
    pub patch: Patch,
    /// The build ID of the module, 64 hex digits.
    pub build_id: String,
    /// The `fixups.tsv` of the rewriter. It names offsets for the engine hooks of the services. The host does not
    /// read it.
    pub fixups: PathBuf,
}

impl Module {
    /// The first byte of the module. The patch area of a far layout lies before `base`.
    pub fn start(&self) -> u64 {
        self.base.saturating_add_signed(self.low)
    }

    /// The first byte after the module.
    pub fn end(&self) -> u64 {
        self.start().saturating_add(self.span)
    }

    /// The offset where the bss starts, right after the data.
    pub fn bss_offset(&self) -> u64 {
        self.data.offset.saturating_add(self.data.size)
    }

    /// The guest address of a segment.
    pub fn segment_addr(&self, s: &Segment) -> u64 {
        self.base.saturating_add_signed(s.offset)
    }

    /// The segment that holds the offset, if any.
    pub fn segment_at(&self, offset: u64) -> Option<&Segment> {
        let off = i64::try_from(offset).ok()?;
        self.segments
            .iter()
            .find(|s| off >= s.offset && off < s.offset.saturating_add_unsigned(s.size))
    }

    /// Whether `[offset, offset + len)` lies in one writable segment.
    pub fn writable(&self, offset: u64, len: u64) -> bool {
        let Some(s) = self.segment_at(offset) else {
            return false;
        };
        let end = i64::try_from(offset.saturating_add(len)).unwrap_or(i64::MAX);
        s.prot == Prot::Write && end <= s.offset.saturating_add_unsigned(s.size)
    }

    /// The same module at `base + delta`.
    pub fn moved(&self, delta: i64) -> Option<Module> {
        let mut m = self.clone();
        m.base = self.base.checked_add_signed(delta)?;
        Some(m)
    }

    /// Checks the segments against each other and against the other fields.
    pub fn check(&self) -> Result<()> {
        let n = &self.name;
        if self.base % PAGE != 0 || self.size == 0 {
            return Err(bad(format!(
                "module {n}: base {:#x} is not page aligned or size is 0",
                self.base
            )));
        }
        if self.segments.is_empty() {
            return Err(bad(format!("module {n}: no segments")));
        }
        let mut end = i64::MIN;
        for s in &self.segments {
            let top = s.offset.saturating_add_unsigned(s.size);
            if s.offset % PAGE as i64 != 0 || s.size == 0 || s.size % PAGE != 0 {
                return Err(bad(format!(
                    "module {n}: segment {} is not 16K aligned",
                    s.name
                )));
            }
            if s.file_size > s.size {
                return Err(bad(format!(
                    "module {n}: segment {} has more file bytes than memory",
                    s.name
                )));
            }
            if s.offset < end {
                return Err(bad(format!(
                    "module {n}: segment {} overlaps the one before it or is out of order",
                    s.name
                )));
            }
            end = top;
        }
        if end != self.size as i64 {
            return Err(bad(format!(
                "module {n}: size {:#x} is not the end of the last segment {end:#x}",
                self.size
            )));
        }
        let lowest = self
            .segments
            .iter()
            .map(|s| s.offset)
            .min()
            .unwrap_or(0)
            .min(0);
        if self.low != lowest || self.span != self.size.wrapping_add_signed(self.low.wrapping_neg())
        {
            return Err(bad(format!(
                "module {n}: low {:#x} and span {:#x} do not fit the segments (lowest offset {lowest:#x}, end {end:#x})",
                self.low, self.span
            )));
        }
        let bss_end = self.bss_offset().saturating_add(self.bss_size);
        if !self.writable(self.data.offset, bss_end.saturating_sub(self.data.offset)) {
            return Err(bad(format!(
                "module {n}: data and bss are not in one writable segment"
            )));
        }
        if self.key_page % PAGE != 0 || !self.writable(self.key_page, PAGE) {
            return Err(bad(format!(
                "module {n}: key page {:#x} is not aligned or not in a writable segment",
                self.key_page
            )));
        }
        if self
            .entry
            .is_some_and(|e| self.segment_at(e).is_none_or(|s| s.prot != Prot::Exec))
        {
            return Err(bad(format!(
                "module {n}: the entry is not in an r-x segment"
            )));
        }
        if self.patch.size > 0 {
            let inside = self.segments.iter().any(|s| {
                s.prot == Prot::Exec
                    && s.offset <= self.patch.offset
                    && self.patch.offset.saturating_add_unsigned(self.patch.size)
                        <= s.offset.saturating_add_unsigned(s.size)
            });
            if !inside {
                return Err(bad(format!(
                    "module {n}: the patch area is not inside an r-x segment"
                )));
            }
        }
        Ok(())
    }
}

fn signed(v: &Value, key: &str, ctx: &str) -> Result<i64> {
    field(v, key, ctx)?
        .as_i64()
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a number")))
}

fn region(v: &Value, key: &str, ctx: &str) -> Result<Region> {
    let r = field(v, key, ctx)?;
    let c = format!("{ctx}.{key}");
    Ok(Region {
        offset: num(r, "offset", &c)?,
        size: num(r, "size", &c)?,
    })
}

fn segment(v: &Value, ctx: &str) -> Result<Segment> {
    let name = text(v, "name", ctx)?;
    let c = format!("{ctx}.{name}");
    let prot = match text(v, "prot", &c)?.as_str() {
        "r-x" => Prot::Exec,
        "rw-" => Prot::Write,
        other => return Err(bad(format!("{c}: \"prot\" is {other}, not r-x or rw-"))),
    };
    Ok(Segment {
        offset: signed(v, "offset", &c)?,
        size: num(v, "size", &c)?,
        file_offset: num(v, "file_offset", &c)?,
        file_size: num(v, "file_size", &c)?,
        prot,
        name,
    })
}

/// Reads one entry of `modules`. The keys the host does not use are ignored on purpose.
pub fn parse(v: &Value, i: usize) -> Result<Module> {
    let name = text(v, "name", &format!("modules[{i}]"))?;
    let ctx = format!("module {name}");
    let entry = match v.get("entry") {
        None | Some(Value::Null) => None,
        Some(e) => Some(
            e.as_u64()
                .ok_or_else(|| bad(format!("{ctx}: \"entry\" is not a number")))?,
        ),
    };
    let segments = field(v, "segments", &ctx)?
        .as_array()
        .ok_or_else(|| bad(format!("{ctx}: \"segments\" is not an array")))?
        .iter()
        .map(|s| segment(s, &format!("{ctx}.segments")))
        .collect::<Result<Vec<_>>>()?;
    let patch = field(v, "patch", &ctx)?;
    Ok(Module {
        base: num(v, "base", &ctx)?,
        size: num(v, "size", &ctx)?,
        low: signed(v, "low", &ctx)?,
        span: num(v, "span", &ctx)?,
        text: region(v, "text", &ctx)?,
        rodata: region(v, "rodata", &ctx)?,
        data: region(v, "data", &ctx)?,
        bss_size: num(v, "bss_size", &ctx)?,
        image: PathBuf::from(text(v, "image", &ctx)?),
        imports: PathBuf::from(text(v, "imports", &ctx)?),
        exports: PathBuf::from(text(v, "exports", &ctx)?),
        entry,
        key_page: num(v, "key_page", &ctx)?,
        gates: PathBuf::from(text(v, "gates", &ctx)?),
        segments,
        patch: Patch {
            offset: signed(patch, "offset", &ctx)?,
            size: num(patch, "size", &ctx)?,
        },
        build_id: text(v, "build_id", &ctx)?,
        fixups: PathBuf::from(text(v, "fixups", &ctx)?),
        name,
    })
}

/// Module entries for the tests of this crate.
#[cfg(test)]
pub(crate) mod samples {
    /// A near module: text, data and the patch area follow each other. It spans 0x100000000 to 0x100020000.
    pub(crate) const NEAR: &str = r#"{"name": "main", "base": "0x100000000", "size": "0x20000", "low": 0, "span": "0x20000",
        "text": {"offset": 0, "size": 16384}, "rodata": {"offset": 16384, "size": 16384},
        "data": {"offset": 32768, "size": 16384}, "bss_size": 8192, "image": "main.bin", "imports": "main.imports.json",
        "exports": "main.exports.json", "entry": 0, "build_id": "00", "fixups": "main.fixups.tsv", "key_page": 49152,
        "gates": "main.gates.tsv", "patch": {"offset": 65536, "size": 8192},
        "segments": [
          {"name": "__NX_MAIN_TEXT", "offset": 0, "size": 32768, "file_offset": 0, "file_size": 32768, "prot": "r-x"},
          {"name": "__NX_MAIN_DATA", "offset": 32768, "size": 32768, "file_offset": 32768, "file_size": 16384, "prot": "rw-"},
          {"name": "__NX_MAIN_PATCH", "offset": 65536, "size": 65536, "file_offset": 65536, "file_size": 8192, "prot": "r-x"}]}"#;

    /// A far module: the patch area lies before the base.
    pub(crate) const FAR: &str = r#"{"name": "main", "base": "0x10000040000", "size": "0x44000", "low": -131072, "span": "0x64000",
        "text": {"offset": 0, "size": 16384}, "rodata": {"offset": 16384, "size": 16384},
        "data": {"offset": 131072, "size": 16384}, "bss_size": 16384, "image": "i", "imports": "a", "exports": "b",
        "entry": 0, "build_id": "00", "fixups": "f", "key_page": 262144, "gates": "g",
        "patch": {"offset": -131072, "size": 8192},
        "segments": [
          {"name": "__NX_MAIN_PATCH", "offset": -131072, "size": 16384, "file_offset": 196608, "file_size": 8192, "prot": "r-x"},
          {"name": "__NX_MAIN_TEXT", "offset": 0, "size": 131072, "file_offset": 0, "file_size": 131072, "prot": "r-x"},
          {"name": "__NX_MAIN_DATA", "offset": 131072, "size": 147456, "file_offset": 131072, "file_size": 65536, "prot": "rw-"}]}"#;
}

#[cfg(test)]
mod tests {
    use super::samples::FAR;
    use super::*;
    use crate::json;

    fn far() -> Option<Module> {
        parse(&json::parse(FAR).ok()?, 0).ok()
    }

    #[test]
    fn a_far_module_starts_before_its_base() {
        let m = far();
        assert!(m.is_some());
        let Some(m) = m else { return };
        assert_eq!(m.low, -131072);
        assert_eq!(m.end() - m.start(), 0x64000);
        assert_eq!(m.start(), 0x1_0000_0400_00 - 0x2_0000);
        assert_eq!(m.end(), 0x1_0000_0400_00 + 0x44000);
        assert!(m.check().is_ok(), "{:?}", m.check().err());
        assert!(m.writable(0x20000, 0x10));
        assert!(!m.writable(0x10, 0x10));
        assert!(!m.writable(0x20000, 0x40000));
        assert_eq!(
            m.segment_at(0).map(|s| s.name.as_str()),
            Some("__NX_MAIN_TEXT")
        );
    }

    #[test]
    fn a_span_that_leaves_out_the_part_below_the_base_is_refused() {
        let s = FAR.replace("\"span\": \"0x64000\"", "\"span\": \"0x44000\"");
        let m = json::parse(&s).ok().and_then(|v| parse(&v, 0).ok());
        assert!(m.is_some_and(|m| m.check().is_err()));
    }

    #[test]
    fn a_key_page_outside_the_data_is_refused() {
        let s = FAR.replace("\"key_page\": 262144", "\"key_page\": 4096");
        let m = json::parse(&s).ok().and_then(|v| parse(&v, 0).ok());
        assert!(m.is_some());
        assert!(m.is_some_and(|m| m.check().is_err()));
    }

    #[test]
    fn a_patch_area_outside_the_segments_is_refused() {
        let s = FAR.replace(
            "\"patch\": {\"offset\": -131072",
            "\"patch\": {\"offset\": -262144",
        );
        let m = json::parse(&s).ok().and_then(|v| parse(&v, 0).ok());
        assert!(m.is_some_and(|m| m.check().is_err()));
    }

    #[test]
    fn moving_a_module_keeps_the_offsets() {
        let m = far().and_then(|m| m.moved(0x4000));
        assert!(
            m.as_ref()
                .is_some_and(|m| m.base == 0x1_0000_0400_00 + 0x4000)
        );
        assert!(m.is_some_and(|m| m.start() == 0x1_0000_0400_00 + 0x4000 - 0x2_0000));
    }
}
