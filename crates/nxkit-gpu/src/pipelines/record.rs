//! The list of pipelines that a run asked for (`pipelines.seen`).
//!
//! Every render, mesh and compute pipeline that the host makes is noted here with the description that made it: the
//! library file stems and entry names of the stages (the names the shard index uses), the colour attachments with
//! their formats, blend state and write masks, the vertex layout, the topology class, the sample count, alpha to
//! coverage and ICB support. The next start makes all of them from the runtime archive before the first frame (see
//! `runtime.rs`).
//!
//! The text holds no game data: the stems are shader keys (hashes), the rest are Metal state names and numbers. It also
//! holds the key of the pipeline cache, the hash that the encoder computes for the draw, so that a pipeline made at the
//! start is found by the first draw that asks. The keys follow the key scheme
//! (`KEY_SCHEME` in `keys.rs`), and the folder of the cache is named by it, so a list is only read by a host with the
//! same scheme.
//!
//! The text of the list is `nxkit_pipestate::list`, which the build reads and writes too.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use nxkit_metal::{MeshPipelineDesc, RenderPipelineDesc, ShaderLibrary, Source};

pub(super) use nxkit_pipestate::list::{
    ComputeRec, Entry, MeshRec, RenderRec, Seen, Stage, colors_text, name_ok,
};

/// The names that the shard index uses for a stage, when the library came from a file.
fn stage(l: &ShaderLibrary, entry: &str) -> Option<Stage> {
    let stem = l.stem()?;
    (name_ok(stem) && name_ok(entry)).then(|| (stem.to_string(), entry.to_string()))
}

/// The record of a render description, when every part of it can be written.
pub(super) fn render_rec(
    key: u64,
    vertex_key: u64,
    fallback: bool,
    d: &RenderPipelineDesc<'_>,
) -> Option<RenderRec> {
    let fragment = match d.fragment {
        Some((l, e)) => Some(stage(l, e)?),
        None => None,
    };
    colors_text(&d.colors)?;
    Some(RenderRec {
        key: Some(key),
        vertex_key: Some(vertex_key),
        fallback,
        vertex: stage(d.vertex.0, d.vertex.1)?,
        fragment,
        colors: d.colors.clone(),
        layout: d.vertex_layout.clone(),
        topology: d.topology,
        samples: d.sample_count,
        a2c: d.alpha_to_coverage,
        icb: d.support_icb,
    })
}

/// The record of a mesh description.
pub(super) fn mesh_rec(key: u64, d: &MeshPipelineDesc<'_>) -> Option<MeshRec> {
    let opt = |s: Option<(&ShaderLibrary, &str)>| match s {
        Some((l, e)) => stage(l, e).map(Some),
        None => Some(None),
    };
    colors_text(&d.colors)?;
    Some(MeshRec {
        key: Some(key),
        object: opt(d.object)?,
        mesh: stage(d.mesh.0, d.mesh.1)?,
        fragment: opt(d.fragment)?,
        colors: d.colors.clone(),
        samples: d.sample_count,
        a2c: d.alpha_to_coverage,
        object_threads: d.object_threads,
        mesh_threads: d.mesh_threads,
        payload: d.payload_bytes,
    })
}

/// What the run has asked for. Set once by [`arm`].
static ARMED: AtomicBool = AtomicBool::new(false);
static LIST: Mutex<Option<Seen>> = Mutex::new(None);
/// The shipped list (`nxkit_pipestate::list::SHIPPED_LIST`): its pipelines are not added to the run's list.
static SHIPPED: Mutex<Option<Seen>> = Mutex::new(None);
/// Entries added since the list was last written.
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Pipelines made since the runtime archive was last written: the ones that Metal compiled or specialized.
static FRESH: AtomicU64 = AtomicU64::new(0);

/// Whether `NXKIT_PIPELINE_RECORD` allows the list: on unless it is `0`.
pub(super) fn record_on() -> bool {
    crate::settings::env().pipeline_record
}

/// Starts the list from what earlier runs wrote. The pipelines of `shipped` are not noted: the app has them.
pub(super) fn arm(initial: Seen, shipped: Seen) {
    *SHIPPED.lock().unwrap_or_else(PoisonError::into_inner) = Some(shipped);
    *LIST.lock().unwrap_or_else(PoisonError::into_inner) = Some(initial);
    ARMED.store(true, Ordering::Release);
}

/// True once [`arm`] ran: the hooks of the encoder do nothing before.
pub(super) fn armed() -> bool {
    ARMED.load(Ordering::Acquire)
}

fn add(e: Option<Entry>, fresh: bool) {
    if fresh {
        FRESH.fetch_add(1, Ordering::Relaxed);
    }
    let Some(e) = e else { return };
    if SHIPPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|s| s.contains(&e))
    {
        return;
    }
    let mut g = LIST.lock().unwrap_or_else(PoisonError::into_inner);
    if g.as_mut().is_some_and(|l| l.insert(e)) {
        DIRTY.store(true, Ordering::Release);
    }
}

fn made(s: Source) -> bool {
    matches!(s, Source::Compiled(_) | Source::Specialized(_))
}

/// Notes a render pipeline that the cache made or found.
pub(super) fn render(
    key: u64,
    vertex_key: u64,
    fallback: bool,
    d: &RenderPipelineDesc<'_>,
    s: Source,
) {
    if armed() && s != Source::Cached {
        add(
            render_rec(key, vertex_key, fallback, d).map(Entry::Render),
            made(s),
        );
    }
}

/// Notes a mesh pipeline.
pub(super) fn mesh(key: u64, d: &MeshPipelineDesc<'_>, s: Source) {
    if armed() && s != Source::Cached {
        add(mesh_rec(key, d).map(Entry::Mesh), made(s));
    }
}

/// Notes a compute pipeline of a library from a file.
pub(super) fn compute(key: u64, lib: &ShaderLibrary, entry: &str) {
    if armed() {
        add(
            stage(lib, entry).map(|function| {
                Entry::Compute(ComputeRec {
                    key: Some(key),
                    function,
                })
            }),
            false,
        );
    }
}

/// The list as it is now, when it changed since the last call.
pub(super) fn take_dirty() -> Option<Seen> {
    if !DIRTY.swap(false, Ordering::AcqRel) {
        return None;
    }
    LIST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Puts back the mark that [`take_dirty`] cleared, after a write that failed.
pub(super) fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

/// The pipelines compiled or specialized since the last [`take_fresh`], without taking them.
pub(super) fn fresh() -> u64 {
    FRESH.load(Ordering::Acquire)
}

/// The pipelines compiled or specialized since the last call.
pub(super) fn take_fresh() -> u64 {
    FRESH.swap(0, Ordering::AcqRel)
}

/// Puts back what [`take_fresh`] took, after a write that failed.
pub(super) fn restore_fresh(n: u64) {
    FRESH.fetch_add(n, Ordering::Relaxed);
}
