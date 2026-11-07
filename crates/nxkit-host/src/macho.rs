//! The segments of a 64-bit Mach-O file, read from bytes. The loader uses it for the header that dyld mapped, and to
//! check the segments against `game.json`.

use crate::error::{HostError, Result};
use crate::module::{Module, Prot};

/// `MH_MAGIC_64`.
const MAGIC_64: u32 = 0xfeed_facf;
/// `LC_SEGMENT_64`.
const LC_SEGMENT_64: u32 = 0x19;
/// The size of `mach_header_64`.
pub const HEADER_SIZE: usize = 32;

/// `VM_PROT_READ`.
pub const PROT_READ: u32 = 1;
/// `VM_PROT_WRITE`.
pub const PROT_WRITE: u32 = 2;
/// `VM_PROT_EXECUTE`.
pub const PROT_EXEC: u32 = 4;

/// One `LC_SEGMENT_64`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The segment name, for example `__NX_MAIN_TEXT`.
    pub name: String,
    /// The preferred address.
    pub vmaddr: u64,
    /// The size in memory.
    pub vmsize: u64,
    /// The offset in the file.
    pub fileoff: u64,
    /// The size in the file. The rest of `vmsize` is zero.
    pub filesize: u64,
    /// The initial protection, `PROT_*` bits.
    pub initprot: u32,
}

fn bad(m: &str) -> HostError {
    HostError::Load(format!("Mach-O: {m}"))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)?.try_into().ok().map(u32::from_le_bytes)
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)?.try_into().ok().map(u64::from_le_bytes)
}

/// The total size of the header and the load commands, from the first `HEADER_SIZE` bytes.
pub fn commands_end(header: &[u8]) -> Result<usize> {
    if u32_at(header, 0) != Some(MAGIC_64) {
        return Err(bad("not a 64-bit little endian Mach-O"));
    }
    let size = u32_at(header, 20).ok_or_else(|| bad("short header"))?;
    Ok(HEADER_SIZE + size as usize)
}

/// Reads the segments from the header and the load commands at the start of `bytes`.
pub fn segments(bytes: &[u8]) -> Result<Vec<Segment>> {
    let end = commands_end(bytes)?;
    if bytes.len() < end {
        return Err(bad("load commands are cut off"));
    }
    let ncmds = u32_at(bytes, 16).ok_or_else(|| bad("short header"))?;
    let mut out = Vec::new();
    let mut at = HEADER_SIZE;
    for _ in 0..ncmds {
        let cmd = u32_at(bytes, at).ok_or_else(|| bad("short load command"))?;
        let size = u32_at(bytes, at + 4).ok_or_else(|| bad("short load command"))? as usize;
        if size < 8 || at + size > end {
            return Err(bad("a load command has a bad size"));
        }
        if cmd == LC_SEGMENT_64 {
            let name_bytes = bytes.get(at + 8..at + 24).ok_or_else(|| bad("short segment"))?;
            let n = name_bytes.iter().position(|&c| c == 0).unwrap_or(16);
            let name = String::from_utf8_lossy(&name_bytes[..n]).into_owned();
            let f = |off: usize| u64_at(bytes, at + off).ok_or_else(|| bad("short segment"));
            out.push(Segment {
                name,
                vmaddr: f(24)?,
                vmsize: f(32)?,
                fileoff: f(40)?,
                filesize: f(48)?,
                initprot: u32_at(bytes, at + 60).ok_or_else(|| bad("short segment"))?,
            });
        }
        at += size;
    }
    Ok(out)
}

/// Checks that the library holds every segment of a module as `game.json` says. `m` has the preferred base, and
/// `segs` are read from the file, so no slide is involved. A difference means that the build and `game.json`
/// disagree, so it is an error and not a reason to copy.
pub fn verify(m: &Module, segs: &[Segment]) -> Result<()> {
    for s in &m.segments {
        let found = segs
            .iter()
            .find(|x| x.name == s.name)
            .ok_or_else(|| HostError::Load(format!("module {}: the library has no segment {}", m.name, s.name)))?;
        let want = if s.prot == Prot::Exec { PROT_READ | PROT_EXEC } else { PROT_READ | PROT_WRITE };
        let at = m.segment_addr(s);
        // The library pads the file bytes of a segment to a page, so only the rounded size is comparable.
        let file = s.file_size.div_ceil(0x4000) * 0x4000;
        if found.vmaddr != at || found.vmsize != s.size || found.initprot != want || found.filesize != file {
            return Err(HostError::Load(format!(
                "module {}: segment {} is {:#x} + {:#x} (file bytes {:#x}, protection {}) in the library and {at:#x} + {:#x} (file bytes {file:#x}, protection {want}) in game.json",
                m.name, s.name, found.vmaddr, found.vmsize, found.filesize, found.initprot, s.size
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use crate::module::{parse, samples::NEAR};

    fn sample() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC_64.to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&72u32.to_le_bytes());
        b.extend_from_slice(&[0; 8]);
        b.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
        b.extend_from_slice(&72u32.to_le_bytes());
        let mut name = [0u8; 16];
        name[..14].copy_from_slice(b"__NX_MAIN_TEXT");
        b.extend_from_slice(&name);
        for v in [0x1_0000_0000u64, 0x8000, 0x4000, 0x8000] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(&5u32.to_le_bytes());
        b.extend_from_slice(&[0; 8]);
        b
    }

    #[test]
    fn reads_a_segment() {
        let s = segments(&sample()).unwrap_or_default();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "__NX_MAIN_TEXT");
        assert_eq!(s[0].vmaddr, 0x1_0000_0000);
        assert_eq!(s[0].fileoff, 0x4000);
        assert_eq!(s[0].initprot, PROT_READ | PROT_EXEC);
    }

    #[test]
    fn verifies_the_segments_of_a_module() {
        let m = json::parse(NEAR).ok().and_then(|v| parse(&v, 0).ok());
        let Some(m) = m else { return };
        let seg = |name: &str, off: u64, size: u64, fsize: u64, prot: u32| Segment {
            name: name.into(),
            vmaddr: 0x1_0000_0000 + off,
            vmsize: size,
            fileoff: off,
            filesize: fsize,
            initprot: prot,
        };
        let mut segs = vec![
            seg("__NX_MAIN_TEXT", 0, 0x8000, 0x8000, 5),
            seg("__NX_MAIN_DATA", 0x8000, 0x8000, 0x4000, 3),
            seg("__NX_MAIN_PATCH", 0x10000, 0x10000, 0x4000, 5),
        ];
        assert!(verify(&m, &segs).is_ok());
        segs[1].initprot = 5;
        assert!(verify(&m, &segs).is_err());
        segs.pop();
        assert!(verify(&m, &segs).is_err());
    }

    #[test]
    fn refuses_a_cut_off_file() {
        let b = sample();
        assert!(segments(&b[..60]).is_err());
        assert!(segments(&b[4..]).is_err());
    }
}
