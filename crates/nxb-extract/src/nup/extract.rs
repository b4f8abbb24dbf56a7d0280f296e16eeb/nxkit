//! The output half: a version's ExeFS, control data and images (with every DLC image) in one pass.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{BASE, NO_DIR, Nup, Package, T_ICON, T_LICONS, T_NACP, bad, program_id, scan, u32le};
use crate::error::{ExtractError, Result};
use crate::image::ImagePlan;
use crate::romfs::RomEntry;
use crate::storage::Storage;
use crate::write::{ImageJob, ImageResult, write_images_counted};

/// Files of a container's RomFS folder behind virtual offsets: the image writer reads whole files at those offsets.
struct TreeStorage {
    nup: Arc<Nup>,
    /// virtual offset -> file index
    at: BTreeMap<u64, usize>,
    size: u64,
}

impl Storage for TreeStorage {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let i = *self
            .at
            .get(&offset)
            .ok_or_else(|| ExtractError::format("NUP image read not at a file start"))?;
        let data = self.nup.read(i)?;
        if data.len() != buf.len() {
            return Err(ExtractError::format(
                "NUP image read of another size than the file",
            ));
        }
        buf.copy_from_slice(&data);
        Ok(())
    }
}

/// The RomFS folder `d` as the image writer's input: its entries (paths, sizes, virtual offsets) and storage.
fn tree(nup: &Arc<Nup>, d: u32) -> Result<(Vec<RomEntry>, Arc<dyn Storage>)> {
    let mut out = Vec::new();
    let mut at = BTreeMap::new();
    let mut off = 0u64;
    let mut stack = vec![(d, Vec::new(), 0usize)];
    let mut seen = std::collections::HashSet::new();
    while let Some((dir, path, depth)) = stack.pop() {
        if depth > 64 || !seen.insert(dir) {
            return Err(bad(&nup.path, "folder loop or nesting too deep"));
        }
        for (name, isdir, i) in nup.children(dir) {
            let mut p = path.clone();
            p.push(b'/');
            p.extend_from_slice(&name);
            if isdir {
                out.push(RomEntry {
                    path: p.clone(),
                    dir: true,
                    size: 0,
                    offset: 0,
                });
                stack.push((i as u32, p, depth + 1));
            } else {
                let size = nup.files[i].size;
                if size > 0 {
                    at.insert(off, i);
                }
                out.push(RomEntry {
                    path: p,
                    dir: false,
                    size,
                    offset: off,
                });
                off += size.max(1);
            }
        }
    }
    let st: Arc<dyn Storage> = Arc::new(TreeStorage {
        nup: nup.clone(),
        at,
        size: off,
    });
    Ok((out, st))
}

/// What to extract from a container.
#[derive(Clone, Debug)]
pub struct NupRequest {
    /// The `.nup` file.
    pub nup: PathBuf,
    /// The version to extract (none: the newest update, else the base game).
    pub version: Option<u32>,
    /// The build folder: `extract/v<version>/` and `aoc/` go there (empty: `<folder of the .nup>/<title ID>_build`).
    pub work: PathBuf,
    /// Image writer threads and memory.
    pub threads: usize,
    /// Bytes of file data in flight at most.
    pub mem_limit: u64,
    /// Compute the images' digests without writing them (`RomFS.img` and DLC images are not created).
    pub discard_images: bool,
    /// Optional TSV of the images' digests (whole image and per file).
    pub digests: Option<PathBuf>,
    /// Only the code modules and the control data: no image is read or written.
    pub skip_images: bool,
    /// Where to count the image bytes written, for a progress display.
    pub progress: Option<Arc<crate::run::Counter>>,
}

fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data).map_err(|e| ExtractError::io(format!("write {}", path.display()), e))
}

/// The ExeFS of package `k` into `dst` (module names checked, names starting with `.` left out).
pub(crate) fn exefs(n: &Nup, k: &Package, dst: &Path) -> Result<usize> {
    let mut files = Vec::new();
    for (name, isdir, i) in n.children(k.exefs) {
        if isdir {
            continue;
        }
        let s = String::from_utf8(name.clone())
            .map_err(|_| bad(&n.path, format!("invalid ExeFS name {name:?}")))?;
        if s.is_empty() || s == "." || s == ".." || s.contains('\0') || s.contains('/') {
            return Err(bad(&n.path, format!("invalid ExeFS name {s:?}")));
        }
        if !s.starts_with('.') {
            files.push((s, i));
        }
    }
    let _ = std::fs::remove_dir_all(dst);
    std::fs::create_dir_all(dst)
        .map_err(|e| ExtractError::io(format!("create {}", dst.display()), e))?;
    for (s, i) in &files {
        write_file(&dst.join(s), &n.read(*i)?)?;
    }
    Ok(files.len())
}

/// `localized`: the localized icons record -> [(lowercase tag, bytes)] in record order, first of each tag kept.
fn localized(payload: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut p = 0usize;
    while p < payload.len() {
        let ln = payload[p] as usize;
        let tag =
            String::from_utf8_lossy(payload.get(p + 1..p + 1 + ln).unwrap_or(&[])).to_lowercase();
        p += 1 + ln;
        if p + 4 > payload.len() {
            break;
        }
        let dn = u32le(payload, p) as usize;
        p += 4;
        let v = payload
            .get(p..p.saturating_add(dn).min(payload.len()))
            .unwrap_or(&[])
            .to_vec();
        if !out.iter().any(|(t, _)| *t == tag) {
            out.push((tag, v));
        }
        p = p.saturating_add(dn);
    }
    out
}

/// `control`: `control.nacp` and `icon.jpg` from the package's meta block (what it lacks from `fallback`).
pub(crate) fn control(n: &Nup, k: &Package, fallback: Option<&Package>, out: &Path) -> Result<()> {
    let mut meta = match fallback {
        Some(f) => n.meta(f)?,
        None => BTreeMap::new(),
    };
    meta.extend(n.meta(k)?);
    let icons = localized(meta.get(&T_LICONS).map_or(&[][..], |v| &v[..]));
    let find = |t: &str| icons.iter().find(|(x, _)| x == t).map(|x| x.1.clone());
    let icon = meta
        .get(&T_ICON)
        .filter(|v| !v.is_empty())
        .cloned()
        .or_else(|| find("en-us").filter(|v| !v.is_empty()))
        .or_else(|| find("en").filter(|v| !v.is_empty()))
        .or_else(|| icons.first().map(|x| x.1.clone()));
    for (name, data) in [
        ("control.nacp", meta.get(&T_NACP).cloned()),
        ("icon.jpg", icon),
    ] {
        match data.filter(|d| !d.is_empty()) {
            Some(d) => write_file(&out.join(name), &d)?,
            None => {
                let _ = std::fs::remove_file(out.join(name));
            }
        }
    }
    Ok(())
}

/// The version to take (`version`, else the newest update, else the base game), its package and its RomFS folder
/// (the package's own, else the base game's), with the engine's lines about the choice. The package must have
/// `exefs/main` and `exefs/main.npdm` of the container's program ID.
pub(crate) fn pick(n: &Nup, s: &super::Scan, version: Option<u32>) -> Result<(u32, Package, u32, Vec<String>)> {
    let file = n
        .path
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ver = version.unwrap_or_else(|| s.updates.keys().next_back().copied().unwrap_or(0));
    if ver != 0 && !s.updates.contains_key(&ver) {
        let have: Vec<String> = s.updates.keys().map(u32::to_string).collect();
        return Err(ExtractError::format(format!(
            "no update {ver} in {file} (have: {})",
            if have.is_empty() {
                "none".into()
            } else {
                have.join(", ")
            }
        )));
    }
    let ki = if ver == 0 {
        s.base
    } else {
        s.updates.get(&ver).copied()
    };
    let ki = ki.ok_or_else(|| {
        ExtractError::format(format!(
            "{file}: no base game (version 0); choose a standalone update version"
        ))
    })?;
    let k = n.packages[ki].clone();
    let mut lines = Vec::new();
    if s.base.is_none() {
        lines.push(
            "no base game in this NUP: its update is taken as the whole game (if the game then stops on missing files, the update holds only what changed: pack a NUP with the base game)".to_string(),
        );
    }
    lines.push(format!("{:016X} version {ver}: {}", s.app, k.name));
    // pack_version's checks
    let main_npdm = if k.exefs == NO_DIR {
        None
    } else {
        n.child(k.exefs, b"main.npdm")
    };
    let (Some(npdm), true) = (
        main_npdm,
        k.exefs != NO_DIR && n.child(k.exefs, b"main").is_some(),
    ) else {
        return Err(ExtractError::format(format!(
            "{}: no exefs/main and exefs/main.npdm",
            k.name
        )));
    };
    let pid = program_id(&n.read(npdm)?, &format!("{} exefs/main.npdm", k.name))?;
    if pid != s.app {
        return Err(ExtractError::format(format!(
            "{}: program ID {pid:016X}, the base is {:016X}",
            k.name, s.app
        )));
    }
    let base_romfs = s.base.map(|b| n.packages[b].romfs).unwrap_or(NO_DIR);
    let d = if k.romfs != NO_DIR {
        k.romfs
    } else {
        base_romfs
    };
    if d == NO_DIR {
        return Err(ExtractError::format(format!(
            "{}: no romfs/ in this package or the base game",
            k.name
        )));
    }
    Ok((ver, k, d, lines))
}

/// Extracts one version (and every DLC) of a container. Prints the engine's lines (`<title> version <v>: <package>`,
/// `done: <out>`), returns the image results.
pub fn extract_nup(r: &NupRequest) -> Result<Vec<ImageResult>> {
    let n = Arc::new(Nup::open(&r.nup)?);
    let s = scan(&n)?;
    let (ver, k, d, lines) = pick(&n, &s, r.version)?;
    let mut stdout = std::io::stdout().lock();
    for l in &lines {
        let _ = writeln!(stdout, "{l}");
    }
    let work = if r.work.as_os_str().is_empty() {
        r.nup
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!("{:016X}_build", s.app))
    } else {
        r.work.clone()
    };
    let out = work.join("extract").join(format!("v{ver}"));
    std::fs::create_dir_all(&out)
        .map_err(|e| ExtractError::io(format!("create {}", out.display()), e))?;
    exefs(&n, &k, &out.join("ExeFS"))?;
    let app_pkg = n
        .packages
        .iter()
        .find(|p| p.kind == BASE)
        .unwrap_or(&n.packages[0])
        .clone();
    control(
        &n,
        &k,
        if s.base.is_none() {
            Some(&app_pkg)
        } else {
            None
        },
        &out,
    )?;
    if r.skip_images {
        let _ = writeln!(
            stdout,
            "done: {} (code modules and control data only)",
            out.display()
        );
        return Ok(Vec::new());
    }
    // the version's image and every DLC image in one pass
    let mut jobs = Vec::new();
    let mut add = |d: u32, dst: PathBuf| -> Result<()> {
        let (ents, src) = tree(&n, d)?;
        let plan = ImagePlan::plan(&ents)?;
        let keys = (0..plan.entries.len() as u64).collect();
        jobs.push(ImageJob {
            dst,
            plan,
            src,
            keys,
            discard: r.discard_images,
        });
        Ok(())
    };
    add(d, out.join("RomFS.img"))?;
    let aoc = work.join("aoc");
    if !s.dlc.is_empty() {
        std::fs::create_dir_all(&aoc)
            .map_err(|e| ExtractError::io(format!("create {}", aoc.display()), e))?;
    }
    for (tid, pi) in &s.dlc {
        add(
            n.packages[*pi].romfs,
            aoc.join(format!("{}.img", tid - s.app - 0x1000)),
        )?;
    }
    let (results, closing) =
        write_images_counted(jobs, r.threads, r.mem_limit, r.progress.as_deref())?;
    closing.wait();
    if let Some(pth) = &r.digests {
        let mut t = String::from("# image\tpath\toffset\tsize\tsha256\n");
        for res in &results {
            let img = res.dst.display().to_string();
            t.push_str(&format!(
                "{img}\t\t0\t{}\t{}\tsha1={}\n",
                res.size,
                res.digest.file.sha256.to_hex(),
                res.digest.sha1.to_hex()
            ));
            for (path, off, size, dg) in &res.files_digests {
                t.push_str(&format!(
                    "{img}\t{}\t{off}\t{size}\t{}\n",
                    String::from_utf8_lossy(path),
                    dg.to_hex()
                ));
            }
        }
        write_file(pth, t.as_bytes())?;
    }
    let _ = writeln!(
        stdout,
        "done: {}{}",
        out.display(),
        if s.dlc.is_empty() {
            String::new()
        } else {
            format!(", {} DLC in {}", s.dlc.len(), aoc.display())
        }
    );
    let _ = n.toc_sha;
    Ok(results)
}

/// Whether a file starts like a NUP (`NUP`).
pub fn is_nup(path: &Path) -> bool {
    std::fs::File::open(path).ok().and_then(|mut f| {
        let mut b = [0u8; 3];
        std::io::Read::read_exact(&mut f, &mut b)
            .ok()
            .map(|_| &b == b"NUP")
    }) == Some(true)
}
