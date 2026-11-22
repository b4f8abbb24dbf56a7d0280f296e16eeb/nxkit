//! The Unreal decode of the build (`uepak.py` `job`, `finish`, `find_key`, `code_keys`, `decoded_archive`,
//! `lz4_shaders`): the shader libraries of the IoStore containers, decoded with the game's decoder ([`crate::oodle`]),
//! and the shader code archives of every Unreal pak whose shaders are not inline, with an AES-encrypted index opened
//! by the key found in main. One Oodle job (`DMOJ`, `dm/host/hle_oodle.c`) holds the compressed shaders of the paks
//! for the game's own decoder in the Mac host. LZ4 shaders are decoded here and need no job.

use std::sync::Arc;

use super::R;
use super::uepak::{Pak, footer, inline_shaders, range, ve};
use super::uetoc::{IO_SHADER_LIBRARY, Toc, dirname, inline_in_containers};
use crate::rd::u32le;
use crate::src::Src;

const ARCHIVE_VERSION: u32 = 2;
const JOB_MAGIC: u32 = 0x4a4f_4d44;
const OUT_MAGIC: u32 = 0x524f_4d44;

/// A shader of an archive: (offset from the code start, size, raw size, frequency).
type Shader = (u64, u32, u32, u8);

/// `archive`: the shaders of a version 2 shader code archive, the end of the table before the shader entries and
/// the code start.
pub(super) fn tables(b: &[u8], what: &str) -> R<(Vec<Shader>, usize, usize)> {
    let mut r = super::uepak::Rd::new(b, 0, what.to_string());
    if r.le(4)? != u64::from(ARCHIVE_VERSION) {
        return ve(format!("{what}: not a version 2 shader code archive"));
    }
    for elem in [20usize, 20, 16] {
        let n = r.le(4)? as usize;
        r.raw(n.saturating_mul(elem))?;
    }
    let head = r.pos();
    let n = r.le(4)? as usize;
    let ents = r.raw(n.saturating_mul(17))?;
    let sh = ents
        .chunks_exact(17)
        .map(|e| {
            let q = |a: usize, k: usize| {
                e[a..a + k]
                    .iter()
                    .rev()
                    .fold(0u64, |x, &y| x << 8 | u64::from(y))
            };
            (q(0, 8), q(8, 4) as u32, q(12, 4) as u32, e[16])
        })
        .collect();
    let np = r.le(4)? as usize;
    r.raw(np.saturating_mul(16))?;
    let ni = r.le(4)? as usize;
    r.raw(ni.saturating_mul(4))?;
    let code = r.pos();
    Ok((sh, head, code))
}

/// `decoded_archive`: the archive with every shader replaced by its decoded bytes (in entry order): the same tables,
/// each entry's size its raw size, the code laid out again.
pub fn decoded_archive(b: &[u8], raws: &[Vec<u8>]) -> R<Vec<u8>> {
    let (sh, head, code) = tables(b, "shader code archive")?;
    if raws.len() != sh.len() {
        return ve(format!(
            "{} decoded shaders for an archive of {}",
            raws.len(),
            sh.len()
        ));
    }
    let mut out = b[..head + 4].to_vec();
    let mut off = 0u64;
    for (&(_, _, raw, fr), d) in sh.iter().zip(raws) {
        if d.len() != raw as usize {
            return ve(format!(
                "decoded shader of {} bytes, the archive says {raw}",
                d.len()
            ));
        }
        out.extend(off.to_le_bytes());
        out.extend(raw.to_le_bytes());
        out.extend(raw.to_le_bytes());
        out.push(fr);
        off += u64::from(raw);
    }
    out.extend_from_slice(&b[head + 4 + 17 * sh.len()..code]);
    for d in raws {
        out.extend_from_slice(d);
    }
    Ok(out)
}

/// The stored bytes of shader `s` of an archive whose code starts at `code`.
fn stored<'a>(b: &'a [u8], code: usize, s: &Shader, what: &str) -> R<&'a [u8]> {
    let a = usize::try_from(s.0)
        .unwrap_or(usize::MAX)
        .saturating_add(code);
    b.get(a..a.saturating_add(s.1 as usize)).ok_or_else(|| {
        crate::SplitError::Red(format!(
            "ValueError: {what}: a shader runs past the archive"
        ))
    })
}

/// `lz4_shaders`: every shader decoded as an LZ4 block, or `None` when a compressed one is no LZ4 block that decodes
/// to its raw size as an LSLG record (Oodle: the game's own decoder is needed).
fn lz4_shaders(b: &[u8], sh: &[Shader], code: usize) -> R<Option<Vec<Vec<u8>>>> {
    let mut out = Vec::with_capacity(sh.len());
    for s in sh {
        let c = stored(b, code, s, "shader code archive")?;
        if s.1 == s.2 {
            out.push(c.to_vec());
            continue;
        }
        if s.2 as usize > 255 * c.len() {
            return Ok(None);
        }
        match crate::lz4::block_upto(c, s.2 as usize) {
            Ok(d) if d.len() == s.2 as usize && d.starts_with(b"LSLG") => out.push(d),
            _ => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// `code_keys`: 32-byte keys that main's code stores from two 16-byte literals (`stp qA, qB, [x0]` after `ldr qA`
/// and `ldr qB`, each from an `adrp` page plus its offset, within 8 instructions before it).
pub fn code_keys(img: &[u8], lo: usize) -> Vec<[u8; 32]> {
    let n = lo.min(img.len()) / 4;
    let w: Vec<u32> = (0..n)
        .map(|i| u32le(img, 4 * i).unwrap_or(0) as u32)
        .collect();
    let mut out = Vec::new();
    for (i, &x) in w.iter().enumerate() {
        if x & 0xFFFF_83E0 != 0xAD00_0000 {
            continue;
        }
        let (mut page, mut half) = ([None::<i64>; 32], [None::<i64>; 32]);
        for (j, &y) in w.iter().enumerate().take(i).skip(i.saturating_sub(8)) {
            let rd = (y & 31) as usize;
            if y & 0x9F00_0000 == 0x9000_0000 {
                let imm = i64::from((y >> 29) & 3) | i64::from((y >> 5) & 0x7FFFF) << 2;
                let imm = if imm & (1 << 20) != 0 {
                    imm - (1 << 21)
                } else {
                    imm
                };
                page[rd] = Some(((j as i64 * 4) & !0xFFF) + (imm << 12));
            } else if y & 0xFFC0_0000 == 0x3DC0_0000 && page[((y >> 5) & 31) as usize].is_some() {
                let base = page[((y >> 5) & 31) as usize].unwrap_or(0);
                half[rd] = Some(base + i64::from((y >> 10) & 0xFFF) * 16);
            } else {
                page[rd] = None;
                half[rd] = None;
            }
        }
        let (a, b) = (half[(x & 31) as usize], half[((x >> 10) & 31) as usize]);
        if let (Some(a), Some(b)) = (a, b) {
            let get = |p: i64| {
                usize::try_from(p)
                    .ok()
                    .and_then(|p| img.get(p..p.checked_add(16)?))
            };
            if let (Some(a), Some(b)) = (get(a), get(b)) {
                let mut k = [0u8; 32];
                k[..16].copy_from_slice(a);
                k[16..].copy_from_slice(b);
                out.push(k);
            }
        }
    }
    out
}

/// `find_key`: the AES key that decrypts the first index block `blk` of a pak to a short mount point: one main's code
/// stores from two 16-byte literals ([`code_keys`]), else one of main's 32-byte windows from `lo` to `hi` (read-only
/// and data segments, 4-aligned, at least 20 distinct bytes).
pub fn find_key(img: &[u8], lo: usize, hi: usize, blk: &[u8; 16]) -> Option<[u8; 32]> {
    let opens = |k: &[u8; 32]| {
        let mut d = *blk;
        crate::aes256::Aes256::new(k).decrypt_block(&mut d);
        let n = i32::from_le_bytes([d[0], d[1], d[2], d[3]]);
        0 < n && n < 512 && &d[4..7] == b"../"
    };
    if let Some(k) = code_keys(img, lo).into_iter().find(|k| opens(k)) {
        return Some(k);
    }
    let hi = hi.min(img.len());
    let mut a = lo;
    while a + 32 < hi {
        let mut k = [0u8; 32];
        k.copy_from_slice(&img[a..a + 32]);
        let mut seen = [false; 256];
        let distinct = k
            .iter()
            .filter(|&&b| !std::mem::replace(&mut seen[b as usize], true))
            .count();
        if distinct >= 20 && opens(&k) {
            return Some(k);
        }
        a += 4;
    }
    None
}

/// An archive of the decode.
#[derive(Clone, Debug)]
pub struct UeArchive {
    /// Its file name (`ShaderArchive-<library>-<format>.ushaderbytecode`).
    pub name: String,
    /// The pak (or the IoStore TOC) it came from, relative to the RomFS root.
    pub pak: String,
    /// Its path in the pak (`chunk <chunk ID in hex>` in an IoStore container).
    pub path: String,
    /// Its shader count.
    pub shaders: usize,
    /// Its compressed shaders (they go to the job).
    pub compressed: usize,
    /// `Oodle`, `LZ4` (decoded here), `IoStore, decoded` or empty.
    pub compression: &'static str,
    bytes: Vec<u8>,
}

/// The decode of one version: the archives and the records of the Oodle job.
#[derive(Debug, Default)]
pub struct UeJob {
    /// The archives: the IoStore libraries, then the paks' archives in pak and path order.
    pub archives: Vec<UeArchive>,
    /// (archive, shader, compressed bytes, raw size).
    recs: Vec<(usize, usize, Vec<u8>, u32)>,
}

/// Whether an IoStore TOC holds Unreal Engine 5 shader libraries (`Toc.of_type(IO_SHADER_LIBRARY)`).
pub fn toc_has_shader_libraries(b: &[u8], rel: &str) -> bool {
    Toc::open(b, rel).is_ok_and(|t| t.of_type(IO_SHADER_LIBRARY))
}

/// The key of an encrypted pak: the one in main that decrypts its first index block.
fn main_key(main: &[u8], src: &dyn Src, rel: &str, ioff: u64) -> R<[u8; 32]> {
    let img = crate::nso::image(main)?;
    let lo = u32le(main, 0x24)?;
    let blk: [u8; 16] = range(src, ioff, 16, rel)?.try_into().unwrap_or([0; 16]);
    find_key(&img, lo, img.len(), &blk).ok_or_else(|| {
        crate::SplitError::Red(format!(
            "ValueError: {rel}: the index is encrypted and no key in main (a 32-byte window or two 16-byte literals its code stores together) decrypts it"
        ))
    })
}

impl UeJob {
    /// Adds an archive. A later archive of the same name replaces the earlier one (its file is written over).
    fn push(&mut self, a: UeArchive) -> usize {
        if let Some(k) = self.archives.iter().position(|x| x.name == a.name) {
            self.recs.retain(|r| r.0 != k);
            self.archives[k].bytes.clear();
        }
        self.archives.push(a);
        self.archives.len() - 1
    }
}

/// `job`: the shader libraries of the IoStore containers in `files`, then the shader code archives of its paks whose
/// shaders are not inline. `files` holds the paks, the IoStore TOCs and partitions and the manifests at the top
/// (path relative to the RomFS root, the file, in path order). An encrypted pak or container is opened with the key
/// found in main (`main`: the NSO file). The IoStore libraries need the game's decoder ([`crate::oodle`]).
pub fn ue_job(files: &[(String, Arc<dyn Src>)], main: &[u8]) -> R<UeJob> {
    let paks: Vec<&(String, Arc<dyn Src>)> =
        files.iter().filter(|f| f.0.ends_with(".pak")).collect();
    let mut key: Option<[u8; 32]> = None;
    let mut job = UeJob::default();
    let libs = super::uelib::io_libraries(files, &mut || {
        for (rel, src) in &paks {
            if let Ok((true, ioff, _, _, _)) = footer(src.as_ref(), rel) {
                key = Some(main_key(main, src.as_ref(), rel, ioff)?);
                return Ok(key);
            }
        }
        Ok(None)
    })?;
    for l in libs {
        job.push(UeArchive {
            name: l.name,
            pak: l.rel,
            path: l.path,
            shaders: l.shaders,
            compressed: 0,
            compression: "IoStore, decoded",
            bytes: l.bytes,
        });
    }
    for (rel, src) in paks {
        let Ok((enc, ioff, _, _, _)) = footer(src.as_ref(), rel) else {
            continue;
        };
        let beside = || {
            inline_in_containers(
                files
                    .iter()
                    .filter(|x| x.0.ends_with(".utoc") && dirname(&x.0) == dirname(rel))
                    .map(|x| (x.0.as_str(), x.1.as_ref())),
            )
        };
        if inline_shaders(src.as_ref(), rel, &beside) {
            continue;
        }
        if enc && key.is_none() {
            key = Some(main_key(main, src.as_ref(), rel, ioff)?);
        }
        let p = Pak::open_keyed(src.clone(), rel, key.as_ref())?;
        let mut paths: Vec<&String> = p
            .names
            .iter()
            .filter(|x| x.ends_with(".ushaderbytecode"))
            .collect();
        paths.sort();
        for path in paths {
            let mut b = p.read(path)?;
            let what = format!("{rel}: {path}");
            let (sh, _, code) = tables(&b, &what)?;
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let mut comp: Vec<usize> = (0..sh.len()).filter(|&i| sh[i].1 != sh[i].2).collect();
            let mut how = if comp.is_empty() { "" } else { "Oodle" };
            if !comp.is_empty()
                && let Some(raws) = lz4_shaders(&b, &sh, code)?
            {
                b = decoded_archive(&b, &raws)?;
                comp.clear();
                how = "LZ4";
            }
            let mut recs = Vec::with_capacity(comp.len());
            for &i in &comp {
                let c = stored(&b, code, &sh[i], &what)?;
                recs.push((i, c.to_vec(), sh[i].2));
            }
            let k = job.push(UeArchive {
                name,
                pak: rel.clone(),
                path: path.clone(),
                shaders: sh.len(),
                compressed: comp.len(),
                compression: how,
                bytes: b,
            });
            job.recs
                .extend(recs.into_iter().map(|(i, c, raw)| (k, i, c, raw)));
        }
    }
    Ok(job)
}

impl UeJob {
    /// The records the job holds.
    pub fn records(&self) -> usize {
        self.recs.len()
    }

    /// The job file: `DMOJ`, u32 count, then per record u32 compressed length, u32 raw length and the bytes.
    pub fn job_file(&self) -> Vec<u8> {
        let mut o = JOB_MAGIC.to_le_bytes().to_vec();
        o.extend((self.recs.len() as u32).to_le_bytes());
        for (_, _, c, raw) in &self.recs {
            o.extend((c.len() as u32).to_le_bytes());
            o.extend(raw.to_le_bytes());
            o.extend_from_slice(c);
        }
        o
    }

    /// `finish`: the archives with every shader decoded (file name, bytes), from the host's output of the job (`DMOR`,
    /// u32 count, then per record u32 decoded length, 0 for a failure, and the bytes, or `None` when the job is empty).
    pub fn finish(&self, out: Option<&[u8]>) -> R<Vec<(String, Vec<u8>)>> {
        let empty = [OUT_MAGIC.to_le_bytes(), [0; 4]].concat();
        let b = out.unwrap_or(&empty);
        if u32le(b, 0).ok() != Some(OUT_MAGIC as usize) || u32le(b, 4).ok() != Some(self.recs.len())
        {
            return ve("the job output does not match the job");
        }
        let mut o = 8usize;
        let mut dec: std::collections::HashMap<(usize, usize), &[u8]> = Default::default();
        for (k, i, _, _) in &self.recs {
            let n = u32le(b, o)?;
            o += 4;
            if n == 0 {
                return ve(format!(
                    "{}: shader {i} was not decoded",
                    self.archives[*k].name
                ));
            }
            let Some(d) = b.get(o..o + n) else {
                return ve("the job output ends early");
            };
            dec.insert((*k, *i), d);
            o += n;
        }
        let mut res = Vec::new();
        for (k, a) in self.archives.iter().enumerate() {
            if a.bytes.is_empty() {
                continue;
            }
            let (sh, _, code) = tables(&a.bytes, &a.name)?;
            let raws = sh
                .iter()
                .enumerate()
                .map(|(i, s)| match dec.get(&(k, i)) {
                    Some(d) => Ok(d.to_vec()),
                    None => stored(&a.bytes, code, s, &a.name).map(<[u8]>::to_vec),
                })
                .collect::<R<Vec<_>>>()?;
            res.push((a.name.clone(), decoded_archive(&a.bytes, &raws)?));
        }
        Ok(res)
    }
}

#[cfg(test)]
mod tests;
