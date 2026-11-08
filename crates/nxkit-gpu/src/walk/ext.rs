//! The extension records: what each of them changes in the state of the walker, and the detail of draws and clears.

use std::sync::Arc;

use nxkit_nvn::ext::{DrawKind, Ext, ExtCode, Setting, StateKind};
use nxkit_nvn::mem::GuestMem;

use super::objects::{self, write_masks};
use super::programs::Kind;
use super::{ATTRIBUTES, MAX_COLOR, State, STREAMS, UBOS, Walker, empty_draw, textures};
use crate::detail::{
    AttributeDetail, ClearDetail, DrawArgs, DrawDetail, DynamicDetail, ProgramRef, Stage, StreamDetail, UboDetail,
};

/// The `NVNshaderStage` values that have uniform buffers here, as the slot in `State::ubo`.
fn ubo_stage(stage: u16) -> Option<usize> {
    match stage {
        0 => Some(0),
        1 => Some(1),
        5 => Some(2),
        _ => None,
    }
}

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

impl Walker {
    pub(super) fn ext<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, x: &Ext) {
        match x.code {
            ExtCode::Program => {
                let (program, bits) = (x.word64(0), x.word(2));
                for (s, p) in st.program.iter_mut().enumerate() {
                    if bits >> s & 1 != 0 {
                        *p = program;
                    }
                }
            }
            ExtCode::VertexBuffer => {
                if let Some(v) = st.vertex_buffer.get_mut(usize::from(x.arg)) {
                    *v = (x.word64(0), x.word64(2));
                }
            }
            ExtCode::UniformBuffer => {
                let slot = usize::from(x.arg & 0xff);
                if let (Some(stage), true) = (ubo_stage(x.arg >> 8), slot < UBOS) {
                    st.ubo[stage][slot] = (x.word64(0), x.word64(2).min(u64::from(u32::MAX)) as u32);
                }
            }
            ExtCode::Viewport if x.arg == 0 => st.viewport = [f(x, 0), f(x, 1), f(x, 2), f(x, 3)],
            ExtCode::DepthRange if x.arg == 0 => st.depth_range = [f(x, 0), f(x, 1)],
            ExtCode::Scissor if x.arg == 0 => {
                let v = |i: usize| x.word(i) as i32;
                st.scissor = Some([v(0).max(0) as u32, v(1).max(0) as u32, v(2).max(0) as u32, v(3).max(0) as u32]);
            }
            ExtCode::State => self.state_object(st, x),
            ExtCode::VertexState => vertex_state(st, x),
            ExtCode::Setting => setting(st, x),
            ExtCode::UpdateUniform => self.update_uniform(mem, x),
            ExtCode::ClearColor => {
                let slot = (x.arg & 0xff) as u8;
                if slot < MAX_COLOR {
                    let kind = x.arg >> 8;
                    let c = |i: usize| clear_value(kind, x.word(1 + i));
                    self.out.clears.push(ClearDetail { color: [c(0), c(1), c(2), c(3)], ..ClearDetail::default() });
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
                let d = self.draw(&*mem, st, x);
                self.out.draws.push(d);
            }
            ExtCode::Dispatch => {
                let groups = if x.arg == 0 { [x.word(0), x.word(1), x.word(2)] } else { [0; 3] };
                let d = self.dispatch(&*mem, st, groups);
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
            StateKind::Blend => st.blends.set_blend(w),
            StateKind::Color => st.blends.set_color(w),
            StateKind::ChannelMask => st.masks = write_masks(w),
            StateKind::Multisample => st.raster.set_multisample(w),
            StateKind::Polygon => st.raster.set_polygon(w),
            StateKind::DepthStencil => st.depth_stencil = [x.word(0), x.word(1)],
        }
    }

    /// The command writes its bytes into the buffer when the GPU runs it. The walker writes them now and remembers the
    /// range, so that the draws that bind it get the bytes they saw.
    fn update_uniform<M: GuestMem>(&mut self, mem: &mut M, x: &Ext) {
        let (buffer, offset, len) = (x.word64(0), x.word64(4), usize::from(x.arg));
        let at = buffer.saturating_add(offset);
        let bytes: Vec<u8> = x.words().get(6..).unwrap_or(&[]).iter().flat_map(|w| w.to_le_bytes()).take(len).collect();
        let Ok(host) = self.map.host_address(at, bytes.len() as u64) else {
            return;
        };
        if mem.write(host, &bytes).is_err() {
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
        if let Some(snapshot) = self.snaps.borrow().get(&(addr, size)) {
            return snapshot.clone();
        }
        let end = addr + u64::from(size);
        // A partial update does not reduce the bound range. Its unchanged tail can contain constants that the
        // shader still reads. Copying only the updated prefix makes those reads escape the scratch allocation.
        if !self.updated.iter().any(|(s, e)| addr < *e && *s < end) {
            // Most bound blocks have no inline updates. Remember that result as well: scanning the update
            // history for every UBO of every draw otherwise dominates the command walker on large frames.
            self.snaps.borrow_mut().insert((addr, size), None);
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let host = self.map.host_address(addr, u64::from(size)).ok()?;
        mem.read(host, &mut buf).ok()?;
        let snap: Arc<[u8]> = buf.into();
        self.snaps.borrow_mut().insert((addr, size), Some(snap.clone()));
        Some(snap)
    }

    fn draw<M: GuestMem>(&mut self, mem: &M, st: &State, x: &Ext) -> DrawDetail {
        let mut d = empty_draw();
        static SHOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let show =
            || crate::diag("prog") && SHOWN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40;
        let Some((topology, args)) = draw_args(x) else {
            if crate::diag("indirect") {
                static K: [std::sync::atomic::AtomicU32; 16] = [const { std::sync::atomic::AtomicU32::new(0) }; 16];
                let n = K[usize::from(x.arg) & 15].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n.is_power_of_two() {
                    eprintln!("DIAG indirect kind {} dropped {} times, program {:x?}", x.arg, n + 1, st.program);
                }
            }
            if show() {
                eprintln!("DIAG draw: no draw args, kind {}", x.arg);
            }
            return d;
        };
        self.track.program(0, st.program[0]);
        let Some(vertex) = self.programs.find(st.program[0], Kind::Vertex) else {
            if show() {
                eprintln!(
                    "DIAG draw: no vertex stage, programs {:x?}, stages of [0]: {}",
                    st.program,
                    self.programs.describe(st.program[0])
                );
            }
            return d;
        };
        self.track.program(1, st.program[1]);
        self.track.read(&cache::DRAW_READS);
        d.program = ProgramRef {
            vertex: vertex.clone(),
            fragment: self.programs.find(st.program[1], Kind::Fragment).cloned(),
        };
        d.topology = topology;
        d.args = args;
        d.blend = st.blends.detail();
        d.write_masks = st.masks;
        d.depth_stencil = objects::depth_stencil(&st.depth_stencil, &st.stencil);
        d.raster = st.raster.detail();
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
        d.textures.reserve_exact(texture_count(st, 0) + texture_count(st, 1));
        textures_into(st, 0, Stage::Vertex, &mut d.textures);
        textures_into(st, 1, Stage::Fragment, &mut d.textures);
        d.dynamic = DynamicDetail {
            viewport: st.viewport,
            depth_range: st.depth_range,
            scissor: st.scissor,
            blend_color: st.blend_color,
            stencil_ref: st.stencil.reference,
        };
        d
    }
}

/// The topology and arguments of a draw record. None for the kinds that the encoder has no detail for.
fn draw_args(x: &Ext) -> Option<(u32, DrawArgs)> {
    let kind = x.arg;
    if kind == DrawKind::Arrays as u16 {
        let args = DrawArgs::Arrays { first: x.word(1), count: x.word(2), instances: 1, base_instance: 0 };
        Some((x.word(0), args))
    } else if kind == DrawKind::ArraysInstanced as u16 {
        let args =
            DrawArgs::Arrays { first: x.word(1), count: x.word(2), base_instance: x.word(3), instances: x.word(4) };
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
    } else {
        None
    }
}

/// A chunk of vertex attribute (argument 0) or vertex stream (argument 1) states.
fn vertex_state(st: &mut State, x: &Ext) {
    let (first, count) = (x.word(0) as usize, x.word(1) as usize);
    for i in 0..count {
        let at = first + i;
        if x.arg == 0 {
            if let Some(slot) = st.attribute.get_mut(at) {
                let (format, offset, stream) = objects::attribute(x.word(2 + i));
                *slot = (format != 0).then_some((format, offset, stream));
            }
        } else if let Some(slot) = st.stream.get_mut(at) {
            *slot = (x.word(2 + 2 * i), x.word(3 + 2 * i));
        }
    }
}

fn setting(st: &mut State, x: &Ext) {
    let kind = x.arg;
    if kind == Setting::BlendColor as u16 {
        st.blend_color = [f(x, 0), f(x, 1), f(x, 2), f(x, 3)];
    } else if kind == Setting::StencilValueMask as u16 {
        st.stencil.set_read(x.word(0), x.word(1));
    } else if kind == Setting::StencilMask as u16 {
        st.stencil.set_write(x.word(0), x.word(1));
    } else if kind == Setting::StencilRef as u16 {
        st.stencil.set_ref(x.word(0), x.word(1));
    } else if kind == Setting::PolygonOffsetClamp as u16 {
        st.raster.offset = [f(x, 0), f(x, 1), f(x, 2)];
    } else if kind == Setting::DepthClamp as u16 {
        st.raster.clamp = x.word(0) != 0;
    }
}
