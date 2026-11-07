//! A small writer for the game's Mach-O dylib (`nxgame.dylib`).
//!
//! The file has a header segment, the module segments at fixed addresses, an empty `__LINKEDIT`, and the load
//! commands dyld needs (`LC_ID_DYLIB`, `LC_UUID`, `LC_BUILD_VERSION`, `LC_LOAD_DYLIB`, `LC_SYMTAB`). It has no
//! symbols, no relocations and no code of its own. The segment sizes are multiples of 16K and every segment has its
//! own file range. Measured on macOS arm64 (see R48): the file loads with `dlopen`, passes `codesign` and runs code
//! from a segment. dyld slides the image, so the addresses are preferred ones.

const PAGE: u64 = 0x4000;
const HEADER_SIZE: usize = 32;
const SEGMENT_CMD: usize = 72;

/// One segment to write.
pub struct Seg<'a> {
    /// The segment name, 16 bytes at most.
    pub name: &'a str,
    /// The preferred address (16K aligned).
    pub addr: u64,
    /// The bytes the file holds. The rest of `mem_size` is zero when the image is loaded.
    pub data: &'a [u8],
    /// The size in memory (16K aligned, at least as large as the data rounded up to 16K).
    pub mem_size: u64,
    /// Whether the segment is r-x, else rw-.
    pub exec: bool,
}

fn segment_cmd(name: &str, addr: u64, vmsize: u64, fileoff: u64, filesize: u64, prot: u32) -> Vec<u8> {
    let mut n = [0u8; 16];
    n[..name.len()].copy_from_slice(name.as_bytes());
    let mut c = Vec::with_capacity(SEGMENT_CMD);
    c.extend_from_slice(&0x19u32.to_le_bytes());
    c.extend_from_slice(&(SEGMENT_CMD as u32).to_le_bytes());
    c.extend_from_slice(&n);
    for v in [addr, vmsize, fileoff, filesize] {
        c.extend_from_slice(&v.to_le_bytes());
    }
    for v in [prot, prot, 0, 0] {
        c.extend_from_slice(&v.to_le_bytes());
    }
    c
}

fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn padded(s: &[u8], to: usize) -> Vec<u8> {
    let mut v = s.to_vec();
    v.resize(to, 0);
    v
}

/// Writes the dylib. `segs` must be in ascending address order without overlap. `uuid` is the image's identity.
pub fn write(segs: &[Seg], uuid: [u8; 16]) -> Result<Vec<u8>, String> {
    let first = segs.first().ok_or("macho: no segments")?;
    let mut end = 0u64;
    for s in segs {
        if s.name.is_empty() || s.name.len() > 16 || !s.name.is_ascii() {
            return Err(format!("macho: segment name {:?} is not 1 to 16 ASCII bytes", s.name));
        }
        if s.addr % PAGE != 0 || s.mem_size % PAGE != 0 || s.mem_size == 0 {
            return Err(format!("macho: segment {} is not 16K aligned", s.name));
        }
        if (s.data.len() as u64) > s.mem_size {
            return Err(format!("macho: segment {} has more bytes than memory", s.name));
        }
        if s.addr < end {
            return Err(format!("macho: segment {} overlaps the one before it or is out of order", s.name));
        }
        end = s.addr + s.mem_size;
    }
    if first.addr < PAGE {
        return Err("macho: no room for the header segment before the first one".into());
    }
    let mut cmds = vec![segment_cmd("__TEXT", first.addr - PAGE, PAGE, 0, PAGE, 5)];
    let mut body: Vec<u8> = Vec::new();
    let mut off = PAGE;
    for s in segs {
        let file = (s.data.len() as u64).div_ceil(PAGE) * PAGE;
        cmds.push(segment_cmd(s.name, s.addr, s.mem_size, off, file, if s.exec { 5 } else { 3 }));
        body.extend_from_slice(s.data);
        body.resize(body.len() + (file as usize - s.data.len()), 0);
        off += file;
    }
    cmds.push(segment_cmd("__LINKEDIT", end, PAGE, off, 8, 1));
    let id = padded(b"@rpath/nxgame.dylib", 24);
    let mut c = words(&[0xd, 24 + id.len() as u32, 24, 2, 0x10000, 0x10000]);
    c.extend_from_slice(&id);
    cmds.push(c);
    let mut c = words(&[0x1b, 24]);
    c.extend_from_slice(&uuid);
    cmds.push(c);
    cmds.push(words(&[0x32, 24, 1, 0xe0000, 0xe0000, 0]));
    let lib = padded(b"/usr/lib/libSystem.B.dylib", 32);
    let mut c = words(&[0xc, 24 + lib.len() as u32, 24, 2, 0x10000, 0x10000]);
    c.extend_from_slice(&lib);
    cmds.push(c);
    cmds.push(words(&[2, 24, off as u32, 0, (off + 4) as u32, 4]));
    let size: usize = cmds.iter().map(Vec::len).sum();
    if HEADER_SIZE + size > PAGE as usize {
        return Err(format!(
            "macho: the load commands ({} bytes) do not fit the header page",
            HEADER_SIZE + size
        ));
    }
    if off > u64::from(u32::MAX) {
        return Err("macho: the file is larger than 4 GiB".into());
    }
    let mut out = words(&[0xfeed_facf, 0x0100_000c, 0, 6, cmds.len() as u32, size as u32, 0x0010_0085, 0]);
    for c in &cmds {
        out.extend_from_slice(c);
    }
    out.resize(PAGE as usize, 0);
    out.extend_from_slice(&body);
    out.extend_from_slice(&[0; 8]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32at(d: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
    }

    #[test]
    fn writes_a_header_and_the_segment_bytes() {
        let code = [0x40, 0x05, 0x80, 0xd2, 0xc0, 0x03, 0x5f, 0xd6];
        let segs = [
            Seg { name: "__NX_A_TEXT", addr: 0x100_0001_0000, data: &code, mem_size: PAGE, exec: true },
            Seg { name: "__NX_A_DATA", addr: 0x100_0001_4000, data: b"hello", mem_size: 3 * PAGE, exec: false },
        ];
        let f = write(&segs, [7; 16]).unwrap();
        assert_eq!(u32at(&f, 0), 0xfeed_facf);
        assert_eq!(u32at(&f, 16), 9);
        assert_eq!(&f[PAGE as usize..PAGE as usize + 8], &code);
        assert_eq!(&f[2 * PAGE as usize..2 * PAGE as usize + 5], b"hello");
        assert_eq!(f.len() as u64, 3 * PAGE + 8);
    }

    #[test]
    fn refuses_bad_input() {
        let s = |name, addr| Seg { name, addr, data: &[], mem_size: PAGE, exec: false };
        assert!(write(&[], [0; 16]).is_err());
        assert!(write(&[s("__THIS_NAME_IS_TOO_LONG", 0x1_0000)], [0; 16]).is_err());
        assert!(write(&[s("__A", 0x1_0001)], [0; 16]).is_err());
        assert!(write(&[s("__A", 0x2_0000), s("__B", 0x2_0000)], [0; 16]).is_err());
    }
}
