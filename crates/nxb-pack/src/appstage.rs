//! The Mac app stage: materialise a bundle from a file set, then sign it, in one call.
//!
//! What the reference build script `pack_app.py` does today (measured on the reference title: 0:45 for 58,152 shader files
//! cloned by batched `cp -R -c` processes, then two `codesign` runs that read all 4.7 GB again):
//! here the file set is a [`StagePlan`] (destination path, source path, digests when known).
//! Directories are created once, files are cloned (`std::fs::copy`, which uses `fclonefileat`
//! on APFS and falls back to a copy) by a thread pool, generated files are written directly, and
//! the digests the seal needs come from the plan, or are computed by the same worker right after
//! its copy (one read per file, from the page cache when the copy was a real copy). Signing then
//! runs with those digests ([`crate::sign_bundle`] with a [`DigestSource`]): no file is read twice.
//!
//! The output is exactly what staging the same files and running `codesign` would give, and the
//! [`Error::Unsupported`] contract holds: on that error the caller runs `codesign` on the staged
//! bundle instead.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bundle::{self, BundleOptions, BundleReport, Layout};
use crate::digest::{self, DigestSource, FileDigests, HashJob};
use crate::error::{Error, Result, format_err};
use crate::hash::unhex;

/// Where one staged file's content comes from.
#[derive(Clone, Debug)]
pub enum Content {
    /// Cloned or copied from this file.
    File(PathBuf),
    /// Written from memory (Info.plist, PkgInfo, small tables).
    Bytes(Vec<u8>),
}

/// One file of the bundle.
#[derive(Clone, Debug)]
pub struct StageEntry {
    /// Destination path relative to the bundle directory (`Contents/Resources/romfs.img`).
    pub dest: String,
    /// The content.
    pub content: Content,
    /// The content's digests when the producer knows them (a build store).`sha1` may be absent
    /// when it is not needed.
    pub digests: Option<FileDigests>,
}

/// The file set of a bundle.
#[derive(Clone, Debug, Default)]
pub struct StagePlan {
    /// Every file, in any order. Symbolic links and empty directories are not staged.
    pub entries: Vec<StageEntry>,
}

/// Bytes staged so far and in all, for a progress display: one atomic add for each file.
#[derive(Debug, Default)]
pub struct Staged {
    /// Bytes of the files staged (and hashed) so far.
    pub done: AtomicU64,
    /// Bytes of all files.
    pub total: AtomicU64,
}

/// Options for [`stage_and_sign`].
pub struct StageOptions {
    /// Worker threads for copying and hashing.
    pub threads: usize,
    /// Compute missing digests while staging (true), or let the signer hash after staging.
    pub hash_while_staging: bool,
    /// Entitlements of the main executable.
    pub entitlements: Option<Vec<u8>>,
    /// Hardened runtime.
    pub runtime: bool,
    /// Counts the staged bytes (`None`: not counted).
    pub progress: Option<Arc<Staged>>,
}

impl Default for StageOptions {
    fn default() -> Self {
        StageOptions {
            threads: digest::default_threads(),
            hash_while_staging: true,
            entitlements: None,
            runtime: false,
            progress: None,
        }
    }
}

/// What [`stage_and_sign`] did.
#[derive(Clone, Debug, Default)]
pub struct StageReport {
    /// Files staged.
    pub files: usize,
    /// Bytes staged (logical sizes).
    pub bytes: u64,
    /// Files whose digests were computed during staging.
    pub hashed_while_staging: usize,
    /// The signer's report.
    pub sign: BundleReport,
    /// Time of the staging phase (directories, clones, writes, digests).
    pub stage_time: Duration,
    /// Time of the signing phase.
    pub sign_time: Duration,
}

/// Digests by path relative to the sealed root.
struct MapDigests(HashMap<String, (u64, FileDigests)>);

impl DigestSource for MapDigests {
    fn lookup(&self, path: &str, size: u64) -> Option<FileDigests> {
        self.0
            .get(path)
            .filter(|(s, _)| *s == size)
            .map(|(_, d)| *d)
    }
}

fn check_rel(p: &str) -> Result<()> {
    if p.is_empty()
        || p.starts_with('/')
        || p.split('/').any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format_err(format!("bad destination path {p:?}")));
    }
    Ok(())
}

/// Stages `plan` into the new directory `out` (which must not exist) and signs it ad hoc.
pub fn stage_and_sign(plan: &StagePlan, out: &Path, opts: &StageOptions) -> Result<StageReport> {
    let t0 = Instant::now();
    if out.exists() {
        return Err(Error::Refused(format!("{} exists", out.display())));
    }
    let mut dirs = BTreeSet::new();
    for e in &plan.entries {
        check_rel(&e.dest)?;
        let mut d = e.dest.as_str();
        while let Some(i) = d.rfind('/') {
            d = &d[..i];
            if !dirs.insert(d.to_string()) {
                break;
            }
        }
    }
    fs::create_dir_all(out).map_err(|e| Error::io(out, e))?;
    for d in &dirs {
        // BTreeSet order puts parents before children.
        let p = out.join(d);
        match fs::create_dir(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::io(&p, e)),
        }
    }

    // Largest first, so the long clones/copies start early.
    let mut order: Vec<usize> = (0..plan.entries.len()).collect();
    let size_of = |e: &StageEntry| match &e.content {
        Content::File(p) => fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        Content::Bytes(b) => b.len() as u64,
    };
    let sizes: Vec<u64> = plan.entries.iter().map(size_of).collect();
    order.sort_by_key(|i| std::cmp::Reverse(sizes[*i]));
    if let Some(c) = &opts.progress {
        c.total.store(sizes.iter().sum(), Ordering::Relaxed);
    }
    let next = AtomicUsize::new(0);
    let computed: Mutex<Vec<(usize, FileDigests)>> = Mutex::new(Vec::new());
    let threads = opts.threads.max(1);
    let results: Vec<Result<()>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut buf = Vec::new();
                    let mut local = Vec::new();
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = order.get(k) else { break };
                        let e = &plan.entries[i];
                        let dst = out.join(&e.dest);
                        match &e.content {
                            Content::File(src) => {
                                fs::copy(src, &dst).map_err(|err| Error::io(src, err))?;
                            }
                            Content::Bytes(b) => {
                                fs::write(&dst, b).map_err(|err| Error::io(&dst, err))?
                            }
                        }
                        if opts.hash_while_staging && e.digests.is_none() {
                            let job = HashJob {
                                path: dst,
                                size: sizes[i],
                                sha1: true,
                            };
                            local.push((i, digest::hash_file(&job, &mut buf)?));
                        }
                        if let Some(c) = &opts.progress {
                            c.done.fetch_add(sizes[i], Ordering::Relaxed);
                        }
                    }
                    computed
                        .lock()
                        .map_err(|_| format_err("lock"))?
                        .extend(local);
                    Ok(())
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(format_err("stage worker panicked")))
            })
            .collect()
    });
    for r in results {
        r?;
    }
    let computed = computed.into_inner().map_err(|_| format_err("lock"))?;
    let mut report = StageReport {
        files: plan.entries.len(),
        bytes: sizes.iter().sum(),
        hashed_while_staging: computed.len(),
        ..StageReport::default()
    };

    // Digests keyed by path relative to the sealed root.
    let layout = Layout::detect(out)?;
    let root_rel = layout
        .root
        .strip_prefix(out)
        .map_err(|_| format_err("sealed root outside the bundle"))?;
    let prefix = match root_rel.to_str() {
        Some("") => String::new(),
        Some(r) => format!("{r}/"),
        None => return Err(Error::Unsupported("non-UTF-8 root".into())),
    };
    let mut map = HashMap::new();
    let known = plan
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| e.digests.map(|d| (i, d)));
    for (i, d) in known.chain(computed) {
        if let Some(rel) = plan.entries[i].dest.strip_prefix(&prefix) {
            map.insert(rel.to_string(), (sizes[i], d));
        }
    }
    report.stage_time = t0.elapsed();

    let t1 = Instant::now();
    let source = MapDigests(map);
    let bopts = BundleOptions {
        entitlements: opts.entitlements.clone(),
        sign_nested: true,
        runtime: opts.runtime,
        threads,
        digests: &source,
    };
    report.sign = bundle::sign_layout(&layout, &bopts)?;
    report.sign_time = t1.elapsed();
    Ok(report)
}

/// Reads a stage manifest: `dest <TAB> source-path [<TAB> size <TAB> sha1-or-- <TAB> sha256]`
/// per line (the digest columns as in [`crate::TsvDigests`]).
pub fn parse_manifest(text: &str) -> Result<StagePlan> {
    let mut plan = StagePlan::default();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let bad = || format_err(format!("manifest line {}: {line:?}", n + 1));
        let digests = match f.len() {
            2 => None,
            5 => {
                let sha1 = match f[3] {
                    "-" => None,
                    h => Some(unhex(h).and_then(|v| v.try_into().ok()).ok_or_else(bad)?),
                };
                let sha256 = unhex(f[4])
                    .and_then(|v| v.try_into().ok())
                    .ok_or_else(bad)?;
                f[2].parse::<u64>().map_err(|_| bad())?;
                Some(FileDigests { sha1, sha256 })
            }
            _ => return Err(bad()),
        };
        plan.entries.push(StageEntry {
            dest: f[0].to_string(),
            content: Content::File(f[1].into()),
            digests,
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lines() {
        let d = FileDigests {
            sha1: Some([1; 20]),
            sha256: [2; 32],
        };
        let line = crate::TsvDigests::line("/src/x", 3, &d);
        let text = format!("Contents/a\t/src/a\nContents/x\t{line}\n");
        let p = parse_manifest(&text).unwrap();
        assert_eq!(p.entries.len(), 2);
        assert!(p.entries[0].digests.is_none());
        assert_eq!(p.entries[1].digests, Some(d));
        assert!(parse_manifest("a\tb\tc").is_err());
        assert!(check_rel("Contents/../x").is_err());
        assert!(check_rel("/abs").is_err());
        assert!(check_rel("Contents/x").is_ok());
    }
}
