//! The extension records: what each of them changes in the state of the walker, and the detail of draws and clears.

use std::sync::Arc;

use nxkit_nvn::ext::{DrawKind, Ext, ExtCode, Setting, StateKind};
use nxkit_nvn::mem::GuestMem;

use super::cache::{self, sc};
use super::objects::{self, Blends, StencilValues, write_masks};
use super::programs::Kind;
use super::{
    ATTRIBUTES, MAX_COLOR, MESH, MESH_TEXTURES, STREAMS, State, TASK, UBOS, Walker, bound_count, empty_draw,
    images_into, sbufs_into, texture_count, textures_into,
};
use crate::detail::{
    AttributeDetail, ClearDetail, CopyDetail, CopyKind, DrawArgs, DrawDetail, DynamicDetail, ProgramRef, Stage,
    StreamDetail, UboDetail,
};

/// The `NVNshaderStage` values that have uniform buffers here, as the slot in `State::ubo`. The recorder writes the
/// task and mesh stages as 3 and 4, the stages of tessellation, which the encoder does not run.
fn ubo_stage(stage: u16) -> Option<usize> {
    match stage {
        0 => Some(0),
        1 => Some(1),
        5 => Some(2),
        3 => Some(TASK),
        4 => Some(MESH),
        _ => None,
    }
}

/// The `NVNshaderStageBits` of the NVN2 task and mesh stages and the program slots they go to.
const MESH_STAGE_BITS: [(u32, usize); 2] = [(6, TASK), (7, MESH)];

fn f(x: &Ext, i: usize) -> f32 {
    f32::from_bits(x.word(i))
}

/// The `nvnCommandBufferClearColor` value of a clear. Integer clears give their value as a number.
fn clear_value(kind: u16, bits: u32) -> f32 {
    match kind {
        1 => bits as i32 as f32,
        2 => bits as f32,
        _ => f32::from_bits(bits),
    }
}

/// Bytes of a push constant block (the translator's `PUSH_SIZE`).
const PUSH_BYTES: usize = 0x100;

impl Walker {
    pub(super) fn ext<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, x: &Ext) {
        match x.code {
            ExtCode::ReportCounter if matches!(x.word(0), 0 | 0x10) => {
                self.out.reports.push(crate::detail::CounterReport { kind: x.word(0), addr: x.word64(1) });
            }
            ExtCode::Program => {
                let (program, bits) = (x.word64(0), x.word(2));
                for (s, p) in st.program.iter_mut().enumerate() {
                    if bits >> s & 1 != 0 {
                        *p = program;
                        self.track.write(cache::program_bit(s));
                    }
                }
                for (bit, s) in MESH_STAGE_BITS {
                    if bits >> bit & 1 != 0 {
                        st.program[s] = program;
                        self.track.write(cache::program_bit(s));
                    }
                }
            }
            ExtCode::VertexBuffer => {
                if let Some(v) = st.vertex_buffer.get_mut(usize::from(x.arg)) {
                    *v = (x.word64(0), x.word64(2));
                    self.track.write(cache::vertex_buffer_bit(usize::from(x.arg)));
                }
            }
            ExtCode::UniformBuffer => {
                let slot = usize::from(x.arg & 0xff);
                if let (Some(stage), true) = (ubo_stage(x.arg >> 8), slot < UBOS) {
                    st.ubo[stage][slot] =
                        (x.word64(0), x.word64(2).min(u64::from(u32::MAX)) as u32);
                    self.track.write(cache::ubo_bit(stage, slot));
                }
            }
            ExtCode::StorageBuffer => {
                let slot = usize::from(x.arg & 0xff);
                if let (Some(stage), true) = (ubo_stage(x.arg >> 8), slot < UBOS) {
                    st.sbuf[stage][slot] =
                        (x.word64(0), x.word64(2).min(u64::from(u32::MAX)) as u32);
                    self.track.write(cache::sbo_bit(stage, slot));
                }
            }
            ExtCode::Viewport if x.arg == 0 => {
                st.viewport = [f(x, 0), f(x, 1), f(x, 2), f(x, 3)];
                self.track.write(cache::scalar_bit(sc::VIEWPORT));
            }
            ExtCode::DepthRange if x.arg == 0 => {
                st.depth_range = [f(x, 0), f(x, 1)];
                self.track.write(cache::scalar_bit(sc::DEPTH_RANGE));
            }
            ExtCode::Scissor if x.arg == 0 => {
                let v = |i: usize| x.word(i) as i32;
                st.scissor = Some([
                    v(0).max(0) as u32,
                    v(1).max(0) as u32,
                    v(2).max(0) as u32,
                    v(3).max(0) as u32,
                ]);
                self.track.write(cache::scalar_bit(sc::SCISSOR));
            }
            ExtCode::State => self.state_object(st, x),
            ExtCode::TargetView if crate::diag("target") => {
                static VIEWS: std::sync::Mutex<Vec<(u16, [u32; 4])>> = std::sync::Mutex::new(Vec::new());
                let value = (x.arg, [x.word(0), x.word(1), x.word(2), x.word(3)]);
                if let Ok(mut views) = VIEWS.lock() {
                    if views.len() < 80 && !views.contains(&value) {
                        eprintln!("DIAG target view {value:x?}");
                        views.push(value);
                    }
                }
            }
            ExtCode::VertexState => self.vertex_state(st, x),
            ExtCode::Setting => self.setting(st, x),
            ExtCode::ClearBuffer
            | ExtCode::CopyBufferToBuffer
            | ExtCode::DownsampleView => self.out.copies.push(CopyDetail::default()),
            ExtCode::ClearTexture => self.out.copies.push(clear_detail(x)),
            ExtCode::CopyBufferToTexture | ExtCode::CopyTextureToBuffer | ExtCode::CopyTextureToTexture => {
                self.track.read(&cache::COPY_READS);
                self.out.copies.push(copy_detail(st, x));
            }
            // Push constants: the first word is the byte offset, then `arg >> 1` bytes; bit 0 of `arg` is compute.
            ExtCode::Push => {
                let (compute, len) = (x.arg & 1 != 0, usize::from(x.arg >> 1));
                let w = x.words();
                let off = w.first().copied().unwrap_or(0) as usize;
                let bytes: Vec<u8> = w.iter().skip(1).flat_map(|v| v.to_le_bytes()).take(len).collect();
                let slot = &mut st.push[usize::from(compute)];
                let mut block: Vec<u8> = slot.as_deref().map_or_else(|| vec![0; PUSH_BYTES], <[u8]>::to_vec);
                if let Some(dst) = block.get_mut(off..(off + bytes.len()).min(PUSH_BYTES)) {
                    let n = dst.len();
                    dst.copy_from_slice(&bytes[..n]);
                }
                *slot = Some(block.into());
            }
            ExtCode::UpdateUniform => {
                self.out.copies.push(CopyDetail::default());
                self.update_uniform(mem, x);
            }
            ExtCode::ClearColor => {
                let slot = (x.arg & 0xff) as u8;
                if slot < MAX_COLOR {
                    let kind = x.arg >> 8;
                    let c = |i: usize| clear_value(kind, x.word(1 + i));
                    self.out.clears.push(ClearDetail {
                        color: [c(0), c(1), c(2), c(3)],
                        ..ClearDetail::default()
                    });
                }
            }
            ExtCode::ClearDepthStencil => {
                if crate::diag("clear") {
                    static SHOWN: std::sync::atomic::AtomicU32 =
                        std::sync::atomic::AtomicU32::new(0);
                    if SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1000 < 20 {
                        eprintln!(
                            "DIAG clear depth {} write {} stencil {} mask {:#x} viewport {:?}",
                            f(x, 0),
                            x.word(1),
                            x.word(2),
                            x.word(3),
                            st.viewport
                        );
                    }
                }
                self.out.clears.push(ClearDetail {
                    depth: f(x, 0),
                    stencil: x.word(2),
                    ..ClearDetail::default()
                });
            }
            ExtCode::Draw => {
                let t = self.prof.start();
                let d = self.draw(&*mem, st, x);
                self.prof.lap(&self.prof.detail_ns, t);
                self.track.patches_of(&d.textures, self.out.draws.len(), false);
                let t = self.prof.start();
                self.prof.draw_built(&d);
                self.out.draws.push(Arc::new(d));
                self.prof.lap(&self.prof.alloc_draw_ns, t);
            }
            ExtCode::Dispatch => {
                let groups = if x.arg == 0 {
                    [x.word(0), x.word(1), x.word(2)]
                } else {
                    [0; 3]
                };
                let t = self.prof.start();
                let d = self.dispatch(&*mem, st, groups);
                self.prof.lap(&self.prof.detail_ns, t);
                self.track.patches_of(&d.textures, self.out.dispatches.len(), true);
                self.out.dispatches.push(d);
            }
            _ => {}
        }
    }

    fn state_object(&mut self, st: &mut State, x: &Ext) {
        let Some(kind) = StateKind::from_u8(x.arg as u8) else {
            return;
        };
        let w = x.words();
        match kind {
            StateKind::Blend => {
                st.blends.set_blend(w);
                self.track.write(cache::blend_bit(Blends::target(w)));
            }
            StateKind::Color => {
                st.blends.set_color(w);
                self.track.write(cache::scalar_bit(sc::BLEND_ENABLE));
            }
            StateKind::ChannelMask => {
                st.masks = write_masks(w);
                self.track.write(cache::scalar_bit(sc::MASKS));
            }
            StateKind::Multisample => {
                st.raster.set_multisample(w);
                self.track.write(cache::scalar_bit(sc::MULTISAMPLE));
            }
            StateKind::Polygon => {
                st.raster.set_polygon(w);
                self.track.write(cache::scalar_bit(sc::POLYGON));
            }
            StateKind::DepthStencil => {
                st.depth_stencil = [x.word(0), x.word(1)];
                self.track.write(cache::scalar_bit(sc::DEPTH_STENCIL));
            }
        }
    }

    /// The command writes its bytes into the buffer when the GPU runs it. The walker writes them now and remembers the
    /// range, so that the draws that bind it get the bytes they saw.
    fn update_uniform<M: GuestMem>(&mut self, mem: &mut M, x: &Ext) {
        self.track.update(x, self.out.draws.len(), self.out.dispatches.len());
        let mut bytes = std::mem::take(&mut self.update_bytes);
        self.write_update(mem, x, &mut bytes);
        self.update_bytes = bytes;
    }

    /// Writes the bytes of an update record through `bytes` and records the range.
    pub(super) fn write_update<M: GuestMem>(&mut self, mem: &mut M, x: &Ext, bytes: &mut Vec<u8>) {
        let (buffer, offset, len) = (x.word64(0), x.word64(4), usize::from(x.arg));
        let at = buffer.saturating_add(offset);
        bytes.clear();
        bytes.extend(
            x.words()
                .get(6..)
                .unwrap_or(&[])
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .take(len),
        );
        let Ok(host) = self.map.host_address(at, bytes.len() as u64) else {
            return;
        };
        if let Some(log) = &mut self.undo {
            let mut old = vec![0u8; bytes.len()];
            if mem.read(host, &mut old).is_ok() {
                log.push((host, old));
            }
        }
        if mem.write(host, bytes).is_err() {
            return;
        }
        let end = at + bytes.len() as u64;
        self.snaps
            .get_mut()
            .retain(|(a, s), _| !(*a < end && at < *a + u64::from(*s)));
        match self.updated.last_mut() {
            Some((s, e)) if at <= *e && *s <= end => {
                *s = (*s).min(at);
                *e = (*e).max(end);
            }
            _ => {
                if self.updated.len() >= 512 {
                    self.updated.remove(0);
                }
                self.updated.push((at, end));
            }
        }
    }

    /// The uniform buffers of the bank (0 vertex, 1 fragment, 2 compute) that are bound, added to `out`.
    pub(super) fn ubos_into<M: GuestMem>(
        &self,
        mem: &M,
        st: &State,
        slot: usize,
        stage: Stage,
        out: &mut Vec<UboDetail>,
    ) {
        out.extend(
            st.ubo[slot]
                .iter()
                .enumerate()
                .filter(|(_, (a, s))| *a != 0 && *s != 0)
                .map(|(i, (a, s))| UboDetail {
                    stage,
                    slot: i as u32,
                    address: *a,
                    size: *s,
                    snapshot: self.snapshot(mem, *a, *s),
                }),
        );
    }

    pub(super) fn ubos<M: GuestMem>(
        &self,
        mem: &M,
        st: &State,
        slot: usize,
        stage: Stage,
    ) -> Vec<UboDetail> {
        let mut out = Vec::with_capacity(bound_count(&st.ubo[slot]));
        self.ubos_into(mem, st, slot, stage, &mut out);
        out
    }

    /// The bytes of a buffer that an update touched, as the guest memory holds them now.
    pub(super) fn snapshot<M: GuestMem>(&self, mem: &M, addr: u64, size: u32) -> Option<Arc<[u8]>> {
        let t = self.prof.start();
        let r = self.snapshot_of(mem, addr, size);
        self.prof.lap(&self.prof.snap_ns, t);
        r
    }

    fn snapshot_of<M: GuestMem>(&self, mem: &M, addr: u64, size: u32) -> Option<Arc<[u8]>> {
        let p = &self.prof;
        p.count(&p.snaps, 1);
        let key = (addr, size);
        if let Some(snapshot) = self.snaps.borrow().get(&key) {
            p.count(&p.snap_hits, 1);
            return snapshot.clone();
        }
        let end = addr + u64::from(size);
        // A partial update does not reduce the bound range. Its unchanged tail can contain constants that the
        // shader still reads. Copying only the updated prefix makes those reads escape the scratch allocation.
        // With the overlap on, every bound block gets its copy: the CPU may write it for the next frame while the GPU
        // still reads this one.
        if !self.copy_all && !self.updated.iter().any(|(s, e)| addr < *e && *s < end) {
            // Most bound blocks have no inline updates. Remember that result as well: scanning the update
            // history for every UBO of every draw otherwise dominates the command walker on large frames.
            self.snaps.borrow_mut().insert(key, None);
            p.count(&p.snap_none, 1);
            return None;
        }
        let host = self.map.host_address(addr, u64::from(size)).ok()?;
        let earlier = self.frame_snaps.borrow().get(&key).cloned();
        let snap = match earlier {
            Some(earlier) => {
                // The range was copied before in this frame (another submit). Read the bytes aside: when they are the
                // same, the draws share the buffer and the upload of the frame sees one block.
                let mut tmp = self.snap_tmp.borrow_mut();
                tmp.resize(size as usize, 0);
                mem.read(host, &mut tmp[..]).ok()?;
                if tmp[..] == earlier[..] {
                    p.count(&p.snap_shared, 1);
                    self.snaps.borrow_mut().insert(key, Some(earlier.clone()));
                    return Some(earlier);
                }
                let mut buffer = self.snap_buffer(size);
                Arc::get_mut(&mut buffer)?.copy_from_slice(&tmp);
                buffer
            }
            None => {
                let mut buffer = self.snap_buffer(size);
                mem.read(host, Arc::get_mut(&mut buffer)?).ok()?;
                buffer
            }
        };
        p.count(&p.snap_copied, 1);
        p.count(&p.snap_bytes, u64::from(size));
        self.keep_snap_buffer(&snap);
        self.frame_snaps.borrow_mut().insert(key, snap.clone());
        self.snaps.borrow_mut().insert(key, Some(snap.clone()));
        Some(snap)
    }

    /// A buffer of `size` bytes for a snapshot that nothing else holds. The buffers of snapshots whose frames were
    /// encoded and dropped come back, so a large uniform buffer costs a copy, not a fresh allocation and its page faults.
    /// The pool of a size is a queue in the order of issue. Frames are dropped in that order, so the oldest buffer is
    /// the one that is free first, and one look at the front decides.
    fn snap_buffer(&self, size: u32) -> Arc<[u8]> {
        {
            let mut pool = self.snap_pool.borrow_mut();
            let ring = pool.entry(size).or_default();
            if ring.front().is_some_and(|b| Arc::strong_count(b) == 1 && Arc::weak_count(b) == 0)
                && let Some(b) = ring.pop_front()
            {
                return b;
            }
        }
        let p = &self.prof;
        let t = p.start();
        let b: Arc<[u8]> = vec![0u8; size as usize].into();
        p.lap(&p.alloc_snap_ns, t);
        p.count(&p.snap_fresh, 1);
        b
    }

    /// Keeps a reference to a snapshot buffer, so it comes back once the frame that holds it is dropped.
    fn keep_snap_buffer(&self, b: &Arc<[u8]>) {
        const KEEP: usize = 2048;
        let mut pool = self.snap_pool.borrow_mut();
        let ring = pool.entry(b.len() as u32).or_default();
        if ring.len() < KEEP {
            ring.push_back(b.clone());
        }
    }

    fn draw<M: GuestMem>(&mut self, mem: &M, st: &State, x: &Ext) -> DrawDetail {
        if let Some(args) = mesh_args(x) {
            return self.mesh_draw(mem, st, args);
        }
        let mut d = empty_draw();
        d.push = st.push[0].clone();
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let show =
            || crate::diag("prog") && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40;
        let Some((topology, args)) = draw_args(x) else {
            nxkit_nvn::diag::note(
                nxkit_nvn::diag::Reason::DrawNoArgs,
                format_args!("draw kind {} program {:x?}", x.arg, st.program),
            );
            static K: [std::sync::atomic::AtomicU32; 16] = [const { std::sync::atomic::AtomicU32::new(0) }; 16];
            let n = K[usize::from(x.arg) & 15].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if crate::diag("indirect") && n.is_power_of_two() {
                eprintln!("DIAG indirect kind {} dropped {} times, program {:x?}", x.arg, n + 1, st.program);
            }
            return d;
        };
        self.prof.banks(st);
        self.track.program(0, st.program[0]);
        let Some(vertex) = self.programs.find(st.program[0], Kind::Vertex) else {
            let what = format_args!("programs {:x?}, stages of [0]: {}", st.program, self.programs.describe(st.program[0]));
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::DrawNoVertex, what);
            if show() {
                eprintln!("DIAG draw: no vertex stage, {what}");
            }
            return d;
        };
        self.track.program(1, st.program[1]);
        self.track.read(&cache::DRAW_READS);
        d.program = ProgramRef {
            vertex: vertex.clone(),
            fragment: self.programs.find(st.program[1], Kind::Fragment).cloned(),
            task: None,
            mesh: None,
        };
        d.topology = topology;
        d.args = args;
        fixed_function(st, &mut d);
        d.attributes.reserve_exact(st.attribute.iter().flatten().count());
        let mut used = 0u32;
        for (location, a) in st.attribute.iter().enumerate().take(ATTRIBUTES) {
            if let Some((format, offset, stream)) = *a {
                if stream < STREAMS as u32 {
                    used |= 1 << stream;
                }
                d.attributes.push(AttributeDetail {
                    location: location as u32,
                    format,
                    offset,
                    stream,
                });
            }
        }
        d.streams.reserve_exact(used.count_ones() as usize);
        for index in 0..STREAMS {
            let (address, size) = st.vertex_buffer[index];
            if used >> index & 1 != 0 {
                self.track.read_bit(cache::vertex_buffer_bit(index));
            }
            if used >> index & 1 != 0 && address != 0 {
                self.track.read_bit(cache::stream_bit(index));
                let (stride, divisor) = st.stream[index];
                d.streams.push(StreamDetail {
                    index: index as u32,
                    address,
                    size,
                    stride,
                    divisor,
                });
            }
        }
        d.ubos.reserve_exact(bound_count(&st.ubo[0]) + bound_count(&st.ubo[1]));
        self.ubos_into(mem, st, 0, Stage::Vertex, &mut d.ubos);
        self.ubos_into(mem, st, 1, Stage::Fragment, &mut d.ubos);
        d.sbufs.reserve_exact(bound_count(&st.sbuf[0]) + bound_count(&st.sbuf[1]));
        sbufs_into(st, 0, Stage::Vertex, &mut d.sbufs);
        sbufs_into(st, 1, Stage::Fragment, &mut d.sbufs);
        // With the overlap on, the storage buffers of a draw get a copy too: the CPU writes some of them for the next
        // frame while the GPU still draws this one (Breath of the Wild's dialog text). The encoder uses the copy only
        // for a binding that the shader does not write. Dispatches keep the buffer itself.
        if self.snap_sbufs {
            for b in &mut d.sbufs {
                b.snapshot = self.snapshot(mem, b.address, b.size);
            }
        }
        d.textures.reserve_exact(texture_count(st, 0) + texture_count(st, 1));
        textures_into(st, 0, Stage::Vertex, &mut d.textures);
        textures_into(st, 1, Stage::Fragment, &mut d.textures);
        images_into(st, 0, Stage::Vertex, &mut d.images);
        images_into(st, 1, Stage::Fragment, &mut d.images);
        if !d.images.is_empty() {
            self.track.truncated();
        }
        d
    }

    /// The detail of a task and mesh draw. The program of the mesh stage gives the task and mesh shaders. A game binds
    /// one program for all stages, so the walker looks in the slots of the mesh, task, fragment and vertex stages, in
    /// this order, for a program with a mesh shader. The draw binds the buffers of the task and mesh banks, the
    /// textures of the bank they share and those of the fragment stage. It has no vertex input.
    fn mesh_draw<M: GuestMem>(&mut self, mem: &M, st: &State, args: DrawArgs) -> DrawDetail {
        let mut d = empty_draw();
        d.push = st.push[0].clone();
        for s in [MESH, TASK, 1, 0] {
            self.track.program(s, st.program[s]);
        }
        let found = [MESH, TASK, 1, 0]
            .into_iter()
            .map(|s| st.program[s])
            .find_map(|p| Some((p, self.programs.find(p, Kind::Mesh)?.clone())));
        let Some((program, mesh)) = found else {
            let what = format_args!(
                "mesh draw {args:?}: no mesh stage, programs {:x?}, stages of the mesh slot: {}",
                st.program,
                self.programs.describe(st.program[MESH])
            );
            nxkit_nvn::diag::note(nxkit_nvn::diag::Reason::DrawNoVertex, what);
            return d;
        };
        let task = [st.program[TASK], program]
            .into_iter()
            .find_map(|p| self.programs.find(p, Kind::Task).cloned());
        let fragment = [st.program[1], program]
            .into_iter()
            .find_map(|p| self.programs.find(p, Kind::Fragment).cloned());
        self.track.read(&cache::DRAW_READS);
        self.track.read(&cache::MESH_READS);
        let has_task = task.is_some();
        d.program = ProgramRef { vertex: d.program.vertex, fragment, task, mesh: Some(mesh) };
        d.args = args;
        fixed_function(st, &mut d);
        let banks: &[(usize, Stage)] = if has_task {
            &[(TASK, Stage::Task), (MESH, Stage::Mesh), (1, Stage::Fragment)]
        } else {
            &[(MESH, Stage::Mesh), (1, Stage::Fragment)]
        };
        for &(bank, stage) in banks {
            self.ubos_into(mem, st, bank, stage, &mut d.ubos);
            sbufs_into(st, bank, stage, &mut d.sbufs);
            let textures = if stage == Stage::Fragment { 1 } else { MESH_TEXTURES };
            textures_into(st, textures, stage, &mut d.textures);
            images_into(st, textures, stage, &mut d.images);
        }
        if !d.images.is_empty() {
            self.track.truncated();
        }
        d
    }
}

/// Fills the blend, depth, stencil, raster and dynamic state of a draw.
fn fixed_function(st: &State, d: &mut DrawDetail) {
    d.blend = st.blends.detail();
    d.write_masks = st.masks;
    d.depth_stencil = objects::depth_stencil(&st.depth_stencil, &st.stencil);
    d.raster = st.raster.detail();
    d.dynamic = DynamicDetail {
        viewport: st.viewport,
        depth_range: st.depth_range,
        scissor: st.scissor,
        blend_color: st.blend_color,
        stencil_ref: st.stencil.reference,
    };
}

/// The arguments of a mesh draw record, or None for another kind.
fn mesh_args(x: &Ext) -> Option<DrawArgs> {
    let kind = x.arg;
    if kind == DrawKind::MeshTasks as u16 {
        Some(DrawArgs::MeshTasks { first: x.word(0), count: x.word(1) })
    } else if kind == DrawKind::MeshTasksIndirect as u16 {
        Some(DrawArgs::MeshTasksIndirect { indirect: x.word64(0) })
    } else {
        None
    }
}

/// The topology and arguments of a draw record that is not a mesh draw. None for the kinds that the encoder has no
/// detail for (transform feedback and `DrawTexture`).
fn draw_args(x: &Ext) -> Option<(u32, DrawArgs)> {
    let kind = x.arg;
    if kind == DrawKind::Arrays as u16 {
        let args = DrawArgs::Arrays {
            first: x.word(1),
            count: x.word(2),
            instances: 1,
            base_instance: 0,
        };
        Some((x.word(0), args))
    } else if kind == DrawKind::ArraysInstanced as u16 {
        let args = DrawArgs::Arrays {
            first: x.word(1),
            count: x.word(2),
            base_instance: x.word(3),
            instances: x.word(4),
        };
        Some((x.word(0), args))
    } else if kind == DrawKind::Elements as u16 {
        let args = DrawArgs::Indexed {
            index_type: x.word(1),
            count: x.word(2),
            address: x.word64(3),
            base_vertex: x.word(5) as i32,
            base_instance: x.word(6),
            instances: x.word(7),
        };
        Some((x.word(0), args))
    } else if kind == DrawKind::ArraysIndirect as u16 {
        Some((x.word(0), DrawArgs::ArraysIndirect { indirect: x.word64(1) }))
    } else if kind == DrawKind::ElementsIndirect as u16 {
        let args = DrawArgs::IndexedIndirect {
            index_type: x.word(1),
            address: x.word64(2),
            indirect: x.word64(4),
        };
        Some((x.word(0), args))
    } else if kind == DrawKind::MultiArraysIndirectCount as u16 {
        let args = DrawArgs::MultiIndirectCount {
            index: None,
            indirect: x.word64(1),
            count: x.word64(3),
            max: x.word(5),
            stride: x.word64(6),
        };
        Some((x.word(0), args))
    } else if kind == DrawKind::MultiElementsIndirectCount as u16 {
        let args = DrawArgs::MultiIndirectCount {
            index: Some((x.word(1), x.word64(2))),
            indirect: x.word64(4),
            count: x.word64(6),
            max: x.word(8),
            stride: x.word64(9),
        };
        Some((x.word(0), args))
    } else {
        None
    }
}

/// A chunk of vertex attribute (argument 0) or vertex stream (argument 1) states.
impl Walker {
    fn vertex_state(&mut self, st: &mut State, x: &Ext) {
        let (first, count) = (x.word(0) as usize, x.word(1) as usize);
        for i in 0..count {
            let at = first + i;
            if x.arg == 0 {
                if let Some(slot) = st.attribute.get_mut(at) {
                    let (format, offset, stream) = objects::attribute(x.word(2 + i));
                    *slot = (format != 0).then_some((format, offset, stream));
                    self.track.write(cache::attribute_bit(at));
                }
            } else if let Some(slot) = st.stream.get_mut(at) {
                *slot = (x.word(2 + 2 * i), x.word(3 + 2 * i));
                self.track.write(cache::stream_bit(at));
            }
        }
    }

    fn setting(&mut self, st: &mut State, x: &Ext) {
        let kind = x.arg;
        let mut set = |group: usize| self.track.write(cache::scalar_bit(group));
        if kind == Setting::BlendColor as u16 {
            st.blend_color = [f(x, 0), f(x, 1), f(x, 2), f(x, 3)];
            set(sc::BLEND_COLOR);
        } else if kind == Setting::StencilValueMask as u16 {
            st.stencil.set_read(x.word(0), x.word(1));
            faces(x.word(0), sc::STENCIL_READ, &mut set);
        } else if kind == Setting::StencilMask as u16 {
            st.stencil.set_write(x.word(0), x.word(1));
            faces(x.word(0), sc::STENCIL_WRITE, &mut set);
        } else if kind == Setting::StencilRef as u16 {
            st.stencil.set_ref(x.word(0), x.word(1));
            faces(x.word(0), sc::STENCIL_REF, &mut set);
        } else if kind == Setting::PolygonOffsetClamp as u16 {
            st.raster.offset = [f(x, 0), f(x, 1), f(x, 2)];
            set(sc::OFFSET);
        } else if kind == Setting::CopyRowStride as u16 {
            st.copy_strides[0] = stride(x);
            set(sc::STRIDE_ROW);
        } else if kind == Setting::CopyImageStride as u16 {
            st.copy_strides[1] = stride(x);
            set(sc::STRIDE_IMAGE);
        } else if kind == Setting::DepthClamp as u16 {
            st.raster.clamp = x.word(0) != 0;
            set(sc::CLAMP);
        }
    }
}

/// Calls `set` with the group of each face that an `NVNface` value names, starting at `first`.
fn faces(face: u32, first: usize, set: &mut impl FnMut(usize)) {
    let bits = StencilValues::face_bits(face);
    for i in 0..2 {
        if bits >> i & 1 != 0 {
            set(first + i);
        }
    }
}

/// The level and first layer of a `NVNtextureView` in four words. Without a view the copy uses level 0 and layer 0.
fn view_of(w: [u32; 4]) -> (u32, u32) {
    let flags = w[0] & 0xff;
    let level = if flags & 1 != 0 { w[0] >> 16 & 31 } else { 0 };
    let layer = if flags & 2 != 0 { w[1] >> 16 } else { 0 };
    (level, layer)
}

/// The origin and size of a `NVNcopyRegion` in six words. A negative value counts as 0 and a depth of 0 as 1.
fn region_of(w: [u32; 6]) -> ([u32; 3], [u32; 3]) {
    let v = |i: usize| (w[i] as i32).max(0) as u32;
    ([v(0), v(1), v(2)], [v(3), v(4), v(5).max(1)])
}

/// The detail of a `ClearTexture` record: the argument is `kind | has_view << 8` and the words are the texture
/// address (2), the channel mask, the colour (4), the view (4, if there is one) and the region (6). A record that is
/// too short keeps the default entry.
fn clear_detail(x: &Ext) -> CopyDetail {
    let has_view = x.arg >> 8 & 1 != 0;
    let at = 7 + if has_view { 4 } else { 0 };
    if x.words().len() < at + 6 {
        return CopyDetail::default();
    }
    let (level, layer) = if has_view { view_of([7, 8, 9, 10].map(|i| x.word(i))) } else { (0, 0) };
    let (origin, size) = region_of([0, 1, 2, 3, 4, 5].map(|i| x.word(at + i)));
    CopyDetail {
        kind: CopyKind::Clear,
        dst_level: level,
        dst_layer: layer,
        dst_origin: origin,
        size,
        dst_size: size,
        clear: [3, 4, 5, 6].map(|i| x.word(i)),
        clear_kind: u32::from(x.arg & 0xff),
        channel_mask: x.word(2),
        ..CopyDetail::default()
    }
}

/// The detail of a copy record with a texture side. The words are described at [`ExtCode::CopyBufferToTexture`] and
/// [`ExtCode::CopyTextureToTexture`]. A record that is too short for its words keeps the default entry.
fn copy_detail(st: &State, x: &Ext) -> CopyDetail {
    let view = |at: usize| view_of([x.word(at), x.word(at + 1), x.word(at + 2), x.word(at + 3)]);
    let region = |at: usize| region_of([0, 1, 2, 3, 4, 5].map(|i| x.word(at + i)));
    let mut d = CopyDetail { row_stride: st.copy_strides[0], image_stride: st.copy_strides[1], ..CopyDetail::default() };
    if x.code == ExtCode::CopyTextureToTexture {
        let (has_src, has_dst) = (x.arg & 1 != 0, x.arg & 2 != 0);
        let src_at = 5 + if has_src { 4 } else { 0 };
        let dst_at = src_at + 6 + if has_dst { 4 } else { 0 };
        if x.words().len() < dst_at + 6 {
            return d;
        }
        let (src_level, src_layer) = if has_src { view(5) } else { (0, 0) };
        let (dst_level, dst_layer) = if has_dst { view(dst_at - 4) } else { (0, 0) };
        let (src_origin, size) = region(src_at);
        let (dst_origin, dst_size) = region(dst_at);
        return CopyDetail {
            kind: CopyKind::TextureToTexture,
            src_level, src_layer, src_origin, dst_level, dst_layer, dst_origin, size, dst_size,
            ..d
        };
    }
    let (level, layer) = if x.arg & 1 != 0 { view(5) } else { (0, 0) };
    let at = if x.arg & 1 != 0 { 9 } else { 5 };
    if x.words().len() < at + 6 {
        return d;
    }
    let (origin, size) = region(at);
    d.buffer = x.word64(2);
    d.size = size;
    d.dst_size = size;
    if x.code == ExtCode::CopyBufferToTexture {
        d.kind = CopyKind::BufferToTexture;
        (d.dst_level, d.dst_layer, d.dst_origin) = (level, layer, origin);
    } else {
        d.kind = CopyKind::TextureToBuffer;
        (d.src_level, d.src_layer, d.src_origin) = (level, layer, origin);
    }
    d
}

/// The value of a stride setting. A negative stride is no stride.
fn stride(x: &Ext) -> u64 {
    u64::try_from(x.word64(0) as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_metal::AddressMap;
    use nxkit_nvn::mem::VecMem;

    #[test]
    fn indirect_and_mesh_draw_records_give_their_arguments() {
        let draw = |kind: DrawKind| Ext::new(ExtCode::Draw, kind as u16);
        let x = draw(DrawKind::MultiElementsIndirectCount).u32(4).u32(2).u64(0x1000).u64(0x2000).u64(0x3000).u32(9).u64(20);
        assert_eq!(
            draw_args(&x),
            Some((4, DrawArgs::MultiIndirectCount {
                index: Some((2, 0x1000)),
                indirect: 0x2000,
                count: 0x3000,
                max: 9,
                stride: 20
            }))
        );
        let x = draw(DrawKind::MultiArraysIndirectCount).u32(5).u64(0x2000).u64(0x3000).u32(3).u64(16);
        assert_eq!(
            draw_args(&x),
            Some((5, DrawArgs::MultiIndirectCount { index: None, indirect: 0x2000, count: 0x3000, max: 3, stride: 16 }))
        );
        let x = draw(DrawKind::ElementsIndirect).u32(4).u32(1).u64(0x1000).u64(0x2000);
        assert_eq!(
            draw_args(&x),
            Some((4, DrawArgs::IndexedIndirect { index_type: 1, address: 0x1000, indirect: 0x2000 }))
        );
        let x = draw(DrawKind::ArraysIndirect).u32(0).u64(0x2000);
        assert_eq!(draw_args(&x), Some((0, DrawArgs::ArraysIndirect { indirect: 0x2000 })));
        let x = draw(DrawKind::MeshTasks).u32(7).u32(2048);
        assert_eq!(mesh_args(&x), Some(DrawArgs::MeshTasks { first: 7, count: 2048 }));
        assert_eq!(draw_args(&x), None);
        let x = draw(DrawKind::MeshTasksIndirect).u64(0x4000);
        assert_eq!(mesh_args(&x), Some(DrawArgs::MeshTasksIndirect { indirect: 0x4000 }));
        assert_eq!(mesh_args(&draw(DrawKind::Arrays).u32(4).u32(0).u32(3)), None);
    }

    #[test]
    fn uniform_snapshot_keeps_the_unchanged_tail() {
        let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
        let mut w = Walker::new(Default::default(), map);
        let mut mem = VecMem::new(0x1000, 128);
        mem.write(0x1070, &[1, 2, 3, 4]).unwrap();
        w.updated.push((0x2000, 0x2010));
        let a = w.snapshot(&mem, 0x2000, 128).unwrap();
        assert_eq!(a.len(), 128);
        assert_eq!(&a[112..116], &[1, 2, 3, 4]);
        assert!(Arc::ptr_eq(&a, &w.snapshot(&mem, 0x2000, 128).unwrap()));

        // CPU writes between queue submissions must not reuse the previous snapshot.
        mem.write(0x1070, &[5, 6, 7, 8]).unwrap();
        w.submit(&mut mem, 1, &[]);
        let b = w.snapshot(&mem, 0x2000, 128).unwrap();
        assert_eq!(&b[112..116], &[5, 6, 7, 8]);
        assert_eq!(&a[112..116], &[1, 2, 3, 4]);
    }

    #[test]
    fn a_range_with_the_same_bytes_again_in_the_frame_shares_its_buffer() {
        let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
        let mut w = Walker::new(Default::default(), map);
        w.copy_all = true;
        let mut mem = VecMem::new(0x1000, 128);
        mem.write(0x1000, &[9, 9]).unwrap();
        let a = w.snapshot(&mem, 0x2000, 128).unwrap();
        // The next submit asks again with the same bytes: the same buffer.
        w.snaps.get_mut().clear();
        let b = w.snapshot(&mem, 0x2000, 128).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // Other bytes give a new buffer, and the first one keeps its bytes.
        w.snaps.get_mut().clear();
        mem.write(0x1000, &[7, 7]).unwrap();
        let c = w.snapshot(&mem, 0x2000, 128).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert_eq!((&a[..2], &c[..2]), (&[9u8, 9][..], &[7u8, 7][..]));
        // A new frame starts again.
        w.take();
        w.snaps.get_mut().clear();
        let d = w.snapshot(&mem, 0x2000, 128).unwrap();
        assert!(!Arc::ptr_eq(&c, &d));
    }

    #[test]
    fn the_buffer_of_a_dropped_frame_comes_back_in_order() {
        let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
        let mut w = Walker::new(Default::default(), map);
        w.copy_all = true;
        let mem = VecMem::new(0x1000, 128);
        let first = w.snapshot(&mem, 0x2000, 64).unwrap();
        let second = w.snapshot(&mem, 0x2040, 64).unwrap();
        let (p1, p2) = (first.as_ptr(), second.as_ptr());
        w.take();
        w.snaps.get_mut().clear();
        // The frame is still held, so these are new buffers.
        let busy = w.snapshot(&mem, 0x2000, 64).unwrap();
        assert!(busy.as_ptr() != p1 && busy.as_ptr() != p2);
        drop((first, second, busy));
        w.snaps.get_mut().clear();
        w.take();
        let again = w.snapshot(&mem, 0x2000, 64).unwrap();
        assert_eq!(again.as_ptr(), p1);
        let again = w.snapshot(&mem, 0x2040, 64).unwrap();
        assert_eq!(again.as_ptr(), p2);
    }

    #[test]
    fn an_inline_update_invalidates_a_cached_unmodified_range() {
        let map = AddressMap::new(0x1000, 0x2000, 128).unwrap();
        let mut walker = Walker::new(Default::default(), map);
        let mut mem = VecMem::new(0x1000, 128);
        assert!(walker.snapshot(&mem, 0x2000, 128).is_none());
        assert!(walker.snaps.borrow().contains_key(&(0x2000, 128)));
        let update = Ext::new(ExtCode::UpdateUniform, 4).u32(0x2000).u32(0).u32(128).u32(0)
            .u32(16).u32(0).u32(0x12345678);
        walker.update_uniform(&mut mem, &update);
        let snapshot = walker.snapshot(&mem, 0x2000, 128).unwrap();
        assert_eq!(&snapshot[16..20], &0x12345678u32.to_le_bytes());
    }

    #[test]
    fn unbound_vertex_inputs_get_a_constant_stream() {
        let mut d = empty_draw();
        d.attributes.push(AttributeDetail {
            location: 0,
            format: 0x0a,
            offset: 0,
            stream: 0,
        });
        d.streams.push(StreamDetail {
            index: 0,
            address: 0x2000,
            size: 128,
            stride: 4,
            divisor: 0,
        });
        let inputs = [
            (0, nxkit_metal::VertexFormat::Float4),
            (7, nxkit_metal::VertexFormat::UInt4),
        ];
        let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
        assert_eq!(layout.attributes.len(), 2);
        assert_eq!(
            layout.attributes[0].1.buffer,
            crate::translate::stream_slot(0)
        );
        assert_eq!(layout.attributes[1].0, 7);
        assert_eq!(
            layout.attributes[1].1.format,
            nxkit_metal::VertexFormat::UInt4
        );
        assert_eq!(
            layout.attributes[1].1.buffer,
            crate::translate::DEFAULT_VERTEX_SLOT
        );
        assert!(
            layout
                .layouts
                .iter()
                .any(|l| l.buffer == crate::translate::DEFAULT_VERTEX_SLOT
                    && l.step == nxkit_metal::StepFunction::Constant)
        );
        // The declared attribute itself also needs the fallback when its stream is unbound.
        d.streams.clear();
        let layout = crate::translate::vertex_descriptor(&d, &inputs).unwrap();
        assert!(
            layout
                .attributes
                .iter()
                .all(|(_, a)| a.buffer == crate::translate::DEFAULT_VERTEX_SLOT)
        );
    }

    fn copy_ext(code: ExtCode, arg: u16, words: &[u32]) -> Ext {
        words.iter().fold(Ext::new(code, arg), |x, w| x.u32(*w))
    }

    #[test]
    fn a_buffer_to_texture_copy_reads_the_buffer_view_and_region() {
        let mut st = State::default();
        st.copy_strides = [512, 4096];
        // texture 0x2000, buffer 0x9_0000_1000, flags, view (levels, layers), region.
        let view = [1 | 2 | 3 << 16, 5 << 16, 0, 0];
        let region = [4u32, 8, 2, 120, 75, 3];
        let words: Vec<u32> = [0x2000, 0, 0x1000, 9, 0].into_iter().chain(view).chain(region).collect();
        let d = copy_detail(&st, &copy_ext(ExtCode::CopyBufferToTexture, 1, &words));
        assert_eq!((d.kind, d.buffer), (CopyKind::BufferToTexture, 0x9_0000_1000));
        assert_eq!((d.dst_level, d.dst_layer), (3, 5));
        assert_eq!((d.dst_origin, d.size, d.dst_size), ([4, 8, 2], [120, 75, 3], [120, 75, 3]));
        assert_eq!((d.row_stride, d.image_stride), (512, 4096));

        // Without a view the region starts 4 words earlier and the copy uses level 0, layer 0.
        let words = [0x2000, 0, 0x1000, 9, 0, 0, 0, 0, 320, 180, 0];
        let d = copy_detail(&st, &copy_ext(ExtCode::CopyBufferToTexture, 0, &words));
        assert_eq!((d.dst_level, d.dst_layer, d.dst_origin, d.size), (0, 0, [0; 3], [320, 180, 1]));
    }

    #[test]
    fn a_texture_to_buffer_copy_keeps_the_texture_on_the_source_side() {
        let words = [0x2000, 0, 0x1000, 9, 0, 0, 0, 0, 16, 16, 1];
        let d = copy_detail(&State::default(), &copy_ext(ExtCode::CopyTextureToBuffer, 0, &words));
        assert_eq!(d.kind, CopyKind::TextureToBuffer);
        assert_eq!((d.src_origin, d.size, d.dst_origin), ([0; 3], [16, 16, 1], [0; 3]));
    }

    #[test]
    fn a_texture_to_texture_copy_reads_both_views_and_regions() {
        let src_view = [1 | 2 << 16, 0, 0, 0];
        let dst_view = [1 | 2 | 1 << 16, 7 << 16, 0, 0];
        let words: Vec<u32> = [1, 0, 2, 0, 0]
            .into_iter()
            .chain(src_view)
            .chain([1, 2, 0, 64, 32, 1])
            .chain(dst_view)
            .chain([5, 6, 0, 64, 32, 1])
            .collect();
        let d = copy_detail(&State::default(), &copy_ext(ExtCode::CopyTextureToTexture, 3, &words));
        assert_eq!(d.kind, CopyKind::TextureToTexture);
        assert_eq!((d.src_level, d.src_layer, d.src_origin), (2, 0, [1, 2, 0]));
        assert_eq!((d.dst_level, d.dst_layer, d.dst_origin), (1, 7, [5, 6, 0]));
        assert_eq!((d.size, d.dst_size), ([64, 32, 1], [64, 32, 1]));

        // Only the destination has a view.
        let words: Vec<u32> = [1, 0, 2, 0, 0, 0, 0, 0, 8, 8, 1].into_iter().chain(dst_view).chain([0, 0, 0, 8, 8, 1]).collect();
        let d = copy_detail(&State::default(), &copy_ext(ExtCode::CopyTextureToTexture, 2, &words));
        assert_eq!((d.src_level, d.dst_level, d.dst_layer), (0, 1, 7));
    }

    #[test]
    fn a_clear_texture_record_reads_colour_mask_view_and_region() {
        let colour = [1.0f32.to_bits(), 0.5f32.to_bits(), 0, 0.25f32.to_bits()];
        let head = [0x2000, 0, 0xb];
        let words: Vec<u32> = head
            .into_iter()
            .chain(colour)
            .chain([1 | 2 | 2 << 16, 3 << 16, 0, 0])
            .chain([1, 2, 0, 30, 40, 1])
            .collect();
        let d = clear_detail(&copy_ext(ExtCode::ClearTexture, 1 << 8, &words));
        assert_eq!(d.kind, CopyKind::Clear);
        assert_eq!((d.dst_level, d.dst_layer, d.dst_origin, d.size), (2, 3, [1, 2, 0], [30, 40, 1]));
        assert_eq!((d.clear, d.clear_kind, d.channel_mask), (colour, 0, 0xb));

        let words: Vec<u32> = head.into_iter().chain([7, 0, 0, 0]).chain([0, 0, 0, 8, 8, 0]).collect();
        let d = clear_detail(&copy_ext(ExtCode::ClearTexture, 2, &words));
        assert_eq!((d.clear_kind, d.clear[0], d.dst_level, d.size), (2, 7, 0, [8, 8, 1]));
        assert_eq!(clear_detail(&copy_ext(ExtCode::ClearTexture, 0, &head)), CopyDetail::default());
    }

    #[test]
    fn a_short_copy_record_keeps_an_empty_entry() {
        let d = copy_detail(&State::default(), &copy_ext(ExtCode::CopyBufferToTexture, 1, &[1, 0, 2, 0, 0]));
        assert_eq!(d, CopyDetail::default());
    }

    #[test]
    fn the_copy_strides_come_from_settings_and_reset_per_recording() {
        let mut st = State::default();
        let mut walker = Walker::new(Default::default(), AddressMap::new(0x1000, 0x2000, 128).unwrap());
        walker.setting(&mut st, &Ext::new(ExtCode::Setting, Setting::CopyRowStride as u16).u32(256).u32(0));
        walker.setting(&mut st, &Ext::new(ExtCode::Setting, Setting::CopyImageStride as u16).u32(0xffff_ffff).u32(0xffff_ffff));
        assert_eq!(st.copy_strides, [256, 0]);
    }

    /// The records that become `Cmd::Copy` or `Cmd::Downsample` in the decoder, with the entry that the walker keeps.
    fn copy_like(tex: [u64; 2]) -> Vec<(Ext, CopyKind)> {
        let region = [0, 0, 0, 4, 4, 1];
        let t = |i: usize| [tex[i] as u32, (tex[i] >> 32) as u32];
        let join = |a: &[u32], b: &[u32]| a.iter().chain(b).copied().collect::<Vec<u32>>();
        vec![
            (copy_ext(ExtCode::ClearBuffer, 0, &[0x100, 0, 4, 0, 0]), CopyKind::Other),
            (copy_ext(ExtCode::CopyBufferToTexture, 0, &join(&[t(0)[0], t(0)[1], 0x100, 0, 0], &region)), CopyKind::BufferToTexture),
            (copy_ext(ExtCode::CopyBufferToBuffer, 0, &[0x100, 0, 0x200, 0, 4, 0, 0]), CopyKind::Other),
            (copy_ext(ExtCode::CopyTextureToBuffer, 0, &join(&[t(0)[0], t(0)[1], 0x100, 0, 0], &region)), CopyKind::TextureToBuffer),
            (copy_ext(ExtCode::UpdateUniform, 4, &[0x300, 0, 0, 0, 0, 0, 7]), CopyKind::Other),
            (copy_ext(ExtCode::CopyTextureToTexture, 0, &join(&[t(0)[0], t(0)[1], t(1)[0], t(1)[1], 0], &[0, 0, 0, 4, 4, 1, 0, 0, 0, 4, 4, 1])), CopyKind::TextureToTexture),
            // A downsample of an address that is no texture is dropped by the decoder and still has an entry.
            (copy_ext(ExtCode::DownsampleView, 0, &[0x7777, 0, 0x8888, 0]), CopyKind::Other),
            (copy_ext(ExtCode::DownsampleView, 0, &[t(0)[0], t(0)[1], t(1)[0], t(1)[1]]), CopyKind::Other),
            (copy_ext(ExtCode::ClearTexture, 0, &join(&[t(1)[0], t(1)[1], 0xf, 0, 0, 0, 0], &region)), CopyKind::Clear),
        ]
    }

    #[test]
    fn the_walker_and_the_decoder_count_the_same_copies() {
        use nxkit_nvn::frame::Cmd;
        use nxkit_nvn::front::FrontEnd;
        use nxkit_nvn::layout::{Target, TextureDesc};
        use nxkit_nvn::record::Record;
        use nxkit_nvn::space::LinearSpace;

        const HOST: u64 = 0x7000_0000;
        const GPU: u64 = 0x15_0400_8000;
        const SIZE: u64 = 1 << 20;
        let mut f = FrontEnd::new(LinearSpace::new(HOST, GPU, SIZE).unwrap(), VecMem::new(HOST, SIZE as usize));
        let desc = TextureDesc {
            target: Target::D2, format: 0x25, flags: 0, samples: 0, width: 4, height: 4, depth: 1, levels: 1,
            stride: 0, packaged_block: None,
        };
        let a = f.create_texture(desc, HOST + 0x1_0000).unwrap();
        let b = f.create_texture(desc, HOST + 0x2_0000).unwrap();
        let tex = [f.texture_address(a).unwrap(), f.texture_address(b).unwrap()];
        let list = copy_like(tex);

        f.add_command_memory(0, HOST + 0x8_0000, 4096).unwrap();
        f.begin_recording(0).unwrap();
        for (x, _) in &list {
            f.write(0, Record::Ext(*x)).unwrap();
        }
        let h = f.end_recording(0).unwrap();
        f.submit(h).unwrap();
        let cmds = f.frame().cmds.iter().filter(|c| matches!(c, Cmd::Copy { .. } | Cmd::Downsample { .. })).count();

        let map = AddressMap::new(HOST, GPU, SIZE).unwrap();
        let mut walker = Walker::new(Default::default(), map);
        let mut mem = VecMem::new(HOST, SIZE as usize);
        let mut at = HOST + 0x8_0000;
        for rec in list.iter().map(|(x, _)| Record::Ext(*x)).chain([Record::End]) {
            let bytes = rec.encode().unwrap();
            mem.write(at, bytes.as_bytes()).unwrap();
            at += bytes.len() as u64;
        }
        walker.submit(&mut mem, 1, &[nxkit_nvn::cmdbuf::CommandHandle(HOST + 0x8_0000)]);
        let detail = walker.take();
        assert_eq!(cmds, list.len());
        assert_eq!(detail.copies.len(), cmds);
        let kinds: Vec<_> = detail.copies.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, list.iter().map(|(_, k)| *k).collect::<Vec<_>>());
        let ords = crate::draws::ordinals(f.frame());
        assert_eq!(ords.last().map(|o| o.copy), Some(cmds as u32 - 1));
    }
}
