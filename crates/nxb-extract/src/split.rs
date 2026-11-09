//! The split stage over extracted versions: walks each version's `RomFS.img` as `shader_split.py` walks it through
//! `romfs.py` (sorted paths, versions in order, later versions prefixed with their directory name), scans `main` of each
//! version first, scans the files with `threads` workers, and keeps the results in the Python's order, so the outputs
//! (`nxb_split::records`) are byte-identical.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use nxb_split::SplitError;
use nxb_split::records::{FileResult, Outputs, build, uniq};
use nxb_split::shaders::{BahsVar, Profile, scan_blob, var_pairs};

use nxb_model::{Digest, sha256};

use crate::error::{ExtractError, Result};
use crate::split_out::write_outputs;

pub use crate::listing::{ImageFile, image_files};

/// Where the split writes.
#[derive(Clone, Debug)]
pub struct SplitOutputs {
    /// Package directory.
    pub pkg: PathBuf,
    /// `shader_map.tsv`.
    pub map: PathBuf,
    /// `shader_facts.tsv`.
    pub facts: PathBuf,
    /// `shader_pairs.tsv`.
    pub pairs: PathBuf,
    /// `pso_pairs.tsv`.
    pub pso_pairs: PathBuf,
}

/// The split over extracted version directories (`.../v0` with `RomFS.img` and `ExeFS/main`), base first.
#[derive(Clone, Debug)]
pub struct SplitRequest {
    /// Version directories.
    pub versions: Vec<PathBuf>,
    /// The profile's `game.tsv`.
    pub game_tsv: PathBuf,
    /// Outputs.
    pub outputs: SplitOutputs,
    /// Workers.
    pub threads: usize,
    /// Counts the scanned file bytes (`total` is set once the files are listed).
    pub progress: Option<std::sync::Arc<crate::run::Counter>>,
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)
            .map_err(|e| ExtractError::io(format!("create {}", d.display()), e))?;
    }
    std::fs::write(path, bytes)
        .map_err(|e| ExtractError::io(format!("write {}", path.display()), e))
}

pub(crate) fn split_err(e: SplitError) -> ExtractError {
    match e {
        SplitError::Unsupported(m) => ExtractError::Unsupported(m),
        other => ExtractError::format(other.to_string()),
    }
}

/// One version for [`Templates::merge`]: its tag (empty for the base), `ExeFS/main` and its files with digests.
pub struct VersionIn {
    /// Directory name of a later version (`v327680`), empty for the base.
    pub tag: String,
    /// The `main` NSO file.
    pub main: Vec<u8>,
    /// (path relative to the RomFS root, SHA-256 of its bytes).
    pub files: Vec<(String, Digest)>,
}

type Template = Arc<OnceLock<std::result::Result<Option<FileResult>, ()>>>;

/// Scan results by content: a file's bytes are scanned once whatever its path or version (its shaders are labelled
/// at merge time), so an update's unchanged files, and copies of one file, cost one scan. Exact: a later copy of
/// content adds no key and no pair the first copy did not.
pub struct Templates {
    pub(crate) prof: Profile,
    game_tsv: String,
    manual: Option<String>,
    pub(crate) by_digest: Mutex<HashMap<Digest, Template>>,
    errors: Mutex<HashMap<String, String>>,
    pub(crate) unsupported: Mutex<Option<String>>,
    pub(crate) notes: Mutex<Vec<String>>,
    scanned: AtomicUsize,
    pub(crate) dicts: std::sync::RwLock<nxb_split::zstd::Dicts>,
    /// Files the planners of the archive jobs read: label, digest, bytes when the images are not kept.
    pub(crate) deferred: Mutex<Vec<Deferred>>,
    pub(crate) keep: bool,
    /// Unity's common strings from the first version's main (`unityfs.common_strings`), `None` when it has none.
    pub(crate) common: Option<std::sync::Arc<std::collections::HashMap<u32, String>>>,
    /// Unity files whose type trees are in other bundles: label, digest, bytes when the images are not kept.
    pub(crate) unity_defer: Mutex<Vec<Deferred>>,
    /// The second pass over them, by digest.
    pub(crate) pass2: Mutex<HashMap<Digest, Outcome>>,
    pub(crate) held: AtomicUsize,
    pub(crate) by_label: Mutex<HashMap<String, Outcome>>,
    pub(crate) jobs: Mutex<(Vec<Outcome>, HashSet<String>)>,
}

/// A held-back file: label, digest, bytes when the images are not kept.
pub(crate) type Deferred = (String, Digest, Option<std::sync::Arc<Vec<u8>>>);

/// A job's or a label's result: shaders, or the text of the reason it is unreadable (empty: reported elsewhere).
pub(crate) type Outcome = std::result::Result<Option<FileResult>, String>;

/// Bytes of held-back archives `--discard-images` may keep in memory.
pub(crate) const HOLD_LIMIT: usize = 8 << 30;

fn relabel(r: &FileResult, label: &str) -> FileResult {
    FileResult {
        keys: r
            .keys
            .iter()
            .map(|(k, f)| {
                let mut f = f.clone();
                f.place = format!("{label}{}", f.place);
                (k.clone(), f)
            })
            .collect(),
        pairs: r.pairs.clone(),
        ..FileResult::default()
    }
}

impl Templates {
    /// Reads the profile.
    pub fn new(game_tsv: &Path) -> Result<Templates> {
        let text = std::fs::read_to_string(game_tsv)
            .map_err(|e| ExtractError::io(format!("read {}", game_tsv.display()), e))?;
        Ok(Templates {
            prof: Profile::from_game_tsv(&text),
            game_tsv: text,
            manual: game_tsv
                .parent()
                .and_then(|d| std::fs::read_to_string(d.join("shader_facts_manual.tsv")).ok()),
            by_digest: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            unsupported: Mutex::new(None),
            notes: Mutex::new(Vec::new()),
            scanned: AtomicUsize::new(0),
            dicts: std::sync::RwLock::new(nxb_split::zstd::Dicts::new()),
            deferred: Mutex::new(Vec::new()),
            keep: false,
            common: None,
            unity_defer: Mutex::new(Vec::new()),
            pass2: Mutex::new(HashMap::new()),
            held: AtomicUsize::new(0),
            by_label: Mutex::new(HashMap::new()),
            jobs: Mutex::new((Vec::new(), HashSet::new())),
        })
    }

    /// Adds the zstd dictionaries of a RomFS's `Pack/ZsDic.pack.zs` (the file's bytes): a plain zstd file holding a
    /// SARC of dictionary files. Call before the scan starts, for every version and add-on RomFS (a later one
    /// replaces an earlier one with the same ID, as the Python's merged dictionary table does).
    pub fn add_dict_pack(&self, pack: &[u8]) -> Result<()> {
        let sarc = nxb_split::zstd::decompress(pack).map_err(split_err)?;
        let members =
            nxb_split::shaders::sarc_members(&sarc, "/Pack/ZsDic.pack.zs").map_err(split_err)?;
        let mut g = self
            .dicts
            .write()
            .map_err(|_| ExtractError::format("dictionary table poisoned"))?;
        for (_, m) in members {
            if m.get(..4) == Some(&[0x37, 0xa4, 0x30, 0xec]) {
                g.add(m).map_err(split_err)?;
            }
        }
        Ok(())
    }

    /// Scans one file (any thread). `label` is `/` + its path as the Python names it.
    pub fn scan(&self, label: &str, data: &[u8], digest: Digest) {
        if self.defer(label, data, digest) {
            return;
        }
        self.scan_with(label, &nxb_split::src::Mem(data), digest);
    }

    pub(crate) fn scan_with(&self, label: &str, src: &dyn nxb_split::src::Src, digest: Digest) {
        let slot = match self.by_digest.lock() {
            Ok(mut g) => g.entry(digest).or_default().clone(),
            Err(_) => return,
        };
        let Ok(dicts) = self.dicts.read() else { return };
        let r = slot.get_or_init(|| {
            self.scanned.fetch_add(1, Ordering::Relaxed);
            let mut notes = Vec::new();
            let env = nxb_split::scanfile::Env {
                dicts: &dicts,
                common: self.common.as_deref(),
                trees: None,
            };
            let r = nxb_split::scanfile::scan_file(&self.prof, src, "", label, &env, &mut notes);
            if let Ok(mut g) = self.notes.lock() {
                g.extend(notes.iter().map(|n| n.replace("{label}", label)));
            }
            match r {
                Ok(Some(r)) if r.defer => {
                    // a Unity file without type trees: scanned again once every bundle's trees are known
                    let held = if self.keep {
                        src.bytes().map(|b| std::sync::Arc::new(b.to_vec()))
                    } else {
                        None
                    };
                    if let Ok(mut g) = self.unity_defer.lock() {
                        g.push((label.to_string(), digest, held));
                    }
                    Ok(Some(r))
                }
                Ok(r) => Ok(r),
                Err(SplitError::Unsupported(m)) => {
                    if let Ok(mut g) = self.unsupported.lock() {
                        g.get_or_insert(format!("{label}{m}"));
                    }
                    Err(())
                }
                Err(_) => Err(()),
            }
        });
        if r.is_err() {
            // the message names this path: scan again with the real label
            let env = nxb_split::scanfile::Env {
                dicts: &dicts,
                common: self.common.as_deref(),
                trees: None,
            };
            if let Err(e) =
                nxb_split::scanfile::scan_file(&self.prof, src, label, label, &env, &mut Vec::new())
            {
                let why = e.to_string();
                let rel = label.trim_start_matches('/');
                let why = if why.trim_start_matches('/').starts_with(rel) {
                    why
                } else {
                    format!("{rel}: {why}")
                };
                if let Ok(mut g) = self.errors.lock() {
                    g.insert(label.to_string(), why);
                }
            }
        }
    }

    /// Distinct contents scanned.
    pub fn scanned(&self) -> usize {
        self.scanned.load(Ordering::Relaxed)
    }

    /// Orders the results as the Python does and builds and writes the outputs.
    pub fn merge(
        self,
        versions: &[VersionIn],
        req: &SplitOutputs,
    ) -> Result<(Outputs, Vec<String>)> {
        if let Some(m) = self.unsupported.into_inner().ok().flatten() {
            return Err(ExtractError::Unsupported(m));
        }
        let prof = &self.prof;
        if let Ok(mut n) = self.notes.lock() {
            n.sort();
            for line in n.iter() {
                println!("{line}");
            }
        }
        let mut results = Vec::new();
        for v in versions.iter() {
            let img = nxb_split::nso::image(&v.main).map_err(split_err)?;
            let label = if v.tag.is_empty() {
                "main".to_string()
            } else {
                format!("main@{}", v.tag)
            };
            let mut found = Vec::new();
            let mut var = BahsVar::new();
            scan_blob(&img, &label, &mut found, &mut var, 0).map_err(split_err)?;
            let more = nxb_split::misc::main_extras(&img, &label, &found).map_err(split_err)?;
            found.extend(more);
            let pairs = var_pairs(prof, &found, &var);
            results.push(FileResult {
                keys: uniq(prof, found, true).map_err(split_err)?,
                pairs,
                ..FileResult::default()
            });
        }
        let by = self.by_digest.into_inner().unwrap_or_default();
        let errors = self.errors.into_inner().unwrap_or_default();
        let mut seen: HashSet<Digest> = HashSet::new();
        let mut bad = Vec::new();
        let (mut again, mut again_seen): (Vec<(String, Digest)>, HashSet<Digest>) =
            (Vec::new(), HashSet::new());
        // the archive jobs come after main and before the files
        let (job_res, taken) = self.jobs.into_inner().unwrap_or_default();
        let mut by_label = self.by_label.into_inner().unwrap_or_default();
        for r in job_res {
            match r {
                Ok(Some(r)) => results.push(r),
                Ok(None) => {}
                Err(why) if !why.is_empty() => {
                    bad.push(format!("[RED] {why} (its shaders are left out)"))
                }
                Err(_) => {}
            }
        }
        for v in versions {
            let mut files: Vec<&(String, Digest)> = v.files.iter().collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            for (rel, d) in files {
                let label = if v.tag.is_empty() {
                    format!("/{rel}")
                } else {
                    format!("/{}/{rel}", v.tag)
                };
                if taken.contains(&label) {
                    continue;
                }
                if let Some(r) = by_label.remove(&label) {
                    match r {
                        Ok(Some(r)) => results.push(r),
                        Ok(None) => {}
                        Err(why) if !why.is_empty() => {
                            let r = rel.as_str();
                            let why = if why.trim_start_matches('/').starts_with(r) {
                                why
                            } else {
                                format!("{r}: {why}")
                            };
                            bad.push(format!("[RED] {why} (its shaders are left out)"));
                        }
                        Err(_) => {}
                    }
                    continue;
                }
                if let Some(why) = errors.get(&label) {
                    bad.push(format!("[RED] {why} (its shaders are left out)"));
                    continue;
                }
                let t = by
                    .get(d)
                    .and_then(|t| t.get())
                    .ok_or_else(|| ExtractError::format(format!("{label}: not scanned")))?;
                if let Ok(Some(r)) = t
                    && r.defer
                {
                    if again_seen.insert(*d) {
                        again.push((label.clone(), *d));
                    }
                    continue;
                }
                if let Ok(Some(r)) = t
                    && seen.insert(*d)
                {
                    results.push(relabel(r, &label));
                }
            }
        }
        // the Unity files that borrowed type trees come last, as in the reference's second pass
        let mut pass2 = self.pass2.into_inner().unwrap_or_default();
        for (label, d) in again {
            match pass2.remove(&d) {
                Some(Ok(Some(r))) => results.push(relabel(&r, &label)),
                Some(Err(why)) if !why.is_empty() => {
                    bad.push(format!("[RED] {why} (its shaders are left out)"))
                }
                _ => {}
            }
        }
        let out = build(prof, &results).map_err(split_err)?;
        write_outputs(req, &self.game_tsv, self.manual.as_deref(), &out, bad.len())?;
        Ok((out, bad))
    }
}

/// Runs the split on extracted version directories, returns the outputs and the `[RED]` lines of unreadable files.
pub fn split(req: &SplitRequest) -> Result<(Outputs, Vec<String>)> {
    let mut t = Templates::new(&req.game_tsv)?;
    let mut versions = Vec::new();
    let mut jobs: Vec<(usize, String, u64, u64)> = Vec::new();
    let mut images = Vec::new();
    for (i, v) in req.versions.iter().enumerate() {
        let tag = if i == 0 {
            String::new()
        } else {
            v.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mainp = v.join("ExeFS/main");
        let main = std::fs::read(&mainp)
            .map_err(|e| ExtractError::io(format!("read {}", mainp.display()), e))?;
        if i == 0 {
            t.set_common(&main);
        }
        let (f, files) = image_files(&v.join("RomFS.img"))?;
        for (p, off, size) in &files {
            let label = if tag.is_empty() {
                format!("/{p}")
            } else {
                format!("/{tag}/{p}")
            };
            jobs.push((images.len(), label, *off, *size));
        }
        if let Some((_, off, size)) = files.iter().find(|x| x.0 == "Pack/ZsDic.pack.zs") {
            let mut pack = vec![0u8; usize::try_from(*size).unwrap_or(0)];
            f.read_exact_at(&mut pack, *off)
                .map_err(|e| ExtractError::io("read Pack/ZsDic.pack.zs", e))?;
            t.add_dict_pack(&pack)?;
        }
        images.push(f);
        versions.push((tag, main, files));
    }
    let digests: Vec<Mutex<Option<Digest>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
    let scanned = req.progress.as_deref();
    if let Some(c) = scanned {
        c.total
            .store(jobs.iter().map(|j| j.3).sum(), Ordering::Relaxed);
    }
    let next = AtomicUsize::new(0);
    let fatal: Mutex<Option<ExtractError>> = Mutex::new(None);
    // NXB_SPLIT_LPT=1: largest first (results are merged in the Python's order by digest, not in completion order).
    // Measured on the reference title: warm cache 8.4 s against 9.6 s in input order, cold from the USB disk 31 s against 12 s
    // (all workers read big files at once), so input order stays the default.
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    if std::env::var_os("NXB_SPLIT_LPT").is_some() {
        order.sort_by_key(|&j| std::cmp::Reverse(jobs[j].3));
    }
    std::thread::scope(|s| {
        for _ in 0..req.threads.max(1) {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&j) = order.get(k) else {
                        break;
                    };
                    let (im, label, off, size) = &jobs[j];
                    // a file of an archive family is read by ranges later: its digest, no scan
                    let mut head = vec![0u8; (*size as usize).min(16)];
                    if let Err(e) = images[*im].read_exact_at(&mut head, *off) {
                        if let Ok(mut g) = fatal.lock() {
                            *g = Some(ExtractError::io(format!("read {label}"), e));
                        }
                        break;
                    }
                    if nxb_split::jobs::deferred(label.trim_start_matches('/'), &head) {
                        match crate::post::stream_digest(&images[*im], *off, *size) {
                            Ok(d) => {
                                t.defer(label, &head, d);
                                if let Ok(mut g) = digests[j].lock() {
                                    *g = Some(d);
                                }
                            }
                            Err(e) => {
                                if let Ok(mut g) = fatal.lock() {
                                    *g = Some(e);
                                }
                                break;
                            }
                        }
                        if let Some(c) = scanned {
                            c.done.fetch_add(*size, Ordering::Relaxed);
                        }
                        continue;
                    }
                    let mut data = vec![0u8; *size as usize];
                    if let Err(e) = images[*im].read_exact_at(&mut data, *off) {
                        if let Ok(mut g) = fatal.lock() {
                            *g = Some(ExtractError::io(format!("read {label}"), e));
                        }
                        break;
                    }
                    let d = sha256(&data);
                    t.scan(label, &data, d);
                    if let Ok(mut g) = digests[j].lock() {
                        *g = Some(d);
                    }
                    if let Some(c) = scanned {
                        c.done.fetch_add(*size, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    if let Some(e) = fatal.into_inner().ok().flatten() {
        return Err(e);
    }
    let mut ds = digests.into_iter().map(|m| m.into_inner().ok().flatten());
    let mut vin = Vec::new();
    let mut post = Vec::new();
    let arcs: Vec<std::sync::Arc<File>> = images.into_iter().map(std::sync::Arc::new).collect();
    for (vi, (tag, main, files)) in versions.into_iter().enumerate() {
        let mut fl = Vec::new();
        let mut sorted = Vec::new();
        for (p, off, size) in files {
            let d = ds
                .next()
                .flatten()
                .ok_or_else(|| ExtractError::format("a split job did not finish"))?;
            sorted.push((p.clone(), off, size, d));
            fl.push((p, d));
        }
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (p, off, size, d) in sorted {
            let label = if tag.is_empty() {
                format!("/{p}")
            } else {
                format!("/{tag}/{p}")
            };
            let src: std::sync::Arc<dyn nxb_split::src::Src> =
                std::sync::Arc::new(crate::post::ImgSrc {
                    file: arcs[vi].clone(),
                    off,
                    size,
                });
            post.push(crate::post::PostFile {
                label,
                src,
                digest: d,
            });
        }
        vin.push(VersionIn {
            tag,
            main,
            files: fl,
        });
    }
    t.post(post, req.threads)?;
    eprintln!(
        "nxb-split: {} distinct files scanned of {}",
        t.scanned(),
        jobs.len()
    );
    t.merge(&vin, &req.outputs)
}
