//! The second kind of split job in the stage: after the images are written, the archive families that the reference
//! reads by entry ranges (`nxb_split::jobs`) are planned over every file, run on a pool, and merged in the
//! reference's order (main, the jobs of each family, then the files).

use crate::fileext::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use nxb_model::{Digest, Sha256};
use nxb_split::SplitError;
use nxb_split::jobs::{self, Ctx, File};
use nxb_split::scanfile::finish;
use nxb_split::src::Src;

use crate::error::{ExtractError, Result};
use crate::split::{HOLD_LIMIT, Outcome, Templates, split_err};
use nxb_split::records::FileResult;
use std::collections::HashSet;

/// A file of an image, read by ranges.
pub struct ImgSrc {
    /// The image.
    pub file: Arc<std::fs::File>,
    /// Offset of the file in the image.
    pub off: u64,
    /// Size of the file.
    pub size: u64,
}

impl Src for ImgSrc {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::result::Result<(), SplitError> {
        if off.saturating_add(buf.len() as u64) > self.size {
            return Err(SplitError::Red("read past the end of the file".into()));
        }
        self.file
            .read_exact_at(buf, self.off + off)
            .map_err(|e| SplitError::Format(format!("read from the image: {e}")))
    }
}

/// A file kept in memory (the images are not kept with `--discard-images`).
pub struct Own(pub Arc<Vec<u8>>);

impl Src for Own {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn bytes(&self) -> Option<&[u8]> {
        Some(&self.0)
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> std::result::Result<(), SplitError> {
        nxb_split::src::Mem(&self.0).read_at(off, buf)
    }
}

/// A file for the planners: its label (`/` + the reference's `rel`), range reader and digest.
pub struct PostFile {
    /// `/rel`.
    pub label: String,
    /// Its bytes.
    pub src: Arc<dyn Src>,
    /// SHA-256 of its bytes.
    pub digest: Digest,
}

/// SHA-256 of a file of an image without reading it whole.
pub fn stream_digest(f: &std::fs::File, off: u64, size: u64) -> Result<Digest> {
    let (mut h, mut at) = (Sha256::new(), 0u64);
    let mut buf = vec![0u8; 8 << 20];
    while at < size {
        let n = ((size - at) as usize).min(buf.len());
        f.read_exact_at(&mut buf[..n], off + at)
            .map_err(|e| ExtractError::io("read an image file", e))?;
        h.update(&buf[..n]);
        at += n as u64;
    }
    Ok(h.finalize())
}

/// Opens an image for `ImgSrc`.
pub fn open_image(p: &Path) -> Result<Arc<std::fs::File>> {
    std::fs::File::open(p)
        .map(Arc::new)
        .map_err(|e| ExtractError::io(format!("open {}", p.display()), e))
}

impl Templates {
    /// Plans and runs the archive jobs over `files` (every file of every version, in the reference's order) and scans
    /// the files that were held back but no job took. Call once, after all files went through `scan` or `defer`.
    pub fn post(&self, files: Vec<PostFile>, threads: usize) -> Result<()> {
        if !self.has_deferred() {
            return self.unity_pass2(&files);
        }
        let live = self.deferred_labels();
        let jfiles: Vec<File> = files
            .iter()
            .map(|f| File {
                rel: f.label.trim_start_matches('/').to_string(),
                src: f.src.clone(),
                live: live.contains(&f.label),
            })
            .collect();
        let nros = jobs::NroSet {
            dir: "None".into(),
            files: Vec::new(),
        };
        let plan = jobs::plan_with(&jfiles, &nros).map_err(split_err)?;
        self.add_notes(plan.notes.iter().cloned());
        let mut taken_labels = std::collections::HashSet::new();
        for (i, f) in files.iter().enumerate() {
            if plan.taken.contains(&i) {
                taken_labels.insert(f.label.clone());
            }
        }
        // Next Level Games archives: read through their index, as plain files at their own place
        for (i, f) in files.iter().enumerate() {
            if !jfiles[i].live || taken_labels.contains(&f.label) {
                continue;
            }
            let head = f.src.as_ref().read_up(0, 4).map_err(split_err)?;
            if head == jobs::nlg::DICT {
                let r = jobs::nlg::shaders(&jfiles, i, &f.label)
                    .and_then(|(found, var)| finish(&self.prof, found, &var));
                self.put_label(&f.label, r);
            } else if jobs::nlg::covered(&jfiles[i].rel, &jfiles) {
                self.put_label(&f.label, Ok(None));
            } else {
                self.scan_src(&f.label, f.src.as_ref(), f.digest);
            }
        }
        // the jobs
        let dicts = self.dicts_guard()?;
        let ctx = Ctx {
            files: &jfiles,
            dicts: &dicts,
        };
        let n = plan.jobs.len();
        let slots: Vec<Mutex<Option<Outcome>>> = (0..n).map(|_| Mutex::new(None)).collect();
        let next = AtomicUsize::new(0);
        let notes = Mutex::new(Vec::<String>::new());
        let unsupported = Mutex::new(None::<String>);
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| {
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = plan.jobs.get(k) else { break };
                        let mut my = Vec::new();
                        let r = (job.run)(&ctx, &mut my)
                            .and_then(|o| finish(&self.prof, o.found, &o.var));
                        if let Ok(mut g) = notes.lock() {
                            g.extend(my);
                        }
                        let v = match r {
                            Ok(r) => Ok(r),
                            Err(SplitError::Unsupported(m)) => {
                                if let Ok(mut u) = unsupported.lock() {
                                    u.get_or_insert(m);
                                }
                                Err(String::new())
                            }
                            Err(e) => {
                                let why = e.to_string();
                                let rel = job.rel.as_str();
                                Err(if why.trim_start_matches('/').starts_with(rel) {
                                    why
                                } else {
                                    format!("{rel}: {why}")
                                })
                            }
                        };
                        if let Ok(mut g) = slots[k].lock() {
                            *g = Some(v);
                        }
                    }
                });
            }
        });
        if let Some(m) = unsupported.into_inner().ok().flatten() {
            return Err(ExtractError::Unsupported(m));
        }
        self.add_notes(notes.into_inner().unwrap_or_default());
        let out = slots
            .into_iter()
            .map(|m| m.into_inner().ok().flatten().unwrap_or(Err(String::new())))
            .collect();
        self.set_jobs(out, taken_labels);
        self.unity_pass2(&files)
    }

    /// The second pass over the Unity files without type trees: the trees of every bundle, then the layouts the
    /// reference keeps for the games that strip them (`KNOWN_TREES`).
    fn unity_pass2(&self, files: &[PostFile]) -> Result<()> {
        let entries: Vec<_> = self
            .unity_defer
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if entries.is_empty() {
            return Ok(());
        }
        let mut trees = nxb_split::unity::ser::Trees::new();
        if let Ok(g) = self.by_digest.lock() {
            let mut all: Vec<_> = g
                .iter()
                .filter_map(|(d, t)| Some((*d, t.get()?.as_ref().ok()?.as_ref()?.trees.clone())))
                .collect();
            all.sort_by_key(|x| x.0);
            for (_, t) in all {
                trees.extend(t);
            }
        }
        for (k, v) in nxb_split::unity::ser::known_trees().map_err(split_err)? {
            trees.entry(k).or_insert(v);
        }
        let dicts = self.dicts_guard()?;
        let env = nxb_split::scanfile::Env {
            dicts: &dicts,
            common: self.common.as_deref(),
            trees: Some(&trees),
        };
        let by_label: std::collections::HashMap<&str, &PostFile> =
            files.iter().map(|f| (f.label.as_str(), f)).collect();
        for (label, digest, held) in entries {
            let src: Arc<dyn Src> = match (held, by_label.get(label.as_str())) {
                (Some(b), _) => Arc::new(Own(b)),
                (None, Some(f)) => f.src.clone(),
                (None, None) => continue,
            };
            let mut notes = Vec::new();
            let r = nxb_split::scanfile::scan_file(
                &self.prof,
                src.as_ref(),
                "",
                &label,
                &env,
                &mut notes,
            );
            self.add_notes(notes.into_iter().map(|n| n.replace("{label}", &label)));
            let v = match r {
                Ok(Some(r)) if r.defer => {
                    return Err(ExtractError::format(format!(
                        "{}: Unity serialized file without type trees, and no bundle has a Shader type tree of the same hash to borrow",
                        label.trim_start_matches('/')
                    )));
                }
                Ok(r) => Ok(r),
                Err(SplitError::Unsupported(m)) => {
                    return Err(ExtractError::Unsupported(format!("{label}{m}")));
                }
                Err(e) => {
                    let why = e.to_string();
                    let rel = label.trim_start_matches('/');
                    Err(if why.trim_start_matches('/').starts_with(rel) {
                        why
                    } else {
                        format!("{rel}: {why}")
                    })
                }
            };
            if let Ok(mut g) = self.pass2.lock() {
                g.insert(digest, v);
            }
        }
        Ok(())
    }
}

/// A file whose bytes are gone (`--discard-images`) and that no planner needs.
struct NoSrc(u64);

impl Src for NoSrc {
    fn len(&self) -> u64 {
        self.0
    }
    fn read_at(&self, _: u64, _: &mut [u8]) -> std::result::Result<(), SplitError> {
        Err(SplitError::Format(
            "the file was not kept (--discard-images)".into(),
        ))
    }
}

impl Templates {
    /// `post` over the written images: `tags[k]` is the directory prefix of image `k` (`""` or `/v327680`).
    pub fn post_images(
        &self,
        tags: &[String],
        results: &[crate::write::ImageResult],
        threads: usize,
    ) -> Result<()> {
        if !self.has_deferred() && !self.has_unity_defer() {
            return Ok(());
        }
        let mut files = Vec::new();
        for (k, r) in results.iter().enumerate() {
            let img = if self.keep {
                None
            } else {
                Some(open_image(&r.dst)?)
            };
            let mut fs: Vec<&(Vec<u8>, u64, u64, Digest)> = r.files_digests.iter().collect();
            fs.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, off, size, d) in fs {
                let label = format!("{}{}", tags[k], String::from_utf8_lossy(path));
                let src: Arc<dyn Src> = match (&img, self.held_bytes(&label)) {
                    (Some(f), _) => Arc::new(ImgSrc {
                        file: f.clone(),
                        off: *off,
                        size: *size,
                    }),
                    (None, Some(b)) => Arc::new(Own(b)),
                    (None, None) => Arc::new(NoSrc(*size)),
                };
                files.push(PostFile {
                    label,
                    src,
                    digest: *d,
                });
            }
        }
        self.post(files, threads)
    }
}

impl Templates {
    /// Keeps the held-back archives in memory (there is no image to read them from, `--discard-images`).
    pub fn keep_deferred(&mut self) {
        self.keep = true;
    }

    /// Unity's common string table from the first version's `main` image (`None` when it has none: not a Unity game).
    pub fn set_common(&mut self, main_nso: &[u8]) {
        self.common = nxb_split::nso::image(main_nso)
            .ok()
            .and_then(|img| nxb_split::unity::bundle::common_strings(&img).ok())
            .map(std::sync::Arc::new);
    }

    /// Holds a file back for the archive planners when its name or first bytes name a family (see
    /// `nxb_split::jobs::deferred`).`true` when it was.
    pub fn defer(&self, label: &str, data: &[u8], digest: Digest) -> bool {
        if !nxb_split::jobs::deferred(label.trim_start_matches('/'), &data[..data.len().min(16)]) {
            return false;
        }
        let bytes = if self.keep {
            if self.held.fetch_add(data.len(), Ordering::Relaxed) + data.len() > HOLD_LIMIT {
                if let Ok(mut g) = self.unsupported.lock() {
                    g.get_or_insert(format!("{label}: archives over {HOLD_LIMIT} bytes with --discard-images (no image to read ranges from)"));
                }
                None
            } else {
                Some(std::sync::Arc::new(data.to_vec()))
            }
        } else {
            None
        };
        if let Ok(mut g) = self.deferred.lock() {
            g.push((label.to_string(), digest, bytes));
        }
        true
    }

    pub(crate) fn has_unity_defer(&self) -> bool {
        self.unity_defer.lock().is_ok_and(|g| !g.is_empty())
    }

    pub(crate) fn has_deferred(&self) -> bool {
        self.deferred.lock().is_ok_and(|g| !g.is_empty())
    }

    pub(crate) fn deferred_labels(&self) -> HashSet<String> {
        self.deferred
            .lock()
            .map(|g| g.iter().map(|d| d.0.clone()).collect())
            .unwrap_or_default()
    }

    /// The bytes held for a deferred file (`--discard-images`).
    pub fn held_bytes(&self, label: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        self.deferred
            .lock()
            .ok()?
            .iter()
            .find(|d| d.0 == label)
            .and_then(|d| d.2.clone())
    }

    pub(crate) fn add_notes(&self, lines: impl IntoIterator<Item = String>) {
        if let Ok(mut g) = self.notes.lock() {
            g.extend(lines);
        }
    }

    pub(crate) fn put_label(
        &self,
        label: &str,
        r: std::result::Result<Option<FileResult>, SplitError>,
    ) {
        let v = match r {
            Ok(r) => Ok(r),
            Err(SplitError::Unsupported(m)) => {
                if let Ok(mut g) = self.unsupported.lock() {
                    g.get_or_insert(format!("{label}: {m}"));
                }
                Err(String::new())
            }
            Err(e) => Err(e.to_string()),
        };
        if let Ok(mut g) = self.by_label.lock() {
            g.insert(label.to_string(), v);
        }
    }

    pub(crate) fn set_jobs(
        &self,
        out: Vec<std::result::Result<Option<FileResult>, String>>,
        taken: HashSet<String>,
    ) {
        if let Ok(mut g) = self.jobs.lock() {
            *g = (out, taken);
        }
    }

    pub(crate) fn dicts_guard(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, nxb_split::zstd::Dicts>> {
        self.dicts
            .read()
            .map_err(|_| ExtractError::format("dictionary table poisoned"))
    }

    /// Scans a held-back file from its range reader (the plain scan of a file no job took).
    pub(crate) fn scan_src(&self, label: &str, src: &dyn nxb_split::src::Src, digest: Digest) {
        self.scan_with(label, src, digest);
    }
}
