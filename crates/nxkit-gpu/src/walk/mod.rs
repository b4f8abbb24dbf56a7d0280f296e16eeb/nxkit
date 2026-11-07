//! The walk of the recordings that a game submits, for the detail that the frame does not carry.
//!
//! The front end of NVN reads a recording into a `Frame`, which holds only what the pass analysis needs. The encoder
//! also needs what each draw binds. The walker reads the same recordings a second time and follows the bind records
//! (programs, buffers, vertex layout, state objects, viewport) to fill a [`FrameDetail`].
//!
//! The ordinals agree with the decoder of the front end. Each `Draw`, each `Dispatch`, each clear of a colour slot
//! below 8 and each depth stencil clear gives one entry, in submit order. A call record plays its recording with the
//! state of the caller, and what the callee sets stays set. The state of a queue stays across submits.
//!
//! A walk runs when the queue submits, because the game may write the command memory again after the submit.

mod ext;
mod objects;
mod programs;

use std::collections::HashMap;
use std::path::PathBuf;

use nxkit_metal::AddressMap;
use nxkit_nvn::backend::StageKey;
use nxkit_nvn::cmdbuf::CommandHandle;
use nxkit_nvn::mem::GuestMem;
use nxkit_nvn::record::{NO_HANDLE, Record};

use crate::detail::{ClearDetail, DispatchDetail, DrawArgs, DrawDetail, FrameDetail, ProgramRef};
use objects::{Blends, Raster, StencilValues};
use programs::Programs;

const MAX_CALL_DEPTH: u32 = 8;
const MAX_RECORDS: u32 = 1 << 20;
const MAX_COLOR: u8 = 8;
const TEXTURE_SLOTS: usize = 128;
const STREAMS: usize = 16;
const ATTRIBUTES: usize = 32;
const UBOS: usize = 16;
/// The stage index of `NVNshaderStage` for compute.
const COMPUTE: usize = 5;

/// The state of one queue between its draws.
pub(super) struct State {
    program: [u64; 6],
    vertex_buffer: [(u64, u64); STREAMS],
    ubo: [[(u64, u32); UBOS]; 3],
    attribute: [Option<(u32, u32, u32)>; ATTRIBUTES],
    stream: [(u32, u32); STREAMS],
    blends: Blends,
    masks: [u8; 8],
    depth_stencil: [u32; 2],
    stencil: StencilValues,
    raster: Raster,
    viewport: [f32; 4],
    depth_range: [f32; 2],
    scissor: Option<[u32; 4]>,
    blend_color: [f32; 4],
    texture: [u64; TEXTURE_SLOTS],
}

impl Default for State {
    fn default() -> Self {
        Self {
            program: [0; 6],
            vertex_buffer: [(0, 0); STREAMS],
            ubo: [[(0, 0); UBOS]; 3],
            attribute: [None; ATTRIBUTES],
            stream: [(0, 0); STREAMS],
            blends: Blends::default(),
            masks: [0xf; 8],
            depth_stencil: [0b10, 0],
            stencil: StencilValues::default(),
            raster: Raster::default(),
            viewport: [0.0; 4],
            depth_range: [0.0, 1.0],
            scissor: None,
            blend_color: [0.0; 4],
            texture: [NO_HANDLE; TEXTURE_SLOTS],
        }
    }
}

/// Reads recordings into the detail of the frame that is being built.
pub(crate) struct Walker {
    programs: Programs,
    queues: HashMap<u64, State>,
    out: FrameDetail,
    map: AddressMap,
    /// The GPU ranges that `UpdateUniformBuffer` wrote. A draw that binds an overlapping range gets a snapshot.
    updated: Vec<(u64, u64)>,
}

impl Walker {
    pub(crate) fn new(shader_dir: PathBuf, map: AddressMap) -> Self {
        Self {
            programs: Programs::new(shader_dir),
            queues: HashMap::new(),
            out: FrameDetail::default(),
            map,
            updated: Vec::new(),
        }
    }

    pub(crate) fn program_set(&mut self, program: u64, keys: &[StageKey]) {
        self.programs.set(program, keys);
    }

    pub(crate) fn program_released(&mut self, program: u64) {
        self.programs.release(program);
    }

    /// Walks `handles` on `queue`.
    pub(crate) fn submit<M: GuestMem>(&mut self, mem: &mut M, queue: u64, handles: &[CommandHandle]) {
        let mut st = self.queues.remove(&queue).unwrap_or_default();
        for h in handles {
            self.run(mem, &mut st, h.0, 0);
        }
        self.queues.insert(queue, st);
    }

    /// The detail of the commands since the last call. The walker starts the next frame empty.
    pub(crate) fn take(&mut self) -> FrameDetail {
        std::mem::take(&mut self.out)
    }

    fn run<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, handle: u64, depth: u32) {
        if depth > MAX_CALL_DEPTH {
            return;
        }
        let mut pos = handle;
        for _ in 0..MAX_RECORDS {
            let Ok((rec, size)) = Record::read(&*mem, pos) else {
                return;
            };
            match rec {
                Record::End => return,
                Record::Jump(to) => {
                    pos = to;
                    continue;
                }
                Record::Call(h) => self.run(mem, st, h, depth + 1),
                other => self.apply(mem, st, other),
            }
            pos += u64::from(size);
        }
    }

    fn apply<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, rec: Record) {
        match rec {
            Record::ClearColor { slot } if slot < MAX_COLOR => self.out.clears.push(ClearDetail::default()),
            Record::ClearDepthStencil => self.out.clears.push(ClearDetail { depth: 1.0, ..ClearDetail::default() }),
            Record::BindTexture { slot, handle } => {
                if let Some(t) = st.texture.get_mut(usize::from(slot)) {
                    *t = handle;
                }
            }
            Record::Draw => self.out.draws.push(empty_draw()),
            Record::Dispatch => {
                let d = self.dispatch(&*mem, st, [0; 3]);
                self.out.dispatches.push(d);
            },
            Record::Ext(x) => self.ext(mem, st, &x),
            _ => {}
        }
    }

    fn dispatch<M: GuestMem>(&self, mem: &M, st: &State, groups: [u32; 3]) -> DispatchDetail {
        let program = self.programs.find(st.program[COMPUTE], programs::Kind::Compute).unwrap_or_default().to_owned();
        DispatchDetail {
            program,
            groups,
            ubos: self.ubos(mem, st, 2, crate::detail::Stage::Compute),
            textures: textures(st, 2, crate::detail::Stage::Compute),
        }
    }
}

/// The detail of a draw that cannot run. It keeps the ordinals of the draws after it.
fn empty_draw() -> DrawDetail {
    DrawDetail {
        program: ProgramRef { vertex: String::new(), fragment: None },
        blend: Default::default(),
        write_masks: [0xf; 8],
        depth_stencil: Default::default(),
        raster: Default::default(),
        topology: 4,
        attributes: Vec::new(),
        streams: Vec::new(),
        args: DrawArgs::Arrays { first: 0, count: 0, instances: 0, base_instance: 0 },
        ubos: Vec::new(),
        textures: Vec::new(),
        dynamic: Default::default(),
    }
}

/// The texture handles of the bank (0 vertex, 1 fragment, 2 compute) that are bound.
fn textures(st: &State, bank: usize, stage: crate::detail::Stage) -> Vec<crate::detail::TextureBind> {
    (0..32)
        .filter_map(|i| {
            let handle = st.texture[bank * 32 + i];
            (handle != NO_HANDLE).then_some(crate::detail::TextureBind { stage, slot: i as u32, handle })
        })
        .collect()
}
