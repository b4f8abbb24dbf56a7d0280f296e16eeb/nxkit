//! The walk of the recordings that a game submits, for the detail that the frame does not carry.
//!
//! The front end of NVN reads a recording into a `Frame`, which holds only what the pass analysis needs. The encoder
//! also needs what each draw binds. The walker reads the same recordings a second time and follows the bind records
//! (programs, buffers, vertex layout, state objects, viewport) to fill a [`FrameDetail`].
//!
//! The ordinals agree with the decoder of the front end. Each `Draw`, each `Dispatch`, each clear of a colour slot
//! below 8 and each depth stencil clear gives one entry, in submit order. Each command that the decoder turns into
//! `Cmd::Copy` or `Cmd::Downsample` gives one copy entry, whatever it copies (see `copy_detail`). A call record plays its recording with the
//! state of the caller, and what the callee sets stays set. The state of a queue stays across submits.
//!
//! A walk runs when the queue submits, because the game may write the command memory again after the submit.
//!
//! A game replays the recordings of its models in every frame, so a call to a recording that the walker saw before can
//! play a template of its earlier walk (see `cache`). The templates are off by default: in Breath of the Wild the
//! recordings with draws never repeat a state within a frame, and the walk was 5.4 ms a frame without templates against
//! 9 ms with them. `NXKIT_WALK_CACHE=1` turns them on and `NXKIT_VERIFY_CACHE=1` walks every recording a second time
//! without them and compares.

mod cache;
mod ext;
mod objects;
mod origin;
mod prof;
mod programs;

use std::path::PathBuf;
use std::sync::Arc;

use nxkit_metal::AddressMap;
use nxkit_nvn::backend::StageKey;
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::mem::GuestMem;
use nxkit_nvn::record::{NO_HANDLE, Record, deferred_handle};

use crate::detail::{
    ClearDetail, CopyDetail, DispatchDetail, DrawArgs, DrawDetail, FrameDetail, ProgramRef, Stage,
    TextureBind, UboDetail,
};
use crate::draws::FastMap;
use objects::{Blends, Raster, StencilValues};
use programs::Programs;

pub(crate) use cache::Invalidations;

const MAX_CALL_DEPTH: u32 = 8;
const MAX_RECORDS: u32 = 1 << 20;
const MAX_COLOR: u8 = 8;
const TEXTURE_SLOTS: usize = 128;
const STREAMS: usize = 16;
const ATTRIBUTES: usize = 32;
const UBOS: usize = 16;
/// The buffer banks: vertex, fragment, compute, task and mesh.
const BANKS: usize = 5;
/// The stage index of `NVNshaderStage` for compute.
const COMPUTE: usize = 5;
/// The program slots and buffer banks of the NVN2 task and mesh stages. The recorder puts the task and mesh stages
/// (6 and 7) into the slots of the tessellation stages (3 and 4).
const TASK: usize = 3;
const MESH: usize = 4;
/// The texture bank that the task and mesh stages share with the other stages that are not vertex, fragment or
/// compute.
const MESH_TEXTURES: usize = 3;

/// The state of one queue between its draws.
#[derive(Clone, PartialEq)]
pub(super) struct State {
    program: [u64; 6],
    vertex_buffer: [(u64, u64); STREAMS],
    ubo: [[(u64, u32); UBOS]; BANKS],
    sbuf: [[(u64, u32); UBOS]; BANKS],
    attribute: [Option<(u32, u32, u32)>; ATTRIBUTES],
    stream: [(u32, u32); STREAMS],
    blends: Blends,
    masks: [u8; 8],
    depth_stencil: [u32; 2],
    stencil: StencilValues,
    raster: Raster,
    viewport: [f32; 4],
    viewport_swizzle: [u32; 4],
    /// The device origin at submit and the queue origin captured at queue creation.
    origins: [bool; 2],
    depth_range: [f32; 2],
    scissor: Option<[u32; 4]>,
    blend_color: [f32; 4],
    texture: [u64; TEXTURE_SLOTS],
    /// The image handles by slot, in the bank layout of `texture`.
    image: [u64; TEXTURE_SLOTS],
    /// `SetCopyRowStride` and `SetCopyImageStride` of the command buffer. 0 is tightly packed.
    copy_strides: [u64; 2],
    /// The push constants of the graphics and the compute stages (bank 24, 0x100 bytes each), shared until changed.
    push: [Option<std::sync::Arc<[u8]>>; 2],
}

impl Default for State {
    fn default() -> Self {
        Self {
            program: [0; 6],
            vertex_buffer: [(0, 0); STREAMS],
            ubo: [[(0, 0); UBOS]; BANKS],
            sbuf: [[(0, 0); UBOS]; BANKS],
            attribute: [None; ATTRIBUTES],
            stream: [(0, 0); STREAMS],
            blends: Blends::default(),
            masks: [0xf; 8],
            depth_stencil: [0b10, 0],
            stencil: StencilValues::default(),
            raster: Raster::default(),
            viewport: [0.0; 4],
            viewport_swizzle: origin::IDENTITY,
            origins: [true; 2],
            depth_range: [0.0, 1.0],
            scissor: None,
            blend_color: [0.0; 4],
            texture: [NO_HANDLE; TEXTURE_SLOTS],
            image: [NO_HANDLE; TEXTURE_SLOTS],
            copy_strides: [0; 2],
            push: [None, None],
        }
    }
}

/// What the walker did since the last [`Walker::take_counters`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WalkCounters {
    /// `submit` calls.
    pub(crate) submits: u32,
    /// Recordings handed to `submit`.
    pub(crate) recordings: u32,
    /// Records read, calls included.
    pub(crate) records: u64,
    /// Draws and dispatches that the walk produced.
    pub(crate) draws: u32,
    pub(crate) dispatches: u32,
    /// Nanoseconds that `submit` took.
    pub(crate) ns: u64,
    /// Nested recordings that a template played instead of a walk.
    pub(crate) walk_hits: u32,
    /// Nested recordings that were walked, and gave a template unless they were cut by a limit.
    pub(crate) walk_misses: u32,
    /// Draws that came from a template, without a walk.
    pub(crate) draws_from_template: u32,
    /// Handles whose templates were dropped because the game wrote their memory.
    pub(crate) invalidations: u32,
    /// Templates dropped because a handle had too many.
    pub(crate) evictions: u32,
    /// Nested recordings walked without a template because their handle is cold (see `Cache::cold`).
    pub(crate) cold_walks: u32,
    /// Walks whose template result differed from the uncached walk (only with the verification on).
    pub(crate) verify_mismatches: u32,
}

/// A uniform buffer range: its GPU address and size.
type SnapKey = (u64, u32);

/// Reads recordings into the detail of the frame that is being built.
pub(crate) struct Walker {
    counters: WalkCounters,
    programs: Programs,
    queues: FastMap<u64, State>,
    /// The current device origin. Draws capture it when their submit is walked.
    origin_lower_left: bool,
    out: FrameDetail,
    map: AddressMap,
    /// The GPU ranges that `UpdateUniformBuffer` wrote. A draw that binds an overlapping range gets a snapshot.
    updated: Vec<(u64, u64)>,
    /// The last snapshot of each uniform buffer range. An update that overlaps a range drops its snapshot.
    snaps: std::cell::RefCell<FastMap<SnapKey, Option<Arc<[u8]>>>>,
    /// The buffers of snapshots by size, oldest first, kept for reuse (`Walker::snap_buffer`).
    snap_pool: std::cell::RefCell<FastMap<u32, std::collections::VecDeque<Arc<[u8]>>>>,
    /// The newest snapshot of each range in the frame. A range with the same bytes again shares its buffer.
    frame_snaps: std::cell::RefCell<FastMap<SnapKey, Arc<[u8]>>>,
    /// The bytes of a range that is compared with an earlier copy. It keeps its capacity.
    snap_tmp: std::cell::RefCell<Vec<u8>>,
    /// Whether every bound uniform buffer gets a copy (the overlap, not the `nosnap` knob). Read once per submit,
    /// because the knob lookup is a string search.
    copy_all: bool,
    /// Whether the storage buffers of a draw get a copy too (the overlap, not the `nosnapsbuf` knob).
    snap_sbufs: bool,
    prof: prof::Prof,
    /// The bytes of the uniform update that is being written. It keeps its capacity between updates.
    update_bytes: Vec<u8>,
    cache: cache::Cache,
    track: cache::Track,
    invalidations: Arc<Invalidations>,
    scratch_ranges: Vec<(u64, u64)>,
    /// Whether nested recordings play from templates. `NXKIT_WALK_CACHE=1` turns it on.
    cache_on: bool,
    /// Walks each recording again without the cache and compares (`NXKIT_VERIFY_CACHE=1`, and always in tests).
    verify: bool,
    /// Set during the uncached walk of the verification.
    bypass: bool,
    /// The old bytes of the guest memory that `write_update` replaced, while the verification runs.
    undo: Option<Vec<(u64, Vec<u8>)>>,
}

impl Walker {
    pub(crate) fn new(shader_dir: PathBuf, map: AddressMap) -> Self {
        Self {
            counters: WalkCounters::default(),
            programs: Programs::new(shader_dir),
            queues: FastMap::default(),
            origin_lower_left: true,
            out: FrameDetail::default(),
            map,
            updated: Vec::new(),
            snaps: std::cell::RefCell::new(FastMap::default()),
            snap_pool: std::cell::RefCell::new(FastMap::default()),
            frame_snaps: std::cell::RefCell::new(FastMap::default()),
            snap_tmp: std::cell::RefCell::new(Vec::new()),
            copy_all: false,
            snap_sbufs: false,
            prof: prof::Prof::new(),
            update_bytes: Vec::new(),
            cache: cache::Cache::default(),
            track: cache::Track::new(),
            invalidations: Arc::new(Invalidations::default()),
            scratch_ranges: Vec::new(),
            cache_on: cfg!(test) || std::env::var("NXKIT_WALK_CACHE").is_ok_and(|v| v == "1"),
            verify: cfg!(test) || std::env::var("NXKIT_VERIFY_CACHE").is_ok_and(|v| v == "1"),
            bypass: false,
            undo: None,
        }
        .with_knobs()
    }

    /// Reads the switches that the snapshots depend on.
    fn with_knobs(mut self) -> Self {
        self.read_knobs();
        self
    }

    fn read_knobs(&mut self) {
        // The CPU runs ahead of the GPU with the overlap, and with the early fences of `NXKIT_PRIVATE=1`: the bytes of
        // a bound range that is not private are copied when the work is submitted.
        let ahead = crate::overlap::on() || (crate::private::early() && !crate::knob("privhonest"));
        self.copy_all = ahead && !crate::knob("nosnap");
        self.snap_sbufs = ahead && !crate::knob("nosnapsbuf");
    }

    pub(crate) fn program_set(&mut self, program: u64, keys: &[StageKey]) {
        self.programs.set(program, keys);
    }

    pub(crate) fn program_released(&mut self, program: u64) {
        self.programs.release(program);
    }

    /// Sets the current device origin without changing the origin captured by a queue.
    pub(crate) fn device_origin(&mut self, lower_left: bool) {
        self.origin_lower_left = lower_left;
    }

    /// The device origin that a present captures before it goes to the encoder thread.
    pub(crate) fn current_origin(&self) -> bool {
        self.origin_lower_left
    }

    /// Sets the origin captured when this queue was initialized.
    pub(crate) fn queue_origin(&mut self, queue: u64, lower_left: bool) {
        self.queues.entry(queue).or_default().origins[1] = lower_left;
    }

    /// Walks `handles` on `queue`.
    pub(crate) fn submit<M: GuestMem>(
        &mut self,
        mem: &mut M,
        queue: u64,
        handles: &[CommandHandle],
    ) {
        let started = std::time::Instant::now();
        self.drain_invalidations();
        self.read_knobs();
        let records = self.counters.records;
        let (draws, dispatches) = (self.out.draws.len(), self.out.dispatches.len());
        // CPU writes between submits do not have UpdateUniform records to invalidate cached snapshots.
        self.snaps.get_mut().clear();
        let mut st = self.queues.remove(&queue).unwrap_or_default();
        st.origins[0] = self.origin_lower_left;
        for h in handles {
            // The copy strides belong to one command buffer and start again at zero in each recording.
            st.copy_strides = [0; 2];
            self.walk_top(mem, &mut st, h.0);
        }
        self.queues.insert(queue, st);
        let c = &mut self.counters;
        c.submits += 1;
        c.recordings += handles.len() as u32;
        c.draws += (self.out.draws.len() - draws) as u32;
        c.dispatches += (self.out.dispatches.len() - dispatches) as u32;
        let ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        c.ns += ns;
        if self.prof.on() {
            let (records, draws, dispatches) = (
                c.records - records,
                self.out.draws.len() - draws,
                self.out.dispatches.len() - dispatches,
            );
            self.prof.report(records, draws, dispatches, ns);
        }
    }

    /// The counters since the last call.
    pub(crate) fn take_counters(&mut self) -> WalkCounters {
        let mut c = std::mem::take(&mut self.counters);
        c.invalidations = std::mem::take(&mut self.cache.invalidations);
        c.evictions = std::mem::take(&mut self.cache.evictions);
        c
    }

    /// The detail of the commands since the last call. The walker starts the next frame empty.
    pub(crate) fn take(&mut self) -> FrameDetail {
        self.frame_snaps.get_mut().clear();
        std::mem::take(&mut self.out)
    }

    /// Walks the recording at `handle`. A call record plays the recording of its callee, from a template when one fits.
    fn run<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32) {
        if depth > MAX_CALL_DEPTH {
            self.track.truncated();
            return;
        }
        if depth == 0 {
            self.walk_records(mem, st, handle, depth);
        } else {
            self.run_nested(mem, st, handle, depth);
        }
    }

    /// Reads the records from `handle` to the end, and follows jumps and calls.
    fn walk_records<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32) {
        let mut pos = handle;
        for _ in 0..MAX_RECORDS {
            let t = self.prof.start();
            let read = Record::read(&*mem, pos);
            self.prof.lap(&self.prof.read_ns, t);
            let Ok((rec, size)) = read else {
                self.track.span(pos, pos + 8);
                return;
            };
            self.track.span(pos, pos + u64::from(size));
            self.counters.records += 1;
            match rec {
                Record::End => return,
                Record::Jump(to) => {
                    pos = to;
                    continue;
                }
                Record::Call(h) => self.run(mem, st, h, depth + 1),
                Record::Ext(x) => {
                    let t = self.prof.start();
                    self.ext(mem, st, &x);
                    self.prof.lap(&self.prof.apply_ns, t);
                }
                other => {
                    let t = self.prof.start();
                    self.apply(mem, st, other);
                    self.prof.lap(&self.prof.apply_ns, t);
                }
            }
            pos += u64::from(size);
        }
        self.track.truncated();
    }

    fn apply<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, rec: Record) {
        match rec {
            Record::ClearColor { slot } if slot < MAX_COLOR => {
                self.out.clears.push(ClearDetail::default())
            }
            Record::ClearDepthStencil => self.out.clears.push(ClearDetail {
                depth: 1.0,
                ..ClearDetail::default()
            }),
            Record::BindTexture { slot, handle } => {
                if let Some(t) = st.texture.get_mut(usize::from(slot)) {
                    *t = handle;
                    self.track.texture_set(usize::from(slot));
                }
            }
            Record::BindImage { slot, handle } => {
                if let Some(t) = st.image.get_mut(usize::from(slot)) {
                    *t = handle;
                }
                // The template cache does not follow image binds, so a recording that binds one is not a template.
                self.track.truncated();
            }
            Record::BindTexturesDeferred {
                first,
                count,
                src,
                tex_base,
                samp_base,
            } => {
                for k in 0..count {
                    let handle = deferred_handle(&*mem, src, k, tex_base, samp_base)
                        .map_or(0, |(_, _, h)| h);
                    let slot = usize::from(first) + usize::from(k);
                    if let Some(t) = st.texture.get_mut(slot) {
                        *t = handle;
                        let source = cache::Track::source(src, k, tex_base, samp_base, handle);
                        self.track.texture_deferred(slot, source);
                    }
                }
            }
            Record::Copy { .. } | Record::Downsample { .. } => {
                self.out.copies.push(CopyDetail::default())
            }
            Record::Draw => self.out.draws.push(Arc::new(empty_draw())),
            Record::Dispatch => {
                let d = self.dispatch(&*mem, st, ([0; 3], None));
                self.track
                    .patches_of(&d.textures, self.out.dispatches.len(), true);
                self.out.dispatches.push(d);
            }
            Record::Ext(x) => self.ext(mem, st, &x),
            _ => {}
        }
    }

    fn dispatch<M: GuestMem>(
        &mut self,
        mem: &M,
        st: &State,
        (groups, indirect): ([u32; 3], Option<u64>),
    ) -> DispatchDetail {
        self.track.program(COMPUTE, st.program[COMPUTE]);
        self.track.read(&cache::DISPATCH_READS);
        let program = self
            .programs
            .find(st.program[COMPUTE], programs::Kind::Compute)
            .map_or_else(String::new, |k| k.to_string());
        let images = images(st, 2, Stage::Compute);
        if !images.is_empty() {
            // Inherited images are not a template key.
            self.track.truncated();
        }
        DispatchDetail {
            program,
            push: st.push[1].clone(),
            groups,
            indirect,
            ubos: self.ubos(mem, st, 2, Stage::Compute),
            sbufs: sbufs(st, 2, Stage::Compute),
            textures: textures(st, 2, Stage::Compute),
            images,
        }
    }
}

/// The empty shader key of a draw that cannot run, shared so that such a draw allocates nothing.
fn empty_key() -> Arc<str> {
    static EMPTY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::from("")).clone()
}

/// The detail of a draw that cannot run. It keeps the ordinals of the draws after it.
pub(crate) fn empty_draw() -> DrawDetail {
    DrawDetail {
        push: None,
        program: ProgramRef {
            vertex: empty_key(),
            fragment: None,
            task: None,
            mesh: None,
        },
        blend: Default::default(),
        write_masks: [0xf; 8],
        depth_stencil: Default::default(),
        raster: Default::default(),
        topology: 4,
        attributes: Vec::new(),
        streams: Vec::new(),
        args: DrawArgs::Arrays {
            first: 0,
            count: 0,
            instances: 0,
            base_instance: 0,
        },
        ubos: Vec::new(),
        sbufs: Vec::new(),
        textures: Vec::new(),
        images: Vec::new(),
        dynamic: Default::default(),
    }
}

/// The number of bound buffers in a bank of `(address, size)` slots.
pub(super) fn bound_count(bank: &[(u64, u32); UBOS]) -> usize {
    bank.iter().filter(|(a, s)| *a != 0 && *s != 0).count()
}

/// Adds the storage buffers of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh) that are bound to `out`. The
/// shader reads and writes them in place, so they carry no snapshot.
pub(super) fn sbufs_into(st: &State, bank: usize, stage: Stage, out: &mut Vec<UboDetail>) {
    out.extend(
        st.sbuf[bank]
            .iter()
            .enumerate()
            .filter(|(_, (a, s))| *a != 0 && *s != 0)
            .map(|(i, (a, s))| UboDetail {
                stage,
                slot: i as u32,
                address: *a,
                size: *s,
                snapshot: None,
            }),
    );
}

/// The storage buffers of the bank (0 vertex, 1 fragment, 2 compute, 3 task, 4 mesh) that are bound.
pub(super) fn sbufs(st: &State, bank: usize, stage: Stage) -> Vec<UboDetail> {
    let mut out = Vec::with_capacity(bound_count(&st.sbuf[bank]));
    sbufs_into(st, bank, stage, &mut out);
    out
}

/// The 32 texture handles of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages).
fn bank(st: &State, bank: usize) -> &[u64] {
    st.texture.get(bank * 32..bank * 32 + 32).unwrap_or(&[])
}

/// The number of texture handles of the bank that are bound.
pub(super) fn texture_count(st: &State, bank_index: usize) -> usize {
    bank(st, bank_index)
        .iter()
        .filter(|h| **h != NO_HANDLE)
        .count()
}

/// Adds the texture handles of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages) that are bound to `out`.
pub(super) fn textures_into(
    st: &State,
    bank_index: usize,
    stage: Stage,
    out: &mut Vec<TextureBind>,
) {
    out.extend(
        bank(st, bank_index)
            .iter()
            .enumerate()
            .filter(|(_, h)| **h != NO_HANDLE)
            .map(|(i, handle)| TextureBind {
                stage,
                slot: i as u32,
                handle: *handle,
            }),
    );
}

/// Adds the image handles of the bank (same layout as the textures) that are bound to `out`.
pub(super) fn images_into(st: &State, bank_index: usize, stage: Stage, out: &mut Vec<TextureBind>) {
    let slots = st
        .image
        .get(bank_index * 32..bank_index * 32 + 32)
        .unwrap_or(&[]);
    out.extend(
        slots
            .iter()
            .enumerate()
            .filter(|(_, h)| **h != NO_HANDLE)
            .map(|(i, handle)| TextureBind {
                stage,
                slot: i as u32,
                handle: *handle,
            }),
    );
}

/// The image handles of the bank that are bound.
fn images(st: &State, bank_index: usize, stage: Stage) -> Vec<TextureBind> {
    let mut out = Vec::new();
    images_into(st, bank_index, stage, &mut out);
    out
}

/// The texture handles of the bank (0 vertex, 1 fragment, 2 compute, 3 the other stages) that are bound.
fn textures(st: &State, bank_index: usize, stage: Stage) -> Vec<TextureBind> {
    let mut out = Vec::with_capacity(texture_count(st, bank_index));
    textures_into(st, bank_index, stage, &mut out);
    out
}
