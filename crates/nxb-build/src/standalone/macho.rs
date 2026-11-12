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
pub(crate) struct Seg<'a> {
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

fn segment_cmd(
    name: &str,
    addr: u64,
    vmsize: u64,
    fileoff: u64,
    filesize: u64,
    prot: u32,
) -> Vec<u8> {
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
pub(crate) fn write(segs: &[Seg], uuid: [u8; 16]) -> Result<Vec<u8>, String> {
    let first = segs.first().ok_or("macho: no segments")?;
    let mut end = 0u64;
    for s in segs {
        if s.name.is_empty() || s.name.len() > 16 || !s.name.is_ascii() {
            return Err(format!(
                "macho: segment name {:?} is not 1 to 16 ASCII bytes",
                s.name
            ));
        }
        if s.addr % PAGE != 0 || s.mem_size % PAGE != 0 || s.mem_size == 0 {
            return Err(format!("macho: segment {} is not 16K aligned", s.name));
        }
        if (s.data.len() as u64) > s.mem_size {
            return Err(format!(
                "macho: segment {} has more bytes than memory",
                s.name
            ));
        }
        if s.addr < end {
            return Err(format!(
                "macho: segment {} overlaps the one before it or is out of order",
                s.name
            ));
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
        cmds.push(segment_cmd(
            s.name,
            s.addr,
            s.mem_size,
            off,
            file,
            if s.exec { 5 } else { 3 },
        ));
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
    let mut out = words(&[
        0xfeed_facf,
        0x0100_000c,
        0,
        6,
        cmds.len() as u32,
        size as u32,
        0x0010_0085,
        0,
    ]);
    for c in &cmds {
        out.extend_from_slice(c);
    }
    out.resize(PAGE as usize, 0);
    out.extend_from_slice(&body);
    out.extend_from_slice(&[0; 8]);
    Ok(out)
}

/// Applies the iOS platform version and executable header marker to the shared dylib.
pub(super) fn ios_dylib(b: &mut [u8]) -> Result<(), String> {
    fn word(b: &[u8], at: usize) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            b.get(at..at + 4)
                .ok_or("truncated Mach-O")?
                .try_into()
                .map_err(|_| "invalid integer width")?,
        ))
    }
    if word(b, 0)? != 0xfeedfacf || word(b, 4)? != 0x0100000c {
        return Err("expected arm64 Mach-O".into());
    }
    let end = 32 + word(b, 20)? as usize;
    if end > b.len() || end > 0x4000 {
        return Err("invalid Mach-O commands".into());
    }
    let mut at = 32;
    let mut found = false;
    let mut text_command = None;
    for _ in 0..word(b, 16)? {
        let cmd = word(b, at)?;
        let size = word(b, at + 4)? as usize;
        if size < 8 || !size.is_multiple_of(8) || at + size > end {
            return Err("invalid Mach-O command".into());
        }
        if cmd == 0x19
            && size >= 72
            && b.get(at + 8..at + 24) == Some(b"__TEXT\0\0\0\0\0\0\0\0\0\0")
        {
            if size != 72 || word(b, at + 64)? != 0 {
                return Err("unexpected header segment".into());
            }
            text_command = Some(at);
        }
        if cmd == 0x32 {
            if size != 24 || found {
                return Err("unexpected build-version command".into());
            }
            for (off, value) in [(8, 2u32), (12, 26 << 16), (16, 26 << 16)] {
                b[at + off..at + off + 4].copy_from_slice(&value.to_le_bytes());
            }
            found = true;
        }
        at += size;
    }
    if at != end || !found {
        return Err("missing build-version command".into());
    }
    // A standard __TEXT,__text section describes the reserved header space. The writer
    // reserves a 16 KiB header segment, so add one section inside that padding.
    let text = text_command.ok_or("missing header segment")?;
    const MARKER: usize = 0x3ffc;
    if b.len() < 0x4000 || end + 80 + 16 > MARKER || b[end..0x4000].iter().any(|v| *v != 0) {
        return Err("no room for header marker".into());
    }
    let vmaddr = u64::from_le_bytes(
        b[text + 24..text + 32]
            .try_into()
            .map_err(|_| "invalid vmaddr")?,
    );
    let section = text + 72;
    b.copy_within(section..end, section + 80);
    b[section..section + 80].fill(0);
    b[section..section + 6].copy_from_slice(b"__text");
    b[section + 16..section + 22].copy_from_slice(b"__TEXT");
    b[section + 32..section + 40].copy_from_slice(&(vmaddr + MARKER as u64).to_le_bytes());
    b[section + 40..section + 48].copy_from_slice(&4u64.to_le_bytes());
    b[section + 48..section + 52].copy_from_slice(&(MARKER as u32).to_le_bytes());
    b[section + 52..section + 56].copy_from_slice(&2u32.to_le_bytes());
    b[section + 64..section + 68].copy_from_slice(&0x80000400u32.to_le_bytes());
    b[text + 4..text + 8].copy_from_slice(&152u32.to_le_bytes());
    b[text + 64..text + 68].copy_from_slice(&1u32.to_le_bytes());
    b[20..24].copy_from_slice(&((end - 32 + 80) as u32).to_le_bytes());
    b[MARKER..MARKER + 4].copy_from_slice(&0xd65f03c0u32.to_le_bytes()); // arm64 RET
    Ok(())
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
            Seg {
                name: "__NX_A_TEXT",
                addr: 0x100_0001_0000,
                data: &code,
                mem_size: PAGE,
                exec: true,
            },
            Seg {
                name: "__NX_A_DATA",
                addr: 0x100_0001_4000,
                data: b"hello",
                mem_size: 3 * PAGE,
                exec: false,
            },
        ];
        let f = write(&segs, [7; 16]).unwrap();
        assert_eq!(u32at(&f, 0), 0xfeed_facf);
        assert_eq!(u32at(&f, 16), 9);
        assert_eq!(&f[PAGE as usize..PAGE as usize + 8], &code);
        assert_eq!(&f[2 * PAGE as usize..2 * PAGE as usize + 5], b"hello");
        assert_eq!(f.len() as u64, 3 * PAGE + 8);
    }

    #[test]
    fn ios_keeps_the_shared_segments_and_changes_only_the_header() {
        let code = [0xc0, 0x03, 0x5f, 0xd6];
        let segs = [Seg {
            name: "__NX_A_TEXT",
            addr: 0x100_0001_0000,
            data: &code,
            mem_size: PAGE,
            exec: true,
        }];
        let mac = write(&segs, [7; 16]).unwrap();
        let mut ios = mac.clone();
        ios_dylib(&mut ios).unwrap();
        assert_eq!(ios.len(), mac.len());
        assert_eq!(&ios[PAGE as usize..], &mac[PAGE as usize..]);
        assert_eq!(u32at(&ios, 16), u32at(&mac, 16));
        assert_eq!(u32at(&ios, 20), u32at(&mac, 20) + 80);
        assert_eq!(u32at(&ios, 0x3ffc), 0xd65f03c0);
        assert_eq!(u32at(&mac, 0x3ffc), 0);
        assert_eq!(&ios[104..110], b"__text");
        for (image, platform, version) in [(&mac, 1, 14 << 16), (&ios, 2, 26 << 16)] {
            let mut at = HEADER_SIZE;
            let mut found = false;
            for _ in 0..u32at(image, 16) {
                if u32at(image, at) == 0x32 {
                    assert_eq!(u32at(image, at + 8), platform);
                    assert_eq!(u32at(image, at + 12), version);
                    assert_eq!(u32at(image, at + 16), version);
                    found = true;
                }
                at += u32at(image, at + 4) as usize;
            }
            assert!(found);
        }
        assert_eq!(write(&segs, [7; 16]).unwrap(), mac);
    }

    #[test]
    fn refuses_bad_input() {
        let s = |name, addr| Seg {
            name,
            addr,
            data: &[],
            mem_size: PAGE,
            exec: false,
        };
        assert!(write(&[], [0; 16]).is_err());
        assert!(write(&[s("__THIS_NAME_IS_TOO_LONG", 0x1_0000)], [0; 16]).is_err());
        assert!(write(&[s("__A", 0x1_0001)], [0; 16]).is_err());
        assert!(write(&[s("__A", 0x2_0000), s("__B", 0x2_0000)], [0; 16]).is_err());
    }
}
