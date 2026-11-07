//! Where each module's parts go in the game's address space and in `nxgame.dylib`.
//!
//! The rewriter writes `<name>.patched.bin` and `<name>.fixups.tsv`. The header lines of the TSV (`# KEY<TAB>value`)
//! tell where the parts are (`nxb-rewrite` `rewrite.rs`). Offsets are from the module's base.
//!
//! Near layout (no `IMAGE_SIZE` key): the file is the image, the zeroed bss, the key page, padding, then the patch
//! area at `PATCH_OFF` (positive). The file offset is the memory offset.
//!
//! Far layout (`IMAGE_SIZE` and `FILE_IMAGE_LEN`): the file is the image padded to `FILE_IMAGE_LEN`, then the patch
//! area, which belongs at `PATCH_OFF` (negative, before the base). The bss and the key page follow the image up to
//! `IMAGE_SIZE` and are zero.
//!
//! Each module becomes three segments: `__NX_<NAME>_TEXT` (r-x, text and rodata up to the data's first page),
//! `__NX_<NAME>_DATA` (rw-, data, bss and key page) and `__NX_<NAME>_PATCH` (r-x, the patch area).

use std::collections::BTreeMap;

/// The page size of the Mac (and the alignment of every segment).
pub const PAGE: u64 = 0x4000;

/// Where the first module's lowest segment starts. One constant, so the host and the build agree. The addresses are
/// preferred ones: dyld slides the image when it loads it.
pub const GUEST_BASE: u64 = 0x100_0000_0000;

/// The distance modules keep from each other.
const MODULE_ALIGN: u64 = 0x1_0000;

/// A module's segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The Mach-O segment name.
    pub name: String,
    /// The kind: `TEXT`, `DATA` or `PATCH`.
    pub kind: &'static str,
    /// The offset from the module's base (negative for a far patch area).
    pub offset: i64,
    /// Where the bytes start in `<name>.patched.bin`.
    pub file_off: usize,
    /// How many bytes the file gives. The rest of `mem_size` is zero.
    pub file_len: usize,
    /// The size in memory (a multiple of [`PAGE`]).
    pub mem_size: u64,
    /// Whether the segment is executable (r-x), else read and write.
    pub exec: bool,
}

/// One module's parts, with offsets from its base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The module's name.
    pub name: String,
    /// Its segments, lowest offset first.
    pub segments: Vec<Segment>,
    /// The patch area: offset from the base and size.
    pub patch: (i64, u64),
    /// The key page: offset from the base and size.
    pub key_page: (u64, u64),
}

/// The header lines of a `.fixups.tsv`.
pub struct Fixups(BTreeMap<String, String>);

impl Fixups {
    /// Reads the `# KEY<TAB>value` lines at the top of the file.
    pub fn parse(text: &str) -> Fixups {
        let mut m = BTreeMap::new();
        for l in text.lines() {
            let Some(rest) = l.strip_prefix("# ") else {
                continue;
            };
            if let Some((k, v)) = rest.split_once('\t') {
                m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
            }
        }
        Fixups(m)
    }

    /// A key's value as a signed number (`0x..` or `-0x..`).
    pub fn num(&self, key: &str) -> Result<Option<i64>, String> {
        let Some(v) = self.0.get(key) else {
            return Ok(None);
        };
        let (neg, digits) = v.strip_prefix('-').map_or((false, v.as_str()), |d| (true, d));
        let n = match digits.strip_prefix("0x") {
            Some(h) => i64::from_str_radix(h, 16),
            None => digits.parse(),
        }
        .map_err(|_| format!("fixups: {key} is not a number: {v}"))?;
        Ok(Some(if neg { -n } else { n }))
    }

    fn need(&self, key: &str) -> Result<i64, String> {
        self.num(key)?
            .ok_or_else(|| format!("fixups: the header has no {key}"))
    }

    /// Whether the header has the key.
    pub fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
}

fn up(v: u64) -> u64 {
    v.div_ceil(PAGE) * PAGE
}

/// The part of the segment name that stands for the module: its name in capitals when all three names fit into 16
/// bytes, else `M<index>`.
fn tag(name: &str, index: usize) -> String {
    let up = name.to_ascii_uppercase();
    if up.len() <= 5 && up.bytes().all(|b| b.is_ascii_alphanumeric()) {
        up
    } else {
        format!("M{index}")
    }
}

/// The segments of one module. `file_len` is the length of `<name>.patched.bin`, `index` the module's place in the
/// load order.
pub fn shape(name: &str, index: usize, file_len: usize, f: &Fixups) -> Result<Shape, String> {
    let bad = |m: String| format!("module {name}: {m}");
    if f.has("BACK_OFF") {
        return Err(format!(
            "unsupported: module {name} has the two-area layout (BACK_OFF): the link step does not place a second code area"
        ));
    }
    let patch_off = f.need("PATCH_OFF")?;
    let key_off = f.need("KEY_PAGE_OFF")? as u64;
    let key_size = f.need("KEY_PAGE_SIZE")? as u64;
    let text_end = f.need("TEXT_END")? as u64;
    let data_off = f.need("DATA_OFF")? as u64;
    let far = f.num("IMAGE_SIZE")?;
    let p = data_off / PAGE * PAGE;
    if p < up(text_end) {
        return Err(bad(format!(
            "the data starts at {data_off:#x}, in the last 16K page of the code (text ends at {text_end:#x}): the page cannot be both r-x and rw-"
        )));
    }
    if patch_off.unsigned_abs() % PAGE != 0 {
        return Err(bad(format!("the patch area at {patch_off:#x} is not 16K aligned")));
    }
    let tag = tag(name, index);
    let seg = |kind: &'static str, offset: i64, file_off: usize, file_len: usize, mem: u64, exec: bool| Segment {
        name: format!("__NX_{tag}_{kind}"),
        kind,
        offset,
        file_off,
        file_len,
        mem_size: up(mem),
        exec,
    };
    let mut segments = vec![seg("TEXT", 0, 0, p as usize, p, true)];
    let patch_size;
    match far {
        None => {
            // The rewriter puts the patch area at the next 64K border after the image, and the key page at the next 16K
            // border. The bytes between the key page and the patch area are zero and nothing points there. No segment
            // covers them, so the host maps them with no access.
            let key_end = key_off + key_size;
            if patch_off < 0 || (patch_off as u64) < up(key_end) {
                return Err(bad(format!(
                    "the near layout needs the patch area ({patch_off:#x}) after the key page (end {key_end:#x})"
                )));
            }
            if file_len < patch_off as usize {
                return Err(bad(format!("the file ({file_len:#x} bytes) ends before the patch area")));
            }
            patch_size = (file_len - patch_off as usize) as u64;
            segments.push(seg("DATA", p as i64, p as usize, (key_end - p) as usize, key_end - p, false));
            segments.push(seg("PATCH", patch_off, patch_off as usize, patch_size as usize, patch_size, true));
        }
        Some(image_size) => {
            let image_size = image_size as u64;
            let image_len = f.need("FILE_IMAGE_LEN")? as usize;
            if patch_off >= 0 || file_len < image_len || image_len as u64 > image_size {
                return Err(bad(format!(
                    "the far layout is inconsistent (patch {patch_off:#x}, file {file_len:#x}, image in file {image_len:#x}, image {image_size:#x})"
                )));
            }
            if key_off + key_size > image_size {
                return Err(bad("the key page is outside the image".into()));
            }
            patch_size = (file_len - image_len) as u64;
            if patch_off + up(patch_size) as i64 > 0 {
                return Err(bad(format!("the patch area at {patch_off:#x} reaches the base")));
            }
            let data_file = image_len.saturating_sub(p as usize);
            segments.push(seg("DATA", p as i64, p as usize, data_file, image_size - p, false));
            segments.insert(0, seg("PATCH", patch_off, image_len, patch_size as usize, patch_size, true));
        }
    }
    segments.sort_by_key(|s| s.offset);
    Ok(Shape {
        name: name.to_string(),
        segments,
        patch: (patch_off, patch_size),
        key_page: (key_off, key_size),
    })
}

impl Shape {
    /// The lowest and the highest offset the module maps: `(low <= 0, high > 0)`.
    pub fn extent(&self) -> (i64, u64) {
        let lo = self.segments.iter().map(|s| s.offset).min().unwrap_or(0).min(0);
        let hi = self.segments.iter().map(|s| s.offset + s.mem_size as i64).max().unwrap_or(0).max(0);
        (lo, hi as u64)
    }
}

/// The bases of the modules in load order, in a row from [`GUEST_BASE`] with a gap between them.
pub fn bases(shapes: &[Shape]) -> Vec<u64> {
    let mut cursor = GUEST_BASE;
    let mut out = Vec::with_capacity(shapes.len());
    for s in shapes {
        let (lo, hi) = s.extent();
        let base = (cursor + lo.unsigned_abs()).div_ceil(MODULE_ALIGN) * MODULE_ALIGN;
        out.push(base);
        cursor = (base + hi).div_ceil(MODULE_ALIGN) * MODULE_ALIGN + MODULE_ALIGN;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near() -> Fixups {
        Fixups::parse(
            "# PATCH_OFF\t0x60000\n# KEY_PAGE_OFF\t0x40000\n# KEY_PAGE_SIZE\t0x4000\n# TEXT_END\t0x10000\n# RO_OFF\t0x10000\n# DATA_OFF\t0x20000\nrow\tx\n",
        )
    }

    fn far() -> Fixups {
        Fixups::parse(
            "# PATCH_OFF\t-0x20000\n# KEY_PAGE_OFF\t0x40000\n# KEY_PAGE_SIZE\t0x4000\n# TEXT_END\t0x10000\n# RO_OFF\t0x10000\n# DATA_OFF\t0x20000\n# IMAGE_SIZE\t0x44000\n# FILE_IMAGE_LEN\t0x30000\n",
        )
    }

    #[test]
    fn a_near_module_has_text_data_and_patch() {
        let s = shape("main", 0, 0x70000, &near()).unwrap();
        let k: Vec<_> = s.segments.iter().map(|x| (x.kind, x.offset, x.mem_size)).collect();
        assert_eq!(k, [("TEXT", 0, 0x20000), ("DATA", 0x20000, 0x24000), ("PATCH", 0x60000, 0x10000)]);
        assert_eq!(s.segments[1].file_len, 0x24000);
        assert_eq!(s.extent(), (0, 0x70000));
    }

    #[test]
    fn a_far_module_puts_the_patch_area_before_the_base() {
        let s = shape("main", 0, 0x32000, &far()).unwrap();
        assert_eq!(s.segments[0].kind, "PATCH");
        assert_eq!((s.segments[0].offset, s.segments[0].file_off, s.segments[0].file_len), (-0x20000, 0x30000, 0x2000));
        assert_eq!(s.segments[2].mem_size, 0x24000);
        assert_eq!(s.segments[2].file_len, 0x10000);
        assert_eq!(s.extent(), (-0x20000, 0x44000));
    }

    #[test]
    fn long_names_use_the_index_and_two_areas_are_refused() {
        let s = shape("subsdk0", 1, 0x70000, &near()).unwrap();
        assert_eq!(s.segments[0].name, "__NX_M1_TEXT");
        assert!(s.segments.iter().all(|x| x.name.len() <= 16));
        let two = Fixups::parse("# BACK_OFF\t0x1000\n");
        assert!(shape("main", 0, 0, &two).unwrap_err().starts_with("unsupported"));
    }

    #[test]
    fn data_inside_the_last_code_page_is_refused() {
        let f = Fixups::parse(
            "# PATCH_OFF\t0x60000\n# KEY_PAGE_OFF\t0x40000\n# KEY_PAGE_SIZE\t0x4000\n# TEXT_END\t0x1f800\n# RO_OFF\t0x1f800\n# DATA_OFF\t0x1f800\n",
        );
        assert!(shape("rtld", 0, 0x70000, &f).is_err());
    }

    #[test]
    fn bases_do_not_overlap() {
        let a = shape("main", 0, 0x32000, &far()).unwrap();
        let b = shape("sdk", 1, 0x70000, &near()).unwrap();
        let bs = bases(&[a.clone(), b.clone()]);
        assert!(bs[0] as i64 + a.extent().0 >= GUEST_BASE as i64);
        assert!(bs[1] as i64 + b.extent().0 >= (bs[0] + a.extent().1) as i64);
        assert_eq!(bs[0] % MODULE_ALIGN, 0);
    }
}
