//! The files of a `DMROMFS1` image with their paths, offsets and sizes.

use crate::fileext::FileExt;
use std::fs::File;
use std::path::Path;

use crate::error::{ExtractError, Result};

/// A file of an image: (relative path, offset, size).
pub type ImageFile = (String, u64, u64);

/// Lists the files of a `DMROMFS1` image with their paths relative to the RomFS root.
pub fn image_files(path: &Path) -> Result<(File, Vec<ImageFile>)> {
    let f =
        File::open(path).map_err(|e| ExtractError::io(format!("open {}", path.display()), e))?;
    let mut h = [0u8; 32];
    f.read_exact_at(&mut h, 0)
        .map_err(|e| ExtractError::io("read image header", e))?;
    if &h[..8] != b"DMROMFS1" {
        return Err(ExtractError::format(format!(
            "{}: not a DMROMFS1 image",
            path.display()
        )));
    }
    let rd = |o: usize| {
        u64::from_le_bytes([
            h[o],
            h[o + 1],
            h[o + 2],
            h[o + 3],
            h[o + 4],
            h[o + 5],
            h[o + 6],
            h[o + 7],
        ])
    };
    let (index, size, count) = (rd(8), rd(16), rd(24) as usize);
    if size > (1 << 30) || count.checked_mul(24).is_none_or(|n| n as u64 > size) {
        return Err(ExtractError::format("image index size"));
    }
    let mut idx = vec![0u8; size as usize];
    f.read_exact_at(&mut idx, index)
        .map_err(|e| ExtractError::io("read image index", e))?;
    let names = count * 24;
    let ent = |i: usize| {
        let e = &idx[i * 24..i * 24 + 24];
        let a = u64::from_le_bytes([e[0], e[1], e[2], e[3], e[4], e[5], e[6], e[7]]);
        let b = u64::from_le_bytes([e[8], e[9], e[10], e[11], e[12], e[13], e[14], e[15]]);
        let no = u32::from_le_bytes([e[16], e[17], e[18], e[19]]) as usize;
        let nl = u16::from_le_bytes([e[20], e[21]]) as usize;
        (a, b, no, nl, e[22] == 0)
    };
    let mut out = Vec::new();
    // (entry, path)
    let mut stack = vec![(0usize, String::new())];
    while let Some((i, p)) = stack.pop() {
        let (a, b, _, _, dir) = ent(i);
        if !dir {
            continue;
        }
        for c in a as usize..(a + b) as usize {
            if c >= count {
                return Err(ExtractError::format("image entry outside the index"));
            }
            let (ca, cb, no, nl, cdir) = ent(c);
            let name = idx
                .get(names + no..names + no + nl)
                .ok_or_else(|| ExtractError::format("image name outside its table"))?;
            let name = String::from_utf8_lossy(name);
            let cp = if p.is_empty() {
                name.into_owned()
            } else {
                format!("{p}/{name}")
            };
            if cdir {
                stack.push((c, cp));
            } else {
                out.push((cp, ca, cb));
            }
        }
    }
    Ok((f, out))
}
