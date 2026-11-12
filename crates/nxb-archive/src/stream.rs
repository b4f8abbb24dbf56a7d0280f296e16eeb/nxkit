//! The pipeline archive while the shader step runs (design-pipeline-rest 10.4, overlap): the pairs are cut into groups
//! by the content of their identity (the two shader keys), so a group is known before any library exists and an
//! insertion changes only its own group. A group's batches start as soon as every shader it names is finished (the
//! shader step reports each one), on a few worker threads beside the shader step. Within a group the bases are planned
//! (`plan::plan`), hashed by their libraries' bytes and cut by content ([`cut`]) as in [`crate::build`], so the batch
//! keys in the store stay content keys. At the end the pieces are merged in key order.
//!
//! Width: every compiler run takes a permit from a [`Headroom`] built from [`StreamInputs::limits`]. A few runs
//! always go, more start while the machine's CPU and memory stay below the limits (90 % each by default), up to the
//! ceiling. While the shader step keeps the cores busy the stream stays narrow; cores it leaves idle, and all of them
//! once it ends, fill with compiler runs, one more per settle interval (the reference title, wave 9: a 232 s tail
//! after the shader step at a fixed three).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use nxb_model::{ContentKey, sha256};
use nxb_sched::{Headroom, Limits};
use nxb_store::Store;
use nxb_tool::ProcessRegistry;

use crate::cut::cut;
use crate::plan::{self, PlannedBase};
use crate::script::Script;
use crate::shards::Shard;
use crate::{BatchReport, Config, Report};

/// A compiled batch: its key, its archive, its report and the pipelines in it.
type Piece = (ContentKey, PathBuf, BatchReport, Vec<crate::shards::Ident>);

/// The pairs (or the compute libraries) of one group and how many of its shaders are still running.
struct Group {
    pairs: Vec<(String, String)>,
    computes: Vec<String>,
    waiting: usize,
}

/// The shader step's key of a compute library (`<key>_b`).
fn compute_key(lib: &str) -> &str {
    lib.strip_suffix("_b").unwrap_or(lib)
}

impl Group {
    /// The keys of the shaders this group needs.
    fn keys(&self) -> HashSet<&str> {
        let pairs = self
            .pairs
            .iter()
            .flat_map(|(v, f)| [v.as_str(), f.as_str()]);
        pairs
            .chain(self.computes.iter().map(|c| compute_key(c)))
            .collect()
    }
}

/// What a streamed archive needs besides [`Config`] (whose `script` is not used).
#[derive(Clone, Debug)]
pub struct StreamInputs {
    /// The library folder the shader step writes (`b-<chain>`).
    pub lib_dir: PathBuf,
    /// The chain version.
    pub chain: String,
    /// The pairs, in `pso_offline.py` order (profile pairs, then the split's `pso_pairs.tsv`).
    pub pairs: Vec<(String, String)>,
    /// The compute libraries (`<key>_b`) that get compute pipelines, in [`crate::compute::read_computes`] order.
    pub computes: Vec<String>,
    /// The bases of the states that a title recorded ([`crate::seen`]). Each goes into the group of its pair, and only
    /// when the pair is in `pairs` and the plan accepts its functions.
    pub extras: Vec<crate::seen::StateBase>,
    /// The recorded states as fully specialized pipelines ([`crate::exact`]). Each goes into the group of its pair, and
    /// only when the pair is in `pairs` and the plan accepts its functions. A group compiles them in batches of their
    /// own, apart from its bases.
    pub exacts: Vec<crate::exact::ExactState>,
    /// `depth_mode zero`.
    pub zero: bool,
    /// Pairs per group (about, a group's identity hash cuts it, at most 4x).
    pub group_target: usize,
    /// Concurrent compiler runs: floor, ceiling and the load limits in between.
    pub limits: Limits,
}

/// Runs `work` on every item of `rx` with `threads` threads, until the sender is gone and the queue is empty.
fn drain<T: Send>(rx: Receiver<T>, threads: usize, work: &(dyn Fn(T) + Sync)) {
    let rx = Mutex::new(rx);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            let rx = &rx;
            s.spawn(move || {
                loop {
                    let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(item) = next else { break };
                    work(item);
                }
            });
        }
    });
}

/// A streamed archive: feed it finished shaders, then [`Stream::finish`].
pub struct Stream {
    groups: Mutex<Vec<Group>>,
    by_key: HashMap<String, Vec<usize>>,
    tx: Mutex<Option<Sender<usize>>>,
    rx: Mutex<Option<Receiver<usize>>>,
}

fn pair_hash(v: &str, f: &str) -> [u8; 32] {
    sha256(format!("{v}\0{f}").as_bytes()).0
}

impl Stream {
    /// Groups the pairs, groups that need no shader are ready at once.
    pub fn new(inp: &StreamInputs) -> Stream {
        let bases: Vec<crate::cut::Base> = inp
            .pairs
            .iter()
            .enumerate()
            .map(|(index, (v, f))| crate::cut::Base {
                hash: pair_hash(v, f),
                // pairs of one vertex shader stay in one group (its functions compile once per batch)
                group: sha256(v.as_bytes()).0,
                index,
                compute: false,
            })
            .collect();
        let mut groups = Vec::new();
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for g in cut(bases, inp.group_target) {
            let gi = groups.len();
            let mut pairs: Vec<(String, String)> =
                g.iter().map(|b| inp.pairs[b.index].clone()).collect();
            // keep the pairs in their file order inside the group (plan order)
            pairs.sort_by_key(|p| inp.pairs.iter().position(|q| q == p));
            let grp = Group {
                waiting: 0,
                computes: Vec::new(),
                pairs,
            };
            let keys = grp.keys();
            for k in &keys {
                by_key.entry((*k).to_string()).or_default().push(gi);
            }
            groups.push(Group {
                waiting: keys.len(),
                ..grp
            });
        }
        for chunk in inp.computes.chunks(crate::compute::COMPUTE_BATCH) {
            let gi = groups.len();
            let grp = Group {
                waiting: 0,
                computes: chunk.to_vec(),
                pairs: Vec::new(),
            };
            let keys = grp.keys();
            for k in &keys {
                by_key.entry((*k).to_string()).or_default().push(gi);
            }
            groups.push(Group {
                waiting: keys.len(),
                ..grp
            });
        }
        let (tx, rx) = channel();
        Stream {
            groups: Mutex::new(groups),
            by_key,
            tx: Mutex::new(Some(tx)),
            rx: Mutex::new(Some(rx)),
        }
    }

    /// Number of groups.
    pub fn groups(&self) -> usize {
        self.groups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The shader keys in group order (the shader step builds them first, so groups complete one after another).
    pub fn key_order(&self) -> Vec<String> {
        let g = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for grp in g.iter() {
            for (v, f) in &grp.pairs {
                for k in [f, v] {
                    if seen.insert(k.clone()) {
                        out.push(k.clone());
                    }
                }
            }
            for c in &grp.computes {
                if seen.insert(compute_key(c).to_string()) {
                    out.push(compute_key(c).to_string());
                }
            }
        }
        out
    }

    /// The shader step's key list is known: shaders it will not build count as done (a pair can name a shader of
    /// another version or one the split did not package), so no group waits for them.
    pub fn building(&self, keys: &HashSet<String>) {
        let mut groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        for (g, grp) in groups.iter_mut().enumerate() {
            let all = grp.keys();
            let absent = all.iter().filter(|k| !keys.contains(**k)).count();
            grp.waiting = grp.waiting.saturating_sub(absent);
            if grp.waiting == 0
                && let Some(tx) = tx.as_ref()
            {
                let _ = tx.send(g);
            }
        }
    }

    /// A shader is finished (whatever its status: a failed one simply has no library, which the plan skips).
    pub fn done(&self, key: &str) {
        let Some(gs) = self.by_key.get(key) else {
            return;
        };
        let mut groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        for &g in gs {
            let w = &mut groups[g].waiting;
            *w = w.saturating_sub(1);
            if *w == 0
                && let Some(tx) = self
                    .tx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
            {
                let _ = tx.send(g);
            }
        }
    }

    /// The shader step ended: release every group still waiting (shaders not built at all).
    pub fn finish(&self) {
        let mut groups = self.groups.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = self
            .tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(tx) = tx {
            for (g, grp) in groups.iter_mut().enumerate() {
                if grp.waiting > 0 {
                    grp.waiting = 0;
                    let _ = tx.send(g);
                }
            }
        }
    }

    /// Compile groups as they become ready until [`Stream::finish`] was called and all are done; then merge into
    /// `c.out`. Run it on its own thread beside the shader step. Returns the report and the planned base count.
    pub fn run(&self, c: &Config, inp: &StreamInputs) -> Result<(Report, usize), String> {
        let t0 = Instant::now();
        let rx = self
            .rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or("stream already ran")?;
        fs::create_dir_all(&c.work).map_err(|e| format!("{}: {e}", c.work.display()))?;
        // The groups are known now, before any shader is done: count one batch for each, so the progress has a total
        // from the start. `group` corrects the count when it plans the group's real batches.
        crate::Batches::planned(c, self.groups());
        let store = Store::open(&c.store).map_err(|e| e.to_string())?;
        let reg = ProcessRegistry::new();
        let seen: Mutex<HashSet<PlannedBase>> = Mutex::new(HashSet::new());
        let pieces: Mutex<Vec<Piece>> = Mutex::new(Vec::new());
        let dropped: Mutex<Vec<crate::Dropped>> = Mutex::new(Vec::new());
        let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let groups = &self.groups;
        let headroom = Headroom::new(inp.limits);
        let work = |g: usize| {
            let (pairs, computes) = {
                let grp = &groups.lock().unwrap_or_else(PoisonError::into_inner)[g];
                (grp.pairs.clone(), grp.computes.clone())
            };
            match group(
                c,
                inp,
                (&pairs, &computes),
                (&seen, &dropped),
                &store,
                &reg,
                &headroom,
            ) {
                Ok(mut p) => pieces
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .append(&mut p),
                Err(e) => errors
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(e),
            }
        };
        // one thread per possible run: a thread waiting for a permit has its group planned already
        drain(rx, inp.limits.ceiling, &work);
        if let Some(e) = errors
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
        {
            return Err(e.clone());
        }
        let mut pieces = pieces.into_inner().unwrap_or_else(PoisonError::into_inner);
        pieces.sort_by_key(|a| a.0.to_hex());
        let shards = pieces
            .iter()
            .map(|p| Shard {
                key: p.0.to_hex(),
                path: p.1.clone(),
                idents: p.3.clone(),
            })
            .collect();
        let t1 = Instant::now();
        let mut report = Report {
            peak_runs: headroom.peak(),
            ..Report::default()
        };
        let keys: Vec<ContentKey> = pieces.iter().map(|p| p.0).collect();
        let paths: Vec<PathBuf> = pieces.iter().map(|p| p.1.clone()).collect();
        report.batches = pieces.into_iter().map(|p| p.2).collect();
        report.dropped = dropped.into_inner().unwrap_or_else(PoisonError::into_inner);
        report.shards = shards;
        if c.merge {
            crate::merge(c, &store, &reg, &keys, &paths, &mut report)?;
        } else {
            report.size = paths
                .iter()
                .map(|p| fs::metadata(p).map_or(0, |m| m.len()))
                .sum();
        }
        report.seconds = (0.0, (t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64());
        let n = seen
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        Ok((report, n))
    }
}

/// Plan, hash, cut and compile one group: its batches' (key, piece, report).
fn group(
    c: &Config,
    inp: &StreamInputs,
    (pairs, computes): (&[(String, String)], &[String]),
    (seen, dropped): (&Mutex<HashSet<PlannedBase>>, &Mutex<Vec<crate::Dropped>>),
    store: &Store,
    reg: &ProcessRegistry,
    headroom: &Headroom,
) -> Result<Vec<Piece>, String> {
    let p = plan::plan(&inp.lib_dir, &inp.chain, pairs, inp.zero);
    let (pipes, _) = crate::compute::plan_compute(&inp.lib_dir, &inp.chain, computes);
    let (bases, pipes): (Vec<PlannedBase>, Vec<crate::compute::ComputePipe>) = {
        let mut s = seen.lock().unwrap_or_else(PoisonError::into_inner);
        // a compute pipeline is counted as a base with no vertex function
        let b = p
            .bases
            .into_iter()
            .filter(|b| s.insert(b.clone()))
            .collect();
        let c = pipes
            .into_iter()
            .filter(|(l, e)| s.insert((String::new(), String::new(), l.clone(), e.clone())))
            .collect();
        (b, c)
    };
    let here: HashSet<(&str, &str)> = pairs
        .iter()
        .map(|(v, f)| (v.as_str(), f.as_str()))
        .collect();
    let mine: Vec<crate::seen::StateBase> = inp
        .extras
        .iter()
        .filter(|e| {
            e.pair_keys()
                .is_some_and(|(v, f)| here.contains(&(v.as_str(), f.as_str())))
        })
        .cloned()
        .collect();
    let (states, refused) = crate::seen::usable(&inp.lib_dir, &inp.chain, &mine, inp.zero);
    dropped
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend(refused);
    let mine_exact: Vec<crate::exact::ExactState> = inp
        .exacts
        .iter()
        .filter(|e| {
            e.pair_keys()
                .is_some_and(|(v, f)| here.contains(&(v.as_str(), f.as_str())))
        })
        .cloned()
        .collect();
    let (exacts, refused) = crate::exact::usable(&inp.lib_dir, &inp.chain, &mine_exact, inp.zero);
    dropped
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .extend(refused);
    if bases.is_empty() && pipes.is_empty() && states.is_empty() && exacts.is_empty() {
        crate::Batches::replanned(c, 1, 0);
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    // the bases and the computes in batches cut by content, the exact states in a batch of their own: a change of the
    // list recompiles that batch and leaves the bases of the group in the store
    let mut jobs: Vec<(std::sync::Arc<Script>, Vec<crate::cut::Base>, bool)> = Vec::new();
    if !(bases.is_empty() && pipes.is_empty() && states.is_empty()) {
        let script = Script::parse(&plan::script_text_all(
            &inp.lib_dir,
            &bases,
            &states,
            &pipes,
        ))?;
        let digests = digests_of(&script)?;
        let mut batches = cut(script.bases(&digests)?, c.target);
        batches.extend(crate::compute::chunks(script.compute_bases(&digests)?));
        let script = std::sync::Arc::new(script);
        jobs.extend(batches.into_iter().map(|b| (script.clone(), b, false)));
    }
    if !exacts.is_empty() {
        let script = Script::parse(&plan::script_text_full(
            &inp.lib_dir,
            &[],
            &[],
            &exacts,
            &[],
        ))?;
        let digests = digests_of(&script)?;
        let batches = crate::cut::whole(script.bases(&digests)?);
        let script = std::sync::Arc::new(script);
        jobs.extend(batches.into_iter().map(|b| (script.clone(), b, true)));
    }
    crate::Batches::replanned(c, 1, jobs.len());
    for (script, batch, exact) in jobs {
        let done = crate::compile_resilient(c, &script, &batch, store, reg, Some(headroom))?;
        crate::Batches::finished(c);
        for (key, piece, rep, kept) in done.pieces {
            let idents = if exact {
                crate::shards::exact_idents(&script, &kept)?
            } else {
                crate::shards::idents(&script, &kept)?
            };
            out.push((key, piece, rep, idents));
        }
        dropped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(done.dropped);
    }
    Ok(out)
}

/// The digest of every library of the script, by label.
fn digests_of(script: &Script) -> Result<HashMap<String, nxb_model::Digest>, String> {
    let mut digests = HashMap::new();
    for (label, path) in script.library_paths()? {
        let b = fs::read(Path::new(&path)).map_err(|e| format!("{path}: {e}"))?;
        digests.insert(label, sha256(&b));
    }
    Ok(digests)
}

#[cfg(test)]
mod tests;
