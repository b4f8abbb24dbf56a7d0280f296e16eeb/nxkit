//! Writing the images of all versions in one pass: one job per file, jobs of all versions merged so that a file of the
//! update follows the same path's file of `v0` (their base blocks then come from the cache), workers `pwrite` each
//! file at its planned offset, and one hasher per image takes the files in image order (SHA-256 + SHA-1 of the whole
//! image for the signer, SHA-256 per file for the split), so no byte is read back to hash it.
//!
//! Memory: a job reserves its size before it starts, in job order, and the hasher releases it, jobs are reserved in
//! the same order as each hasher consumes them, so the earliest pending file always has its memory and the pipeline
//! cannot deadlock.

use crate::fileext::FileExt;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};

use nxb_model::{Digest, DualDigest, FileDigest, Sha1, Sha256, sha256};

use crate::error::{ExtractError, Result};
use crate::image::ImagePlan;
use crate::storage::Storage;

/// One image to write.
pub struct ImageJob {
    /// Where (`RomFS.img`, written as `RomFS.img.part`, renamed at the end).
    pub dst: PathBuf,
    /// The layout.
    pub plan: ImagePlan,
    /// The checked RomFS to read from.
    pub src: Arc<dyn Storage>,
    /// Merge key per plan entry (smaller first, equal keys keep version order).
    pub keys: Vec<u64>,
    /// Digests only: the image is not written (no file is created), its bytes are still read, checked and hashed.
    pub discard: bool,
}

/// What came out of one image.
#[derive(Debug, Clone)]
pub struct ImageResult {
    /// The image file.
    pub dst: PathBuf,
    /// Files and data bytes.
    pub files: u64,
    /// Data bytes.
    pub bytes: u64,
    /// Entries.
    pub entries: u64,
    /// Whole image: size, SHA-256, SHA-1.
    pub digest: DualDigest,
    /// Image size.
    pub size: u64,
    /// Per file: (path, offset in the image, size, SHA-256), image order.
    pub files_digests: Vec<(Vec<u8>, u64, u64, Digest)>,
    /// Paths that failed the hash check.
    pub damaged: Vec<String>,
}

struct Budget {
    state: Mutex<(usize, u64)>, // next job, bytes in flight
    cv: Condvar,
    limit: u64,
}

impl Budget {
    fn take(&self, jobs: &[(usize, usize, u64)], stop: &AtomicBool) -> Option<usize> {
        let mut g = self.state.lock().ok()?;
        loop {
            if stop.load(Ordering::Relaxed) || g.0 >= jobs.len() {
                return None;
            }
            let size = jobs[g.0].2;
            if g.1 == 0 || g.1 + size <= self.limit {
                let j = g.0;
                g.0 += 1;
                g.1 += size;
                return Some(j);
            }
            g = self.cv.wait(g).ok()?;
        }
    }
    fn release(&self, size: u64) {
        if let Ok(mut g) = self.state.lock() {
            g.1 = g.1.saturating_sub(size);
        }
        self.cv.notify_all();
    }
}

/// A file's bytes on their way through the hashers, the memory budget is released when the last hasher drops them.
struct Held<'a> {
    data: Vec<u8>,
    budget: &'a Budget,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.budget.release(self.data.len() as u64);
    }
}

/// A budget nothing is reserved from (the index's bytes).
static NO_BUDGET: Budget = Budget {
    state: Mutex::new((0, 0)),
    cv: Condvar::new(),
    limit: 0,
};

enum Done {
    File {
        entry: usize,
        data: Vec<u8>,
        digest: Digest,
    },
    Damaged {
        entry: usize,
        path: String,
    },
}

fn open_part(dst: &Path, size: u64, discard: bool) -> Result<(PathBuf, File)> {
    if discard {
        let f = OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .map_err(|e| ExtractError::io("open /dev/null", e))?;
        return Ok((PathBuf::new(), f));
    }
    let mut part = dst.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ExtractError::io(format!("create {}", dir.display()), e))?;
    }
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&part)
        .map_err(|e| ExtractError::io(format!("create {}", part.display()), e))?;
    f.set_len(size)
        .map_err(|e| ExtractError::io(format!("size {}", part.display()), e))?;
    Ok((part, f))
}

/// Called by the worker for each file after it is written: (image, path, bytes, SHA-256). The fused split scans here,
/// from the bytes in memory (design-pipeline-rest 9.3).
pub type FileHook<'a> = dyn Fn(usize, &[u8], &[u8], &Digest) + Sync + 'a;

/// [`write_images`] that counts the images' file bytes into `progress`: `total` before the first write, `done` as each
/// file is written (the caller's progress display).
pub fn write_images_counted(
    images: Vec<ImageJob>,
    threads: usize,
    mem_limit: u64,
    progress: Option<&crate::run::Counter>,
) -> Result<(Vec<ImageResult>, Closing)> {
    let Some(c) = progress else {
        return write_images(images, threads, mem_limit, None);
    };
    let total = images
        .iter()
        .flat_map(|j| j.plan.entries.iter().filter(|e| !e.dir).map(|e| e.b))
        .sum();
    c.total.store(total, Ordering::Relaxed);
    let hook = |_: usize, _: &[u8], data: &[u8], _: &Digest| {
        c.done.fetch_add(data.len() as u64, Ordering::Relaxed);
    };
    write_images(images, threads, mem_limit, Some(&hook))
}

/// The TSV of the images' digests (`# image`, path, offset, size, sha256): the whole image, then each file.
pub fn digests_text(results: &[ImageResult]) -> String {
    let mut t = String::from("# image\tpath\toffset\tsize\tsha256\n");
    for r in results {
        let img = r.dst.display().to_string();
        t.push_str(&format!(
            "{img}\t\t0\t{}\t{}\tsha1={}\n",
            r.size,
            r.digest.file.sha256.to_hex(),
            r.digest.sha1.to_hex()
        ));
        for (path, off, size, d) in &r.files_digests {
            t.push_str(&format!(
                "{img}\t{}\t{off}\t{size}\t{}\n",
                String::from_utf8_lossy(path),
                d.to_hex()
            ));
        }
    }
    t
}

/// Writes all images with `threads` workers and at most `mem_limit` bytes of file data in flight.
pub fn write_images(
    images: Vec<ImageJob>,
    threads: usize,
    mem_limit: u64,
    hook: Option<&FileHook<'_>>,
) -> Result<(Vec<ImageResult>, Closing)> {
    // per image: index bytes, header, file
    let mut outs = Vec::new();
    for im in &images {
        let index = im.plan.index_bytes()?;
        let header = im.plan.header(index.len() as u64);
        let total = im.plan.index_offset + index.len() as u64;
        let (part, file) = open_part(&im.dst, total, im.discard)?;
        outs.push((index, header, total, part, Arc::new(file)));
    }
    // jobs: (image, entry, size), merged by key, version order on ties
    let mut jobs: Vec<(u64, usize, usize, usize, u64)> = Vec::new();
    for (i, im) in images.iter().enumerate() {
        // a running maximum keeps each image's jobs in its own order (the hasher's order, see the module docs)
        let mut key = 0u64;
        for (n, (e, k)) in im.plan.entries.iter().zip(&im.keys).enumerate() {
            key = key.max(*k);
            if !e.dir {
                jobs.push((key, i, jobs.len(), n, e.b));
            }
        }
    }
    // stable merge: sort by (key, image) but keep each image's own order (keys per image are made monotone)
    jobs.sort_by_key(|j| (j.0, j.1, j.2));
    let jobs: Vec<(usize, usize, u64)> = jobs.into_iter().map(|j| (j.1, j.3, j.4)).collect();

    let budget = Budget {
        state: Mutex::new((0, 0)),
        cv: Condvar::new(),
        limit: mem_limit,
    };
    let stop = AtomicBool::new(false);
    let (txs, rxs): (Vec<_>, Vec<_>) = images.iter().map(|_| mpsc::channel::<Done>()).unzip();

    let results = std::thread::scope(|sc| -> Result<Vec<ImageResult>> {
        let mut hashers = Vec::new();
        for (i, rx) in rxs.into_iter().enumerate() {
            let im = &images[i];
            let (index, header, total, _, file) = &outs[i];
            let budget = &budget;
            hashers.push(sc.spawn(move || -> Result<ImageResult> {
                // SHA-256 here, SHA-1 in a second thread over the same buffers: the image hash is the one
                // sequential pass of the stage, so its two digests run side by side
                let (sha1_tx, sha1_rx) = mpsc::channel::<Arc<Held<'_>>>();
                std::thread::scope(|hs| {
                    let sha1 = hs.spawn(move || {
                        let mut h = Sha1::new();
                        h.update(header);
                        while let Ok(b) = sha1_rx.recv() {
                            h.update(&b.data);
                        }
                        h
                    });
                    let mut h = Sha256::new();
                    h.update(header);
                    let mut len = header.len() as u64;
                    let files: Vec<usize> = (0..im.plan.entries.len())
                        .filter(|&n| !im.plan.entries[n].dir)
                        .collect();
                    let mut next = 0usize;
                    let mut pending: BTreeMap<usize, Option<(Vec<u8>, Digest)>> = BTreeMap::new();
                    let mut digests = Vec::with_capacity(files.len());
                    let mut damaged = Vec::new();
                    while next < files.len() {
                        let Ok(msg) = rx.recv() else { break };
                        match msg {
                            Done::File {
                                entry,
                                data,
                                digest,
                            } => {
                                pending.insert(entry, Some((data, digest)));
                            }
                            Done::Damaged { entry, path } => {
                                damaged.push(path);
                                pending.insert(entry, None);
                            }
                        }
                        while next < files.len() {
                            let n = files[next];
                            let Some(d) = pending.remove(&n) else { break };
                            let e = &im.plan.entries[n];
                            if let Some((d, digest)) = d {
                                let held = Arc::new(Held { data: d, budget });
                                crate::prof::timed(&crate::prof::COUNTERS.image_hash, || {
                                    h.update(&held.data)
                                });
                                len += held.data.len() as u64;
                                digests.push((e.path.clone(), e.a, e.b, digest));
                                let _ = sha1_tx.send(held);
                            } else {
                                budget.release(e.b);
                            }
                            next += 1;
                        }
                    }
                    file.write_all_at(header, 0)
                        .map_err(|e| ExtractError::io("write image header", e))?;
                    file.write_all_at(index, im.plan.index_offset)
                        .map_err(|e| ExtractError::io("write image index", e))?;
                    h.update(index);
                    len += index.len() as u64;
                    let _ = sha1_tx.send(Arc::new(Held {
                        data: index.clone(),
                        budget: &NO_BUDGET,
                    }));
                    drop(sha1_tx);
                    let sha1 = sha1
                        .join()
                        .map_err(|_| ExtractError::format("SHA-1 thread panicked"))?
                        .finalize();
                    let (nf, bytes) = im.plan.totals();
                    Ok(ImageResult {
                        dst: im.dst.clone(),
                        files: nf,
                        bytes,
                        entries: im.plan.entries.len() as u64,
                        digest: DualDigest {
                            file: FileDigest {
                                len,
                                sha256: h.finalize(),
                            },
                            sha1,
                        },
                        size: *total,
                        files_digests: digests,
                        damaged,
                    })
                })
            }));
        }
        let mut workers = Vec::new();
        for _ in 0..threads.max(1) {
            let (jobs, budget, stop, images, outs) = (&jobs, &budget, &stop, &images, &outs);
            let txs = txs.clone();
            workers.push(sc.spawn(move || -> Result<()> {
                while let Some(j) = budget.take(jobs, stop) {
                    let (i, n, size) = jobs[j];
                    let e = &images[i].plan.entries[n];
                    let mut data = vec![0u8; size as usize];
                    match images[i].src.read_at(e.src, &mut data) {
                        Ok(()) => {}
                        Err(ExtractError::Damaged(_)) => {
                            let path = String::from_utf8_lossy(&e.path).into_owned();
                            let _ = txs[i].send(Done::Damaged { entry: n, path });
                            continue;
                        }
                        Err(err) => {
                            stop.store(true, Ordering::Relaxed);
                            budget.cv.notify_all();
                            return Err(err);
                        }
                    }
                    crate::prof::timed(&crate::prof::COUNTERS.write, || {
                        outs[i].4.write_all_at(&data, e.a)
                    })
                    .map_err(|err| {
                        stop.store(true, Ordering::Relaxed);
                        ExtractError::io(format!("write {}", images[i].dst.display()), err)
                    })?;
                    let digest =
                        crate::prof::timed(&crate::prof::COUNTERS.file_hash, || sha256(&data));
                    if let Some(h) = hook {
                        crate::prof::timed(&crate::prof::COUNTERS.split, || {
                            h(i, &e.path, &data, &digest)
                        });
                    }
                    let _ = txs[i].send(Done::File {
                        entry: n,
                        data,
                        digest,
                    });
                }
                Ok(())
            }));
        }
        drop(txs);
        let mut first_err = None;
        for w in workers {
            match w.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => first_err = first_err.or(Some(e)),
                Err(_) => first_err = first_err.or(Some(ExtractError::format("worker panicked"))),
            }
        }
        let mut res = Vec::new();
        for h in hashers {
            match h.join() {
                Ok(Ok(r)) => res.push(r),
                Ok(Err(e)) => first_err = first_err.or(Some(e)),
                Err(_) => first_err = first_err.or(Some(ExtractError::format("hasher panicked"))),
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(res),
        }
    });
    let results = match results {
        Ok(r) => r,
        Err(e) => {
            for o in &outs {
                let _ = std::fs::remove_file(&o.3);
            }
            return Err(e);
        }
    };
    let damaged: Vec<String> = results.iter().flat_map(|r| r.damaged.clone()).collect();
    if !damaged.is_empty() {
        for o in &outs {
            let _ = std::fs::remove_file(&o.3);
        }
        return Err(ExtractError::Damaged(damaged));
    }
    for (o, im) in outs.iter().zip(&images) {
        if im.discard {
            continue;
        }
        std::fs::rename(&o.3, &im.dst)
            .map_err(|e| ExtractError::io(format!("rename {}", o.3.display()), e))?;
    }
    // Closing a large written file blocks until the external disk has taken the data (seen: 70 to 100 s for 3.7 GB on
    // the USB SSD at its sustained 50 MB/s, under 0.5 s on the internal disk). The images are complete and renamed.
    // the close runs in the background so the next stage can start, and `Closing::wait` reports it.
    let handle = std::thread::spawn(move || {
        let t = std::time::Instant::now();
        drop(outs);
        t.elapsed().as_secs_f64()
    });
    Ok((results, Closing(Some(handle))))
}

/// The images' files closing in the background (see `write_images`).
pub struct Closing(Option<std::thread::JoinHandle<f64>>);

impl Closing {
    /// Nothing left to close.
    pub fn done() -> Closing {
        Closing(None)
    }

    /// Waits for the close, returns its seconds.
    pub fn wait(mut self) -> f64 {
        self.0.take().and_then(|h| h.join().ok()).unwrap_or(0.0)
    }
}

impl std::fmt::Debug for Closing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Closing")
    }
}
