//! The file list of a RomFS image: a `DMROMFS1` image (what the extract stage writes as `RomFS.img`) or a RomFS partition.

use nxb_extract::romfs::read_tables;
use nxb_extract::storage::{FileStorage, Storage};
use std::path::Path;

/// The files of a `DMROMFS1` image (`RomFS.img`): `(path, size, offset in the image)`.
fn dm_image_files(img: &Path) -> Result<Vec<nxb_extract::romfs::RomEntry>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let bad = |m: &str| format!("invalid RomFS image {m}: {}", img.display());
    let mut f = std::fs::File::open(img).map_err(|e| format!("{}: {e}", img.display()))?;
    let total = f.metadata().map_err(|e| e.to_string())?.len();
    let mut h = [0u8; 32];
    f.read_exact(&mut h).map_err(|_| bad("header"))?;
    let u64at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]));
    let (off, size, count) = (u64at(&h, 8), u64at(&h, 16), u64at(&h, 24) as usize);
    if &h[..8] != b"DMROMFS1"
        || count == 0
        || off < 32
        || off > total
        || size != total - off
        || count as u64 > size / 24
    {
        return Err(bad("header"));
    }
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    let mut idx = vec![0u8; size as usize];
    f.read_exact(&mut idx).map_err(|_| bad("index"))?;
    let pool = count * 24;
    let ent = |k: usize| {
        let e = &idx[k * 24..k * 24 + 24];
        (
            u64at(e, 0),
            u64at(e, 8),
            u32::from_le_bytes(e[16..20].try_into().unwrap_or([0; 4])) as usize,
            u16::from_le_bytes([e[20], e[21]]) as usize,
            e[22],
        )
    };
    let mut paths: Vec<Vec<u8>> = vec![Vec::new(); count];
    let mut out = Vec::new();
    for k in 0..count {
        let (a, b, no, nl, t) = ent(k);
        if t == 0 {
            let (a, b) = (a as usize, b as usize);
            if a > count || b > count - a {
                return Err(bad("tree"));
            }
            for j in a..a + b {
                let (_, _, jo, jl, _) = ent(j);
                let name = idx
                    .get(pool + jo..pool + jo + jl)
                    .ok_or_else(|| bad("name"))?;
                let mut p = paths[k].clone();
                p.push(b'/');
                p.extend_from_slice(name);
                paths[j] = p;
            }
        } else {
            let _ = (no, nl);
            out.push(nxb_extract::romfs::RomEntry {
                path: paths[k].clone(),
                dir: false,
                size: b,
                offset: a,
            });
        }
    }
    Ok(out)
}

/// The files of a RomFS image (`DMROMFS1` or a RomFS partition): `(path, size, offset)`, directories left out.
pub(super) fn image_files(
    s: &FileStorage,
    img: &Path,
) -> Result<Vec<nxb_extract::romfs::RomEntry>, String> {
    let mut magic = [0u8; 8];
    let dm = s.size() >= 8 && s.read_at(0, &mut magic).is_ok() && &magic == b"DMROMFS1";
    if dm {
        dm_image_files(img)
    } else {
        Ok(read_tables(s)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|e| !e.dir)
            .collect())
    }
}
