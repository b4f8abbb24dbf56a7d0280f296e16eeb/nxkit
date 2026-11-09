//! Native replacement of a run of compute dispatches.
//!
//! Some games run a whole network as a chain of compute dispatches. A [`Replacement`] knows one such chain. It finds
//! the chain by the program keys of the frame's dispatches, says which bound weight buffers prove that it is the
//! network it knows, and builds a [`Plan`] of native passes. The plan holds buffers, kernels, grids and bindings. This
//! module checks the run, caches the verdict and the built plan, and encodes the plan in place of the run. If anything
//! does not match, the game's own dispatches run as they do without this module. The interface names no network.
//! `native_ndve` is the first user.
//!
//! The steps of a frame:
//!
//! 1. [`Native::prepare`] runs once per frame, before the passes are encoded. It looks for a run, reads the weight bytes
//!    from guest memory through the storage buffer bindings of the run's dispatches, and looks the verdict up. The cache
//!    key is the addresses and a hash of the bytes. Guest memory has no write counter, so every frame reads the bytes
//!    and hashes them. That costs a few dozen microseconds, and a changed weight blob never meets a stale plan. A new key
//!    builds the plan once: the buffers (with their packed weights), the pipelines and the barrier flags.
//! 2. `prepare` then resolves the game's textures from the bindings of the run, so nothing can fail later. The result is
//!    a [`Prepared`] that the passes read.
//! 3. The pass loop skips the dispatches of the run and calls [`encode`] at the last one. [`encode`] makes one compute
//!    pass with the plan's steps and a barrier between steps that depend on each other.
//!
//! `NXKIT_NDVE=0` turns the module off. `NXKIT_DIAG=ndve` prints the capture list once per distinct run.
//! `NXKIT_NDVE_COMPARE=1` with it runs the game's chain and the plan on a sampled frame and prints the difference.

mod book;
mod build;
mod run;
mod util;

#[cfg(test)]
use build::{barrier_flags, written_resources};
use build::{build, used_resources, uses_resource};
use util::{bind_of, blob_start, compare_on, enabled, hash_blobs, run_commands, sbuf_of};
use run::{Compare, PendingCompare, capture, count_fallback, count_native, make_compare};
pub(crate) use book::book_writes;
use book::intermediates_read_later;
pub(crate) use run::{encode, health_lines};

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use nxkit_metal::{
    ArgumentTable, CommandBuffer, ComputeCommand, ComputePipeline, Context, DataBuffer, Dependency,
    GuestMemory, Scratch, Storage, Texture, TextureDesc,
};
use nxkit_ndve::plan::{Binding, GameResource, Plan, Source, Step};
use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::resource::TexId;

use crate::detail::{DispatchDetail, FrameDetail, Stage, TextureBind, UboDetail};
use crate::draws::{FastMap, Shared};
use crate::pipelines::Pipelines;
use crate::snapshot::EncodeView;
use crate::stats::{EncodeStats, Skip};
use crate::textures::Res;

// ---------------------------------------------------------------------------------------------------------------------
// The interface
// ---------------------------------------------------------------------------------------------------------------------

/// The run of dispatches that a replacement found: `len` dispatches from the dispatch at index `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    /// The index of the first dispatch of the run in the frame's dispatch list.
    pub(crate) start: usize,
    /// The number of dispatches.
    pub(crate) len: usize,
}

/// A byte range of the weights, found through a storage buffer binding of the run. The `dispatch` counts from the
/// start of the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlobSpec {
    /// The dispatch of the run whose binding names the bytes.
    pub(crate) dispatch: usize,
    /// The slot of the storage buffer in that dispatch.
    pub(crate) slot: u32,
    /// Where the bound address lies in the blob. The blob starts at the address minus this.
    pub(crate) offset: u64,
    /// The length of the blob in bytes.
    pub(crate) len: u64,
}

/// Where a game resource is bound in the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// A sampled texture of a dispatch.
    Texture {
        /// The dispatch of the run.
        dispatch: usize,
        /// The texture slot.
        slot: u32,
    },
    /// A storage image of a dispatch.
    Image {
        /// The dispatch of the run.
        dispatch: usize,
        /// The image slot.
        slot: u32,
    },
    /// The compute push constants of a dispatch.
    Push {
        /// The dispatch of the run.
        dispatch: usize,
    },
}

/// One game resource and where the run binds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResourceSpec {
    /// The resource the plan names.
    pub(crate) resource: GameResource,
    /// Where it is bound.
    pub(crate) place: Place,
}

/// A network that can replace a run of dispatches.
pub(crate) trait Replacement: Send + Sync {
    /// A short name for the diagnostics and the health report.
    fn name(&self) -> &'static str;

    /// Finds the run in the program keys of the frame's dispatches.
    fn find(&self, keys: &[&str]) -> Option<Run>;

    /// The weight ranges to read. `accepts` and `plan` get their bytes in this order.
    fn blobs(&self) -> &'static [BlobSpec];

    /// True when the bytes are the weights of the network that this replacement implements.
    fn accepts(&self, blobs: &[&[u8]]) -> bool;

    /// Builds the plan from the weights. It is called once for each distinct set of bytes.
    fn plan(&self, blobs: &[&[u8]]) -> Result<Plan, String>;

    /// The Metal source of the kernels that the plan's steps name.
    fn source(&self) -> &'static str;

    /// Where the run binds the game resources that the plan names.
    fn resources(&self) -> &'static [ResourceSpec];
}

// ---------------------------------------------------------------------------------------------------------------------
// The cache
// ---------------------------------------------------------------------------------------------------------------------

/// A plan made ready: the buffers are in Metal, the pipelines are built.
struct Entry {
    plan: Plan,
    buffers: Vec<Arc<DataBuffer>>,
    pipelines: Vec<ComputePipeline>,
    /// For each step, whether a barrier goes before it.
    barrier_before: Vec<bool>,
    /// The game resources that the plan writes, in the order of the plan.
    written: Vec<GameResource>,
}

/// What the cache knows about a set of weights.
#[derive(Clone)]
enum Verdict {
    Ready(Arc<Entry>),
    Refused(&'static str, String),
}

/// The addresses where the blobs start and a hash of all their bytes.
type Key = (Vec<u64>, u64);

/// Why a frame keeps the game's dispatches: a counted reason and a sentence for the diagnostics.
struct Refusal(&'static str, String);

fn refuse<T>(reason: &'static str, what: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal(reason, what.into()))
}

/// The state of the native path, owned by the encoder.
pub(crate) struct Native {
    nets: Vec<Box<dyn Replacement>>,
    cache: HashMap<(usize, Key), Verdict>,
    captured: HashSet<(usize, Key)>,
    blobs: Vec<Vec<u8>>,
    /// The frames the plan ran in, for the sample of the compare.
    ready_frames: u64,
    frames: u64,
    pending: Vec<PendingCompare>,
}

impl Native {
    pub(crate) fn new() -> Self {
        Self::with(vec![Box::new(crate::native_ndve::Ndve)])
    }

    fn with(nets: Vec<Box<dyn Replacement>>) -> Self {
        Self {
            nets,
            cache: HashMap::new(),
            captured: HashSet::new(),
            blobs: Vec::new(),
            ready_frames: 0,
            frames: 0,
            pending: Vec::new(),
        }
    }

    /// Called once at the start of every frame: prints the compare results that are due.
    pub(crate) fn begin_frame(&mut self) {
        self.frames += 1;
        let now = self.frames;
        let mut due = Vec::new();
        self.pending.retain_mut(|p| {
            if p.frame + COMPARE_DELAY <= now {
                due.push(std::mem::take(p));
                false
            } else {
                true
            }
        });
        for p in due {
            p.report();
        }
    }

    /// Looks for a run in the frame and prepares it. `None` means that the game's dispatches run: either no run was
    /// found, or it was refused (counted with the reason).
    pub(crate) fn prepare(&mut self, cx: &PrepareCx<'_>, stats: &mut EncodeStats) -> Option<Prepared> {
        if !enabled() || cx.detail.dispatches.len() < 2 {
            return None;
        }
        let keys: Vec<&str> = cx
            .detail
            .dispatches
            .iter()
            .map(|d| d.program.as_str())
            .collect();
        let (net, run) = self
            .nets
            .iter()
            .enumerate()
            .find_map(|(i, n)| n.find(&keys).map(|r| (i, r)))?;
        match self.attempt(net, run, cx, stats) {
            Ok(p) => {
                count_native(self.nets[net].name());
                Some(p)
            }
            Err(Refusal(reason, what)) => {
                count_fallback(self.nets[net].name(), reason, &what);
                None
            }
        }
    }

    fn attempt(
        &mut self,
        net: usize,
        run: Run,
        cx: &PrepareCx<'_>,
        stats: &mut EncodeStats,
    ) -> Result<Prepared, Refusal> {
        let name = self.nets[net].name();
        let dispatches = &cx.detail.dispatches;
        if run.len == 0
            || run
                .start
                .checked_add(run.len)
                .is_none_or(|e| e > dispatches.len())
        {
            return refuse(
                "run",
                format!(
                    "{name}: run {run:?} lies outside {} dispatches",
                    dispatches.len()
                ),
            );
        }
        let (_, anchor_cmd) = run_commands(&cx.frame.cmds, run).ok_or_else(|| {
            Refusal(
                "interleaved",
                format!("{name}: other commands lie between the dispatches of {run:?}"),
            )
        })?;
        let spans = self.nets[net].blobs();
        let mut starts = Vec::with_capacity(spans.len());
        self.blobs.resize_with(spans.len(), Vec::new);
        for (spec, bytes) in spans.iter().zip(&mut self.blobs) {
            let dd = dispatches
                .get(run.start + spec.dispatch)
                .filter(|_| spec.dispatch < run.len)
                .ok_or_else(|| {
                    Refusal(
                        "binding",
                        format!("{name}: no dispatch {} in the run", spec.dispatch),
                    )
                })?;
            let sbuf = sbuf_of(dd, spec.slot).ok_or_else(|| {
                Refusal(
                    "binding",
                    format!(
                        "{name}: dispatch {} binds no storage buffer {}",
                        spec.dispatch, spec.slot
                    ),
                )
            })?;
            let start = blob_start(sbuf.address, spec.offset).ok_or_else(|| {
                Refusal(
                    "binding",
                    format!(
                        "{name}: address {:#x} is below offset {:#x}",
                        sbuf.address, spec.offset
                    ),
                )
            })?;
            let at = cx.guest_offset(start, spec.len).ok_or_else(|| {
                Refusal(
                    "memory",
                    format!(
                        "{name}: blob {start:#x} + {:#x} is outside guest memory",
                        spec.len
                    ),
                )
            })?;
            bytes.resize(spec.len as usize, 0);
            cx.guest
                .read(at, bytes)
                .map_err(|e| Refusal("memory", format!("{name}: blob read: {e}")))?;
            starts.push(start);
        }
        let hash = hash_blobs(&self.blobs);
        let key: (usize, Key) = (net, (starts, hash));
        if !self.cache.contains_key(&key) {
            let blobs: Vec<&[u8]> = self.blobs.iter().map(Vec::as_slice).collect();
            let verdict = match build(&*self.nets[net], &blobs, cx) {
                Ok(e) => Verdict::Ready(Arc::new(e)),
                Err(Refusal(reason, what)) => Verdict::Refused(reason, what),
            };
            self.cache.insert(key.clone(), verdict);
        }
        let first_time = crate::diag("ndve") && self.captured.insert(key.clone());
        if first_time {
            capture(&*self.nets[net], run, cx, key.1.1);
            match &self.cache[&key] {
                Verdict::Ready(e) => eprintln!(
                    "DIAG ndve {name}: plan ready, {} steps, {} buffers",
                    e.plan.steps.len(),
                    e.buffers.len()
                ),
                Verdict::Refused(reason, what) => {
                    eprintln!("DIAG ndve {name}: refused ({reason}): {what}")
                }
            }
        }
        let entry = match &self.cache[&key] {
            Verdict::Ready(e) => e.clone(),
            Verdict::Refused(reason, what) => return refuse(reason, what.clone()),
        };
        let mut prepared = self.bind(net, &entry, run, cx, anchor_cmd, stats)?;
        if first_time {
            eprintln!(
                "DIAG ndve {name}: intermediates the frame reads later {:?}",
                prepared.reads_later
            );
        }
        self.ready_frames += 1;
        if compare_on()
            && crate::diag("ndve")
            && self.ready_frames % COMPARE_EVERY == COMPARE_FIRST
            && let Some(c) = make_compare(&entry, &prepared, cx)
        {
            self.pending.push(c.pending(self.frames));
            prepared.compare = Some(c);
        }
        Ok(prepared)
    }

    /// Resolves the game resources of the plan from the bindings of the run.
    fn bind(
        &self,
        net: usize,
        entry: &Arc<Entry>,
        run: Run,
        cx: &PrepareCx<'_>,
        anchor_cmd: usize,
        stats: &mut EncodeStats,
    ) -> Result<Prepared, Refusal> {
        let name = self.nets[net].name();
        let mut textures = Vec::new();
        let mut pushes = Vec::new();
        for spec in self.nets[net].resources() {
            if !uses_resource(&entry.plan, spec.resource) {
                continue;
            }
            let writes = entry.written.contains(&spec.resource);
            let dd = |dispatch: usize| {
                cx.detail
                    .dispatches
                    .get(run.start + dispatch)
                    .filter(|_| dispatch < run.len)
                    .ok_or_else(|| {
                        Refusal(
                            "binding",
                            format!(
                                "{name}: {:?} names dispatch {dispatch}, outside the run",
                                spec.resource
                            ),
                        )
                    })
            };
            match spec.place {
                Place::Push { dispatch } => {
                    let bytes = dd(dispatch)?.push.clone().ok_or_else(|| {
                        Refusal(
                            "push",
                            format!(
                                "{name}: dispatch {dispatch} pushed no constants for {:?}",
                                spec.resource
                            ),
                        )
                    })?;
                    pushes.push((spec.resource, bytes));
                }
                Place::Texture { dispatch, slot } | Place::Image { dispatch, slot } => {
                    let d = dd(dispatch)?;
                    let list = if matches!(spec.place, Place::Image { .. }) {
                        &d.images
                    } else {
                        &d.textures
                    };
                    let bind = bind_of(list, slot).ok_or_else(|| {
                        Refusal(
                            "resource",
                            format!(
                                "{name}: dispatch {dispatch} binds nothing at slot {slot} for {:?}",
                                spec.resource
                            ),
                        )
                    })?;
                    let (id, res, header) = cx.resolve(bind.handle).ok_or_else(|| {
                        Refusal(
                            "resource",
                            format!(
                                "{name}: handle {:#x} of {:?} does not resolve",
                                bind.handle, spec.resource
                            ),
                        )
                    })?;
                    if res.scaled {
                        return refuse(
                            "resource",
                            format!("{name}: {:?} is stored at the render scale", spec.resource),
                        );
                    }
                    if writes && !res.desc.shader_write {
                        return refuse(
                            "resource",
                            format!(
                                "{name}: {:?} cannot be written by a shader ({:?})",
                                spec.resource, res.desc.format
                            ),
                        );
                    }
                    // The same view the game's own dispatch binds: its format may differ from the backing's (an
                    // sRGB texture written as plain 8-bit), and a kernel write to the backing would encode.
                    let view = if matches!(spec.place, Place::Image { .. }) {
                        cx.store
                            .image_view(id, res, header, nxkit_metal::TextureKind::D2, 0, stats)
                    } else {
                        cx.store.view(id, res, header, stats)
                    }
                    .map_err(|_| {
                        Refusal(
                            "resource",
                            format!("{name}: no view of {:?} ({:?})", spec.resource, res.desc.format),
                        )
                    })?;
                    let desc = view.desc();
                    textures.push((spec.resource, view, desc, id));
                }
            }
        }
        for used in used_resources(&entry.plan) {
            let have = textures.iter().any(|(r, _, _, _)| *r == used)
                || pushes.iter().any(|(r, _)| *r == used);
            if !have {
                return refuse(
                    "spec",
                    format!(
                        "{name}: the plan names {used:?}, the replacement does not say where it is bound"
                    ),
                );
            }
        }
        let reads_later = intermediates_read_later(cx.frame, anchor_cmd, cx, &textures);
        Ok(Prepared {
            entry: entry.clone(),
            name,
            first: (run.start) as u32,
            anchor: (run.start + run.len - 1) as u32,
            textures,
            pushes,
            reads_later,
            compare: None,
        })
    }
}

/// The frames between two compared frames, and the position of the first one in that period.
const COMPARE_EVERY: u64 = 120;
const COMPARE_FIRST: u64 = 10;
/// The frames after which the compare reads its pictures. The GPU is done with them by then.
const COMPARE_DELAY: u64 = 4;

// ---------------------------------------------------------------------------------------------------------------------
// What the frame needs
// ---------------------------------------------------------------------------------------------------------------------

/// What [`Native::prepare`] reads from the encoder.
pub(crate) struct PrepareCx<'a> {
    pub(crate) ctx: &'a Context,
    pub(crate) guest: &'a Arc<GuestMemory>,
    pub(crate) pipelines: &'a Pipelines,
    pub(crate) frame: &'a Frame,
    pub(crate) detail: &'a FrameDetail,
    pub(crate) view: &'a dyn EncodeView,
    pub(crate) textures: &'a FastMap<TexId, Res>,
    pub(crate) store: &'a crate::textures::TextureStore,
}

impl PrepareCx<'_> {
    /// The offset of a guest GPU address in guest memory, or `None` when the range lies outside.
    fn guest_offset(&self, addr: u64, len: u64) -> Option<u64> {
        let map = self.guest.address_map();
        let off = addr.checked_sub(map.gpu_base())?;
        (off.checked_add(len)? <= map.len()).then_some(off)
    }

    /// The texture of a handle (a texture handle or the texture id of an image descriptor).
    fn resolve(&self, handle: u64) -> Option<(TexId, &Res, &nxkit_nvn::tic::Tic)> {
        let (slot, _) = self.view.resolve(handle).ok()?;
        Some((slot.texture, self.textures.get(&slot.texture)?, &slot.header))
    }
}

/// A run that is ready to be encoded in this frame.
pub(crate) struct Prepared {
    entry: Arc<Entry>,
    name: &'static str,
    /// The index of the first dispatch of the run.
    first: u32,
    /// The index of the last dispatch of the run. The plan is encoded in its place.
    anchor: u32,
    /// The game textures and images of the plan, with the description they were made with.
    textures: Vec<(GameResource, Texture, TextureDesc, TexId)>,
    pushes: Vec<(GameResource, Arc<[u8]>)>,
    /// For each [`GameResource::Intermediate`] the plan writes: whether a later command of the frame reads it.
    reads_later: Vec<(GameResource, bool)>,
    compare: Option<Compare>,
}

impl Prepared {
    /// True when the dispatch with this index is replaced and must not be encoded. A compare frame keeps the game's
    /// dispatches.
    pub(crate) fn skips(&self, dispatch: u32) -> bool {
        self.compare.is_none() && (self.first..=self.anchor).contains(&dispatch)
    }

    /// The textures of the game that the plan writes. The frame books them as written by the GPU in the pass of the
    /// anchor dispatch, as it books the images of the dispatches the plan replaces.
    pub(crate) fn writes(&self) -> Vec<TexId> {
        let mut out: Vec<TexId> = Vec::new();
        for (r, _, _, id) in &self.textures {
            if self.entry.written.contains(r) && !out.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    /// True for the dispatch where the plan is encoded.
    pub(crate) fn is_anchor(&self, dispatch: u32) -> bool {
        dispatch == self.anchor
    }
}

#[cfg(test)]
mod tests;
