//! The NRO modules a game registers (`nro_collect.py`): every SHA-256 the `.nrr` files of the RomFS list, found as
//! loose files, NERD's `.nrolz` files, among the current files of a `data.arc` (Smash ARC format), or by a scan of the
//! other files for stored NROs and zstd frames that inflate to one. Writes `nro<i>.nro`, `nro.tsv`, and with
//! `missing_ok` the hashes no file holds into `missing.tsv` (the build captures them from a boot).

mod image;
mod sdk;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use image::Image;
use nxb_model::sha256;
use nxb_split::SplitError;
use nxb_split::jobs::smash;
use nxb_split::src::Src;

/// Why a collection did not happen.
pub enum Fail {
    /// The script does it (exit code 4).
    Unsupported(String),
    /// The script's `[RED]` or exit 1 case.
    Error(String),
}

fn un<T: ToString>(e: T) -> Fail {
    Fail::Unsupported(e.to_string())
}

/// What a collection found.
pub struct Summary {
    /// NROs written.
    pub written: usize,
    /// NROs of online SDKs, listed but not written.
    pub refused: usize,
    /// Bytes of every registered NRO found.
    pub bytes: usize,
    /// Registered NROs no file holds (listed in `missing.tsv`).
    pub missing: usize,
}

fn nro_size(h: &[u8]) -> u64 {
    if h.len() < 0x40 || &h[0x10..0x14] != b"NRO0" {
        return 0;
    }
    let n = u64::from(u32::from_le_bytes([h[0x18], h[0x19], h[0x1a], h[0x1b]]));
    if n > 0x40 && n < 1 << 30 { n } else { 0 }
}

/// `smash_arc.read`.
fn read(s: &dyn Src, e: &smash::Entry) -> Result<Vec<u8>, Fail> {
    let d = s.read_up(e.1, e.2 as usize).map_err(un)?;
    if d.get(..4) == Some(&[0x28, 0xb5, 0x2f, 0xfd][..]) {
        nxb_split::zstd::decompress(&d).map_err(un)
    } else {
        Ok(d[..d.len().min(e.3 as usize)].to_vec())
    }
}

/// Bytes of one scan job (`nro_collect.py`: at least 64 MiB, at most 48 jobs per file).
fn scan_step(size: u64) -> u64 {
    (64u64 << 20).max(size.div_ceil(48))
}

/// Every offset of `pat` in `[a, b)` of `s`, read in windows.
fn find_all(s: &dyn Src, pat: &[u8], a: u64, b: u64) -> Result<Vec<u64>, Fail> {
    const WIN: u64 = 16 << 20;
    let (mut out, mut at) = (Vec::new(), a);
    let end = b.min(s.len());
    while at < end {
        let n = (end - at).min(WIN) + pat.len() as u64 - 1;
        let n = n.min(s.len() - at);
        let buf = s.read_up(at, n as usize).map_err(un)?;
        let mut i = 0;
        while let Some(k) = buf[i..].windows(pat.len()).position(|w| w == pat) {
            let p = at + (i + k) as u64;
            if p >= end {
                break;
            }
            out.push(p);
            i += k + 1;
        }
        at += WIN;
    }
    Ok(out)
}

/// `scan`: NROs in `[a, b)` of one file, stored ones (`NRO0` at +0x10) and zstd frames whose first block inflates to
/// an NRO header, the wanted ones only.
fn scan(s: &dyn Src, a: u64, b: u64, want: &HashMap<[u8; 32], usize>) -> Result<Vec<Hit>, Fail> {
    let mut out = Vec::new();
    let len = s.len();
    for p in find_all(s, b"NRO0", a, b)? {
        let Some(st) = p.checked_sub(0x10) else {
            continue;
        };
        let head = s.read_up(st, 0x40.min((len - st) as usize)).map_err(un)?;
        let n = nro_size(&head);
        if n > 0 && st + n <= len {
            let body = s.read_up(st, n as usize).map_err(un)?;
            let h = sha256(&body).0;
            if want.contains_key(&h) {
                out.push((h, st, "stored", body));
            }
        }
    }
    for p in find_all(s, &ZMAGIC, a, b)? {
        let probe = s.read_up(p, 0x48000.min((len - p) as usize)).map_err(un)?;
        let Ok(head) = nxb_split::zstd::decompress_head(&probe, 0x40) else {
            continue;
        };
        if nro_size(&head) == 0 {
            continue;
        }
        let mut at = p;
        let mut input = |buf: &mut [u8]| -> Result<(), SplitError> {
            s.read_at(at, buf)?;
            at += buf.len() as u64;
            Ok(())
        };
        let mut body = Vec::new();
        let r = nxb_split::zstd::decompress_first(
            &mut input,
            (len - p).min(1 << 30),
            1 << 30,
            &mut |d| {
                body.extend_from_slice(d);
                Ok(())
            },
        );
        if r.is_err() {
            continue;
        }
        let h = sha256(&body).0;
        if want.contains_key(&h) {
            out.push((h, p, "zstd", body));
        }
    }
    Ok(out)
}

/// A scan's find: hash, offset, how it is stored, bytes.
type Hit = ([u8; 32], u64, &'static str, Vec<u8>);

const ZMAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// The NRR hashes in the order `nro_collect.py` meets them (`nrr_hashes`): first NRR naming a hash wins.
fn nrr_hashes(img: &Image, only: Option<&str>) -> Result<Vec<([u8; 32], String)>, Fail> {
    let mut want: Vec<([u8; 32], String)> = Vec::new();
    let mut seen: HashMap<[u8; 32], usize> = HashMap::new();
    for it in &img.files {
        let name = it.rel.rsplit('/').next().unwrap_or("");
        if !name.ends_with(".nrr") || only.is_some_and(|o| o != name) {
            continue;
        }
        let d = img.src(it).all().map_err(un)?;
        if d.get(..4) != Some(b"NRR0") {
            continue;
        }
        let rd = |o: usize| {
            d.get(o..o + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        // version 1, the Switch 2's (Mario Tennis Fever), has a header 0x180 bytes longer
        let at = if d.get(5) == Some(&1) { 0x4c0 } else { 0x340 };
        let (Some(off), Some(cnt)) = (rd(at), rd(at + 4)) else {
            return Err(Fail::Error(format!("{}: truncated NRR", it.rel)));
        };
        for i in 0..cnt as usize {
            let a = off as usize + 32 * i;
            let h: [u8; 32] = d
                .get(a..a + 32)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(|| Fail::Error(format!("{}: truncated NRR hash list", it.rel)))?;
            if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(h) {
                e.insert(want.len());
                want.push((h, name.to_string()));
            }
        }
    }
    Ok(want)
}

/// NERD's Hagi `.nrolz`: an LZ4 block behind its big-endian uncompressed size.
fn nrolz(raw: &[u8]) -> Vec<u8> {
    let Some(n) = raw.get(..4) else {
        return Vec::new();
    };
    let n = u32::from_be_bytes([n[0], n[1], n[2], n[3]]) as usize;
    match nxb_split::lz4::block(&raw[4..], n) {
        Ok(b) if b.len() == n => b,
        _ => Vec::new(),
    }
}

/// `nro.tsv` as `nro_collect.py` writes it: the header, the rows joined by newlines, and a final newline.
pub fn manifest_text(rows: &[String]) -> String {
    format!("# name\tsha256\tsize\tnrr\twhere\n{}\n", rows.join("\n"))
}

/// `missing.tsv`: one `<sha256>\t<nrr>` line per registered hash no file holds, in the order the NRRs list them.
pub fn missing_text(missing: &[(String, String)]) -> String {
    missing.iter().map(|(h, n)| format!("{h}\t{n}\n")).collect()
}

/// `nro_collect.py <RomFS image> <out dir> [<nrr name>] [--missing-ok]`, the output folder is written whole or not at
/// all. With `missing_ok` a registered hash no file holds goes to `missing.tsv` (the build captures it from a boot),
/// else it is an error.
pub fn collect(
    romfs: &Path,
    out: &Path,
    only: Option<&str>,
    missing_ok: bool,
) -> Result<Summary, Fail> {
    collect_counted(romfs, out, only, missing_ok, None)
}

/// Bytes of the image read so far and in all, for a progress display: one atomic add for each file or scan chunk. The
/// count runs through the image once, then through the files the scan reads (it starts again from 0 for those).
#[derive(Debug, Default)]
pub struct Scanned {
    /// Bytes done in the current pass.
    pub done: std::sync::atomic::AtomicU64,
    /// Bytes in all in the current pass, 0 until known.
    pub total: std::sync::atomic::AtomicU64,
}

/// [`collect`] that counts its passes in `count`.
pub fn collect_counted(
    romfs: &Path,
    out: &Path,
    only: Option<&str>,
    missing_ok: bool,
    count: Option<&Scanned>,
) -> Result<Summary, Fail> {
    use std::sync::atomic::Ordering::Relaxed;
    let img = Image::open(romfs).map_err(Fail::Error)?;
    if let Some(c) = count {
        c.total
            .store(img.files.iter().map(|f| f.size).sum(), Relaxed);
    }
    let mut want = nrr_hashes(&img, only)?;
    if want.is_empty() {
        return Err(Fail::Error(format!(
            "no NRR file with hashes in {}",
            romfs.display()
        )));
    }
    let mut found: BTreeMap<[u8; 32], (String, &str, Vec<u8>)> = BTreeMap::new();
    let mut big: Vec<usize> = Vec::new();
    let mut indexed = 0usize;
    let nro_ext = hash40(b"nro");
    let wanted = |want: &[([u8; 32], String)], h: &[u8; 32]| want.iter().any(|w| &w.0 == h);
    for (k, it) in img.files.iter().enumerate() {
        if let Some(c) = count {
            c.done.fetch_add(it.size, Relaxed);
        }
        let name = it.rel.rsplit('/').next().unwrap_or("");
        if name.ends_with(".nrr") || it.size < 0x40 {
            continue;
        }
        let src = img.src(it);
        let head = src.read_up(0, 0x40).map_err(un)?;
        if name.to_lowercase().ends_with(".nrolz") {
            let body = nrolz(&src.all().map_err(un)?);
            if body.len() >= 0x40 && nro_size(&body[..0x40]) == body.len() as u64 {
                let h = sha256(&body).0;
                if wanted(&want, &h) {
                    found.insert(h, (it.rel.clone(), "lz4", body));
                }
            }
            continue;
        }
        if nro_size(&head) == it.size {
            let body = src.all().map_err(un)?;
            let h = sha256(&body).0;
            if wanted(&want, &h) {
                found.insert(h, (it.rel.clone(), "loose", body));
            }
        } else if head.get(..8) == Some(&smash::MAGIC[..]) {
            indexed += 1;
            for e in smash::files(src.as_ref(), false)
                .map_err(un)?
                .unwrap_or_default()
            {
                if e.4 != nro_ext {
                    continue;
                }
                let body = read(src.as_ref(), &e)?;
                let h = sha256(&body).0;
                if wanted(&want, &h) {
                    found.insert(h, (format!("{}#{:010x}", it.rel, e.0), "index", body));
                } else {
                    println!(
                        "[RED] {}#{:010x}: an NRO that no NRR registers",
                        it.rel, e.0
                    );
                }
            }
        } else if head.get(..4) != Some(b"CRA.") {
            // Camelot's archive is scrambled, so no scan can find anything in it
            big.push(k);
        }
    }
    if indexed > 0 {
        println!(
            "{} NROs among the current files of {indexed} indexed archive(s); {} registered hashes are superseded copies, left out",
            found.len(),
            want.len() - found.len()
        );
        want.retain(|w| found.contains_key(&w.0));
    }
    let left: HashMap<[u8; 32], usize> = want
        .iter()
        .enumerate()
        .filter(|(_, w)| !found.contains_key(&w.0))
        .map(|(i, w)| (w.0, i))
        .collect();
    if !left.is_empty() && !big.is_empty() {
        let gb: u64 = big.iter().map(|&k| img.files[k].size).sum();
        println!(
            "{} registered NROs, {} loose; scanning {} files ({:.1} GB) for the other {}",
            want.len(),
            found.len(),
            big.len(),
            gb as f64 / f64::from(1u32 << 30),
            left.len()
        );
        let mut jobs: Vec<(usize, u64, u64)> = Vec::new();
        for &k in &big {
            let sz = img.files[k].size;
            let step = scan_step(sz);
            let mut a = 0;
            while a < sz {
                jobs.push((k, a, sz.min(a + step + 0x40)));
                a += step;
            }
        }
        let threads = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(10);
        if let Some(c) = count {
            c.done.store(0, Relaxed);
            c.total
                .store(jobs.iter().map(|j| j.2 - j.1).sum(), Relaxed);
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: std::sync::Mutex<Vec<(usize, Vec<Hit>)>> = std::sync::Mutex::new(Vec::new());
        let first_err: std::sync::Mutex<Option<Fail>> = std::sync::Mutex::new(None);
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| {
                    loop {
                        let j = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(k, a, b)) = jobs.get(j) else { break };
                        let src = img.src(&img.files[k]);
                        let r = scan(src.as_ref(), a, b, &left);
                        if let Some(c) = count {
                            c.done.fetch_add(b - a, Relaxed);
                        }
                        match r {
                            Ok(r) => {
                                if let Ok(mut v) = results.lock() {
                                    v.push((j, r));
                                }
                            }
                            Err(e) => {
                                if let Ok(mut f) = first_err.lock() {
                                    f.get_or_insert(e);
                                }
                            }
                        }
                    }
                });
            }
        });
        if let Some(e) = first_err.into_inner().ok().flatten() {
            return Err(e);
        }
        let mut results = results.into_inner().unwrap_or_default();
        results.sort_by_key(|r| r.0);
        for (j, r) in results {
            let k = jobs[j].0;
            for (h, off, how, body) in r {
                found
                    .entry(h)
                    .or_insert_with(|| (format!("{}+{off:#x}", img.files[k].rel), how, body));
            }
        }
    }
    let tmp = out.with_extension("partial");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| Fail::Error(format!("{}: {e}", tmp.display())))?;
    let nrr_of = |h: &[u8; 32]| {
        want.iter()
            .find(|w| &w.0 == h)
            .map(|w| w.1.clone())
            .unwrap_or_default()
    };
    let (mut rows, mut i, mut refused, mut bytes) = (Vec::new(), 0, 0, 0);
    for (h, (wher, how, body)) in &found {
        bytes += body.len();
        let hex = nxb_model::Digest(*h).to_hex();
        let nrr = nrr_of(h);
        if let Some(what) = sdk::online(body) {
            rows.push(format!(
                "refused\t{hex}\t{}\t{nrr}\t{how} {wher} ({what})",
                body.len()
            ));
            refused += 1;
            continue;
        }
        let name = format!("nro{i}");
        i += 1;
        std::fs::write(tmp.join(format!("{name}.nro")), body)
            .map_err(|e| Fail::Error(format!("{name}.nro: {e}")))?;
        rows.push(format!(
            "{name}\t{hex}\t{}\t{nrr}\t{how} {wher}",
            body.len()
        ));
    }
    if refused > 0 {
        println!(
            "{refused} NRO(s) of an online SDK refused, not rewritten (listed as refused in nro.tsv)"
        );
    }
    std::fs::write(tmp.join("nro.tsv"), manifest_text(&rows))
        .map_err(|e| Fail::Error(format!("nro.tsv: {e}")))?;
    let miss: Vec<(String, String)> = want
        .iter()
        .filter(|w| !found.contains_key(&w.0))
        .map(|w| (nxb_model::Digest(w.0).to_hex(), w.1.clone()))
        .collect();
    println!(
        "{} of {} registered NROs found ({:.1} MB) -> {}",
        found.len(),
        want.len(),
        bytes as f64 / 1_048_576.0,
        out.display()
    );
    if !miss.is_empty() && missing_ok {
        std::fs::write(tmp.join("missing.tsv"), missing_text(&miss))
            .map_err(|e| Fail::Error(format!("missing.tsv: {e}")))?;
        println!(
            "{} registered NROs in no file, first {}: the game may decode them at run time",
            miss.len(),
            miss[0].0
        );
    } else if !miss.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(Fail::Error(format!(
            "{} registered NROs not found, first {}",
            miss.len(),
            miss[0].0
        )));
    }
    let _ = std::fs::remove_dir_all(out);
    std::fs::rename(&tmp, out).map_err(|e| Fail::Error(format!("{}: {e}", out.display())))?;
    Ok(Summary {
        written: i,
        refused,
        bytes,
        missing: miss.len(),
    })
}

/// `smash_arc.hash40`: CRC-32 and length.
fn hash40(s: &[u8]) -> u64 {
    u64::from(crc32(s)) | (s.len() as u64) << 32
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in d {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                c >> 1 ^ 0xedb8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use nxb_split::src::Mem;

    fn nro(n: usize, fill: u8) -> Vec<u8> {
        let mut b = vec![fill; n];
        b[0x10..0x14].copy_from_slice(b"NRO0");
        b[0x18..0x1c].copy_from_slice(&(n as u32).to_le_bytes());
        b
    }

    #[test]
    fn manifest_and_missing_are_written_as_the_script_writes_them() {
        assert_eq!(
            super::manifest_text(&[]),
            "# name\tsha256\tsize\tnrr\twhere\n\n"
        );
        assert_eq!(
            super::manifest_text(&["nro0\taa\t64\tx.nrr\tloose a".into()]),
            "# name\tsha256\tsize\tnrr\twhere\nnro0\taa\t64\tx.nrr\tloose a\n"
        );
        assert_eq!(
            super::missing_text(&[("bb".into(), "x.nrr".into()), ("aa".into(), "y.nrr".into())]),
            "bb\tx.nrr\naa\ty.nrr\n"
        );
    }

    #[test]
    fn scan_finds_stored_nros_and_only_wanted_ones() {
        let (a, b) = (nro(0x80, 1), nro(0x90, 2));
        let mut file = vec![0u8; 0x33];
        file.extend_from_slice(&a);
        file.extend_from_slice(&[9; 7]);
        file.extend_from_slice(&b);
        let want = [(nxb_model::sha256(&b).0, 0usize)].into_iter().collect();
        let got = super::scan(&Mem(&file), 0, file.len() as u64, &want).unwrap_or_default();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, 0x33 + 0x80 + 7);
        assert_eq!(got[0].2, "stored");
        assert_eq!(got[0].3, b);
    }

    #[test]
    fn nrolz_is_a_big_endian_size_and_one_lz4_block() {
        // literal-only LZ4 block: token 0xF0, extra length 0x40 - 15 = 49, then the bytes
        let body = nro(0x40, 3);
        let mut raw = 0x40u32.to_be_bytes().to_vec();
        raw.push(0xF0);
        raw.push(0x40 - 15);
        raw.extend_from_slice(&body);
        assert_eq!(super::nrolz(&raw), body);
        assert!(super::nrolz(&raw[..10]).is_empty());
    }

    #[test]
    fn scan_jobs_are_64_mib_or_a_48th() {
        assert_eq!(super::scan_step(1 << 20), 64 << 20);
        assert_eq!(super::scan_step(48 << 30), 1 << 30);
    }

    #[test]
    fn hash40_of_nro() {
        assert_eq!(
            super::hash40(b"nro"),
            u64::from(super::crc32(b"nro")) | 3 << 32
        );
        assert_eq!(super::crc32(b"123456789"), 0xcbf4_3926);
    }
}
