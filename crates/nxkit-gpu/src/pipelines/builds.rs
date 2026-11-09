//! The background compiles of render and mesh pipelines (`NXKIT_PIPELINE_ASYNC`, on by default).
//!
//! A draw whose pipeline the cache lacks is left out (`Skip::Pending`) and its pipeline is queued once per key. The
//! threads take the job that a draw asked for in the latest frame first, the oldest of those first: the pipelines of
//! the frame on screen go before those of a scene long gone, and a job is never lost. A request for a key that waits
//! only marks it as wanted again; the encoder checks that before it builds a job, so a draw that waits costs a lock.

use std::cmp::Reverse;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Condvar;
use std::time::Instant;

use super::*;

/// A pipeline to compile in the background, with an owned copy of its description.
pub(super) struct Job {
    pub(super) key: u64,
    pub(super) what: String,
    pub(super) kind: JobKind,
}

pub(super) enum JobKind {
    Render(RenderJob),
    Mesh(MeshJob),
}

/// A render pipeline of vertex and fragment stages.
pub(super) struct RenderJob {
    pub(super) vertex: (ShaderLibrary, String),
    pub(super) fragment: Option<(ShaderLibrary, String)>,
    pub(super) colors: Vec<Option<nxkit_metal::ColorTarget>>,
    pub(super) vertex_layout: nxkit_metal::VertexDescriptor,
    pub(super) topology: TopologyClass,
    pub(super) sample_count: u32,
    pub(super) alpha_to_coverage: bool,
}

/// A mesh pipeline: object, mesh and fragment stages.
pub(super) struct MeshJob {
    pub(super) object: Option<(ShaderLibrary, String)>,
    pub(super) mesh: (ShaderLibrary, String),
    pub(super) fragment: Option<(ShaderLibrary, String)>,
    pub(super) colors: Vec<Option<nxkit_metal::ColorTarget>>,
    pub(super) sample_count: u32,
    pub(super) alpha_to_coverage: bool,
    pub(super) object_threads: u32,
    pub(super) mesh_threads: u32,
    pub(super) payload_bytes: u32,
}

impl MeshJob {
    pub(super) fn desc(&self) -> MeshPipelineDesc<'_> {
        fn fun((l, e): &(ShaderLibrary, String)) -> (&ShaderLibrary, &str) {
            (l, e.as_str())
        }
        MeshPipelineDesc {
            object: self.object.as_ref().map(fun),
            mesh: fun(&self.mesh),
            fragment: self.fragment.as_ref().map(fun),
            colors: self.colors.clone(),
            sample_count: self.sample_count,
            alpha_to_coverage: self.alpha_to_coverage,
            object_threads: self.object_threads,
            mesh_threads: self.mesh_threads,
            payload_bytes: self.payload_bytes,
        }
    }
}

/// The state of the background compiles.
#[derive(Default)]
pub(super) struct Builds {
    queue: Mutex<Queue>,
    /// Signalled when a job is queued or the builds close.
    ready: Condvar,
    /// The keys whose pipeline Metal refused; their draws are skipped as `Skip::Pipeline`.
    pub(super) failed: Mutex<FastSet<u64>>,
}

#[derive(Default)]
struct Queue {
    /// The jobs no thread has taken yet, by key.
    waiting: FastMap<u64, Waiting>,
    /// The keys a thread compiles now.
    running: FastSet<u64>,
    /// The number of jobs queued so far, which orders jobs of one frame.
    queued: u64,
    /// The threads started; none before the first job.
    threads: usize,
    /// Set when the [`Pipelines`] are dropped: the threads end.
    closed: bool,
}

/// A queued job, with the frame of the last draw that asked for it and its place in the queue.
struct Waiting {
    job: Job,
    frame: u64,
    order: u64,
}

impl Queue {
    /// The key to compile next: asked for in the latest frame, queued first among those.
    fn next(&self) -> Option<u64> {
        pick(self.waiting.iter().map(|(k, w)| (*k, w.frame, w.order)))
    }

    fn publish(&self) {
        stats::queue(self.waiting.len(), self.running.len(), self.threads);
    }
}

/// Of (key, frame of the last request, place in the queue), the key asked for in the latest frame, queued first among
/// those.
fn pick(jobs: impl Iterator<Item = (u64, u64, u64)>) -> Option<u64> {
    jobs.max_by_key(|&(_, frame, order)| (frame, Reverse(order)))
        .map(|(k, _, _)| k)
}

/// The threads that compile pipelines in the background: `NXKIT_PIPELINE_THREADS`, else half the cores, 2 to 8. A
/// thread waits on the system's compiler service most of the time, and that service compiles requests side by side.
fn compile_threads() -> usize {
    std::env::var("NXKIT_PIPELINE_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 8))
        })
}

/// Puts a compile thread in its QoS class, which carries over to the compiler service. User-initiated by default: a
/// pending pipeline leaves draws out of the frame on screen, and at the utility class the service ran on the
/// efficiency cores. `NXKIT_PIPELINE_QOS=utility` brings back the old class.
fn set_compile_qos() {
    if std::env::var("NXKIT_PIPELINE_QOS").is_ok_and(|v| v == "utility") {
        nxkit_metal::thread::set_utility();
    } else {
        nxkit_metal::thread::set_user_initiated();
    }
}

/// Whether a render pipeline that the cache lacks compiles in the background (`NXKIT_PIPELINE_ASYNC=0` turns it off).
/// The draw is left out until it is ready, a frame or two, instead of stalling the encode for the 5 to 30 ms of the
/// compile.
pub(super) fn async_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // The unit tests build synchronously, so a call returns its pipeline.
    *ON.get_or_init(|| {
        !cfg!(test) && std::env::var("NXKIT_PIPELINE_ASYNC").map_or(true, |v| v != "0")
    })
}

impl Builds {
    /// True when `key` is queued or compiling; a queued job is marked as wanted by the current frame.
    pub(super) fn touch(&self, key: u64) -> bool {
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(w) = q.waiting.get_mut(&key) {
            w.frame = stats::frame();
            return true;
        }
        q.running.contains(&key)
    }

    /// Queues `job` once per key. The threads start with the first job; when none can start, the job runs here.
    pub(super) fn start(
        self: &Arc<Self>,
        cache: &Arc<PipelineCache>,
        fallbacks: &Arc<Mutex<FastSet<u64>>>,
        job: Job,
    ) {
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if q.closed || q.running.contains(&job.key) {
            return;
        }
        if let Some(w) = q.waiting.get_mut(&job.key) {
            w.frame = stats::frame();
            return;
        }
        if q.threads == 0 {
            for i in 0..compile_threads() {
                let (me, cache, fallbacks) = (self.clone(), cache.clone(), fallbacks.clone());
                let spawned = std::thread::Builder::new()
                    .name(format!("nxkit-pipeline-{i}"))
                    .spawn(move || me.work(&cache, &fallbacks));
                q.threads += usize::from(spawned.is_ok());
            }
        }
        if q.threads == 0 {
            drop(q);
            self.run(cache, fallbacks, &job);
            return;
        }
        q.queued += 1;
        let order = q.queued;
        q.waiting.insert(
            job.key,
            Waiting {
                job,
                frame: stats::frame(),
                order,
            },
        );
        stats::queued();
        q.publish();
        drop(q);
        self.ready.notify_one();
    }

    /// Ends the threads; the jobs still queued are dropped.
    pub(super) fn close(&self) {
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        q.closed = true;
        q.waiting.clear();
        drop(q);
        self.ready.notify_all();
    }

    /// The loop of a compile thread: takes the next job until the builds close.
    fn work(&self, cache: &PipelineCache, fallbacks: &Mutex<FastSet<u64>>) {
        set_compile_qos();
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
                loop {
                    if q.closed {
                        return;
                    }
                    if let Some(w) = q.next().and_then(|k| q.waiting.remove(&k)) {
                        q.running.insert(w.job.key);
                        q.publish();
                        break w.job;
                    }
                    q = self.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
                }
            };
            // A panic must not leave the key running for good: its draws would wait forever.
            if catch_unwind(AssertUnwindSafe(|| self.run(cache, fallbacks, &job))).is_err() {
                pipe_failed(format_args!("{}: the compile panicked", job.what));
                self.fail(job.key);
            }
            let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            q.running.remove(&job.key);
            q.publish();
        }
    }

    fn fail(&self, key: u64) {
        stats::failed();
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key);
    }

    fn run(&self, cache: &PipelineCache, fallbacks: &Mutex<FastSet<u64>>, job: &Job) {
        let t0 = Instant::now();
        let built = match &job.kind {
            JobKind::Render(r) => {
                let mut desc = RenderPipelineDesc::new(
                    (&r.vertex.0, r.vertex.1.as_str()),
                    r.fragment.as_ref().map(|(l, e)| (l, e.as_str())),
                    &[],
                );
                desc.colors = r.colors.clone();
                desc.vertex_layout = r.vertex_layout.clone();
                desc.topology = r.topology;
                desc.sample_count = r.sample_count;
                desc.alpha_to_coverage = r.alpha_to_coverage;
                build_render(cache, fallbacks, job.key, &mut desc).map(|(_, source)| {
                    crate::coverage::record(cache, source, Some(&desc), &job.what);
                    source
                })
            }
            JobKind::Mesh(m) => cache.mesh_traced(job.key, &m.desc()).map(|(_, source)| {
                crate::coverage::record(cache, source, None, &job.what);
                source
            }),
        };
        match built {
            Ok(source) => stats::made(source, t0.elapsed()),
            Err(e) => {
                pipe_failed(format_args!("{}: {e}", job.what));
                self.fail(job.key);
            }
        }
    }
}

#[cfg(test)]
mod order {
    use super::pick;

    #[test]
    fn the_latest_frame_goes_first_and_within_it_the_oldest_job() {
        // key, frame of the last request, place in the queue
        let jobs = [(1, 10, 1), (2, 12, 3), (3, 12, 2), (4, 11, 4)];
        assert_eq!(pick(jobs.into_iter()), Some(3));
    }

    #[test]
    fn a_job_asked_for_again_overtakes_the_newer_ones_of_an_old_frame() {
        let jobs = [(1, 40, 1), (2, 39, 2), (3, 39, 3)];
        assert_eq!(pick(jobs.into_iter()), Some(1));
        assert_eq!(pick(std::iter::empty()), None);
    }
}
