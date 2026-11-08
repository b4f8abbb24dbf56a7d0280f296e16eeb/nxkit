//! The loose output of a container: a version's RomFS as a folder of files (no `RomFS.img`), each DLC's RomFS as a
//! folder, and the shader split in the same pass.
//!
//! One pass over the container's file data: workers take the files in the order of their data (near-sequential
//! reads), decode a zstd frame or copy a raw file, check it against its size and SHA-256, write it, and hand the bytes
//! to the split while they are in memory. The split keys its results by the SHA-256 the table of contents records,
//! so nothing is hashed twice. A raw file can not be used in place: APFS clones whole files only (`clonefile`), and
//! a container file is a range of the `.nup`, so its bytes are written once.

use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use nxb_model::{Digest, Sha256};
use nxb_split::SplitError;
use nxb_split::src::Src;

use super::extract::{control, exefs, pick};
use super::{Nup, bad, scan};
use crate::error::{ExtractError, Result};
use crate::post::PostFile;
use crate::split::{SplitOutputs, Templates, VersionIn};
use crate::storage::Storage;

/// A raw file larger than this is copied in chunks and scanned from the written file.
const WHOLE: u64 = 64 << 20;
/// The chunk of such a copy.
const CHUNK: usize = 8 << 20;

/// What to unpack from a container.
#[derive(Clone, Debug)]
pub struct UnpackRequest {
    /// The `.nup` file.
    pub nup: PathBuf,
    /// The version (none: the newest update, else the base game).
    pub version: Option<u32>,
    /// The version folder: `ExeFS/`, `control.nacp` and `icon.jpg` go there.
    pub out: PathBuf,
    /// The RomFS folder to write (replaced).
    pub romfs: PathBuf,
    /// The folder of the DLC RomFS folders, `<aoc>/<index>/` (index = DLC title ID - base title ID - 0x1000).
    pub aoc: PathBuf,
    /// Workers.
    pub threads: usize,
    /// Bytes of file data in memory at most.
    pub mem_limit: u64,
    /// Check every file against its SHA-256 (off: only the sizes and the table of contents' SHA-256 are checked).
    pub verify: bool,
    /// The shader split in the same pass: the profile's `game.tsv` and the outputs.
    pub split: Option<(PathBuf, SplitOutputs)>,
    /// Counts the file bytes written.
    pub progress: Option<Arc<crate::run::Counter>>,
}

/// What [`unpack_nup`] wrote.
#[derive(Debug, Default)]
pub struct Unpacked {
    /// The version unpacked.
    pub version: u32,
    /// Files and bytes of the version's RomFS.
    pub files: usize,
    /// Bytes of the version's RomFS.
    pub bytes: u64,
    /// The DLC: (title ID, index).
    pub dlc: Vec<(u64, u32)>,
    /// Lines for the console: the container's notes and the split's `[RED]` lines.
    pub notes: Vec<String>,
    /// Why the split needs the game's own decoder, when it does (the split wrote no outputs then).
    pub split_unsupported: Option<String>,
}

/// One file to write: its container index, the path below its root, and whether the split scans it.
struct Job {
    file: usize,
    path: PathBuf,
    rel: String,
    split: bool,
}

/// Bytes in flight, at most `cap` (one file larger than `cap` still goes, alone).
struct Budget {
    used: Mutex<u64>,
    cv: Condvar,
    cap: u64,
}

impl Budget {
    fn take(&self, n: u64) -> u64 {
        let n = n.min(self.cap);
        let mut g = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *g > 0 && *g + n > self.cap {
            g = self
                .cv
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *g += n;
        n
    }
    fn give(&self, n: u64) {
        let mut g = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *g -= n;
        self.cv.notify_all();
    }
}

/// Gives budget back when it goes out of scope.
struct Give<'a>(&'a Budget, u64);

impl Drop for Give<'_> {
    fn drop(&mut self) {
        if self.1 > 0 {
            self.0.give(self.1);
        }
    }
}

/// A written file, read by ranges (opened at each read: a game has far more files than a process may keep open).
struct LooseSrc {
    path: PathBuf,
    size: u64,
}

impl Src for LooseSrc {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::result::Result<(), SplitError> {
        if off.saturating_add(buf.len() as u64) > self.size {
            return Err(SplitError::Red("read past the end of the file".into()));
        }
        std::fs::File::open(&self.path)
            .and_then(|f| f.read_exact_at(buf, off))
            .map_err(|e| SplitError::Format(format!("read {}: {e}", self.path.display())))
    }
}

/// One file name of the container as a path part: UTF-8, not empty, `.` or `..`, no `/` or NUL.
fn part(n: &Nup, name: &[u8]) -> Result<String> {
    let s = String::from_utf8(name.to_vec())
        .map_err(|_| bad(&n.path, format!("file name is not UTF-8: {name:?}")))?;
    if s.is_empty() || s == "." || s == ".." || s.contains(['/', '\0']) {
        return Err(bad(&n.path, format!("unsafe file name {s:?}")));
    }
    Ok(s)
}

/// Folders as paths relative to the root, and files as (relative path, container index).
type Tree = (Vec<String>, Vec<(String, usize)>);

/// The folders and files below folder `d`.
fn walk(n: &Nup, d: u32) -> Result<Tree> {
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    let mut seen = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![(d, String::new(), 0usize)];
    while let Some((dir, path, depth)) = stack.pop() {
        if depth > 64 || !seen.insert(dir) {
            return Err(bad(&n.path, "folder loop or nesting too deep"));
        }
        for (name, isdir, i) in n.children(dir) {
            let p = if path.is_empty() {
                part(n, &name)?
            } else {
                format!("{path}/{}", part(n, &name)?)
            };
            if !names.insert(p.clone()) {
                return Err(bad(&n.path, format!("{p} is in the container twice")));
            }
            if isdir {
                dirs.push(p.clone());
                stack.push((i as u32, p, depth + 1));
            } else {
                files.push((p, i));
            }
        }
    }
    Ok((dirs, files))
}

/// Replaces `root` with an empty folder holding `dirs`.
fn make_dirs(root: &Path, dirs: &[String]) -> Result<()> {
    if root.exists() {
        std::fs::remove_dir_all(root)
            .map_err(|e| ExtractError::io(format!("remove {}", root.display()), e))?;
    }
    std::fs::create_dir_all(root)
        .map_err(|e| ExtractError::io(format!("create {}", root.display()), e))?;
    for d in dirs {
        let p = root.join(d);
        std::fs::create_dir_all(&p)
            .map_err(|e| ExtractError::io(format!("create {}", p.display()), e))?;
    }
    Ok(())
}

/// Writes one file. Returns its bytes when they fit in memory (the split scans them, then the caller gives back the
/// budget the second value names), else `None`.
fn put(n: &Nup, job: &Job, verify: bool, budget: &Budget) -> Result<(Option<Vec<u8>>, u64)> {
    let f = &n.files[job.file];
    let werr = |e: std::io::Error| ExtractError::io(format!("write {}", job.path.display()), e);
    if f.zstd || f.size <= WHOLE {
        let held = budget.take(f.size + if f.zstd { f.stored } else { 0 });
        let r = n.read_checked(job.file, verify).and_then(|data| {
            std::fs::write(&job.path, &data).map_err(werr)?;
            Ok(data)
        });
        if r.is_err() {
            budget.give(held);
        }
        return Ok((Some(r?), held));
    }
    // a large raw file: in chunks, hashed on the way
    let mut out = std::fs::File::create(&job.path).map_err(werr)?;
    let held = budget.take(CHUNK as u64);
    let mut buf = vec![0u8; CHUNK];
    let mut h = verify.then(Sha256::new);
    let mut at = 0u64;
    let r = (|| {
        while at < f.size {
            let k = ((f.size - at) as usize).min(CHUNK);
            n.st.read_at(f.off + at, &mut buf[..k])?;
            if let Some(h) = h.as_mut() {
                h.update(&buf[..k]);
            }
            std::io::Write::write_all(&mut out, &buf[..k]).map_err(werr)?;
            at += k as u64;
        }
        Ok::<(), ExtractError>(())
    })();
    budget.give(held);
    r?;
    if let Some(h) = h
        && h.finalize().0 != f.sha
    {
        return Err(bad(
            &n.path,
            format!("{} does not match its SHA-256 (damaged file)", job.rel),
        ));
    }
    Ok((None, 0))
}

/// Unpacks one version (and every DLC) of a container into folders, with the shader split in the same pass.
pub fn unpack_nup(r: &UnpackRequest) -> Result<Unpacked> {
    let n = Nup::open(&r.nup)?;
    let s = scan(&n)?;
    let (ver, k, d, mut notes) = pick(&n, &s, r.version)?;
    std::fs::create_dir_all(&r.out)
        .map_err(|e| ExtractError::io(format!("create {}", r.out.display()), e))?;
    exefs(&n, &k, &r.out.join("ExeFS"))?;
    let app_pkg = n
        .packages
        .iter()
        .find(|p| p.kind == super::BASE)
        .unwrap_or(&n.packages[0])
        .clone();
    control(&n, &k, s.base.is_none().then_some(&app_pkg), &r.out)?;

    // the folders first, then the files of the version and of every DLC
    let mut jobs = Vec::new();
    let (dirs, files) = walk(&n, d)?;
    make_dirs(&r.romfs, &dirs)?;
    let (count, bytes) = (files.len(), files.iter().map(|f| n.files[f.1].size).sum());
    for (rel, i) in files {
        jobs.push(Job {
            file: i,
            path: r.romfs.join(&rel),
            rel,
            split: r.split.is_some(),
        });
    }
    let mut dlc = Vec::new();
    // no folder of an earlier unpack stays
    if r.aoc.exists() {
        std::fs::remove_dir_all(&r.aoc)
            .map_err(|e| ExtractError::io(format!("remove {}", r.aoc.display()), e))?;
    }
    for (tid, pi) in &s.dlc {
        let index = u32::try_from(tid - s.app - 0x1000).map_err(|_| {
            bad(
                &n.path,
                format!("DLC {tid:016X}: its index does not fit 32 bits"),
            )
        })?;
        let root = r.aoc.join(index.to_string());
        let (dirs, files) = walk(&n, n.packages[*pi].romfs)?;
        make_dirs(&root, &dirs)?;
        for (rel, i) in files {
            jobs.push(Job {
                file: i,
                path: root.join(&rel),
                rel,
                split: false,
            });
        }
        dlc.push((*tid, index));
    }
    // in the order of the data in the container: the reads stay close to sequential
    jobs.sort_by_key(|j| n.files[j.file].off);

    let mut t = match &r.split {
        Some((game_tsv, _)) => {
            let mut t = Templates::new(game_tsv)?;
            let main = std::fs::read(r.out.join("ExeFS/main")).map_err(|e| {
                ExtractError::io(format!("read {}", r.out.join("ExeFS/main").display()), e)
            })?;
            t.set_common(&main);
            // the zstd dictionaries before any scan
            if let Some(j) = jobs
                .iter()
                .find(|j| j.split && j.rel == "Pack/ZsDic.pack.zs")
            {
                t.add_dict_pack(&n.read(j.file)?)?;
            }
            Some((t, main))
        }
        None => None,
    };

    if let Some(c) = &r.progress {
        c.total.store(
            jobs.iter().map(|j| n.files[j.file].size).sum(),
            Ordering::Relaxed,
        );
    }
    let budget = Budget {
        used: Mutex::new(0),
        cv: Condvar::new(),
        cap: r.mem_limit.max(CHUNK as u64),
    };
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let fatal: Mutex<Option<ExtractError>> = Mutex::new(None);
    let tmpl = t.as_ref().map(|x| &x.0);
    std::thread::scope(|sc| {
        for _ in 0..r.threads.max(1) {
            sc.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let f = &n.files[job.file];
                    let res = put(&n, job, r.verify, &budget).map(|(data, held)| {
                        let _give = Give(&budget, held);
                        if let (Some(t), true) = (tmpl, job.split) {
                            let label = format!("/{}", job.rel);
                            let digest = Digest(f.sha);
                            match data {
                                Some(d) => t.scan(&label, &d, digest),
                                None => {
                                    let src = LooseSrc {
                                        path: job.path.clone(),
                                        size: f.size,
                                    };
                                    let head =
                                        (&src as &dyn Src).read_up(0, 16).unwrap_or_default();
                                    if !t.defer(&label, &head, digest) {
                                        t.scan_with(&label, &src, digest);
                                    }
                                }
                            }
                        }
                    });
                    if let Err(e) = res {
                        if let Ok(mut g) = fatal.lock() {
                            g.get_or_insert(e);
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    if let Some(c) = &r.progress {
                        c.done.fetch_add(f.size, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    if let Some(e) = fatal.into_inner().ok().flatten() {
        return Err(e);
    }

    let mut split_unsupported = None;
    if let (Some((t, main)), Some((_, outputs))) = (t.take(), &r.split) {
        let mut post: Vec<PostFile> = Vec::new();
        let mut files = Vec::new();
        for j in jobs.iter().filter(|j| j.split) {
            let f = &n.files[j.file];
            let digest = Digest(f.sha);
            files.push((j.rel.clone(), digest));
            post.push(PostFile {
                label: format!("/{}", j.rel),
                src: Arc::new(LooseSrc {
                    path: j.path.clone(),
                    size: f.size,
                }),
                digest,
            });
        }
        post.sort_by(|a, b| a.label.cmp(&b.label));
        let scanned = t.scanned();
        let res = t.post(post, r.threads).and_then(|()| {
            t.merge(
                &[VersionIn {
                    tag: String::new(),
                    main,
                    files,
                    dir: Some(r.out.clone()),
                }],
                outputs,
            )
        });
        match res {
            Ok((_, red)) => {
                notes.push(format!(
                    "shader split: {scanned} distinct files scanned of {count}"
                ));
                notes.extend(red);
            }
            Err(ExtractError::Unsupported(m)) => split_unsupported = Some(m),
            Err(e) => return Err(e),
        }
    }
    Ok(Unpacked {
        version: ver,
        files: count,
        bytes,
        dlc,
        notes,
        split_unsupported,
    })
}
