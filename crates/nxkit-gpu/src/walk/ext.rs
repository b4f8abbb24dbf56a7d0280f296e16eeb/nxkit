//! The extension records: what each of them changes in the state of the walker, and the detail of draws and clears.

use std::sync::Arc;

use nxkit_nvn::ext::{DrawKind, Ext, ExtCode, Setting, StateKind};
use nxkit_nvn::mem::GuestMem;

use super::cache::{self, sc};
use super::objects::{self, Blends, StencilValues, write_masks};
use super::programs::Kind;
use super::{
    ATTRIBUTES, MAX_COLOR, STREAMS, State, UBOS, Walker, bound_count, empty_draw, sbufs_into,
    texture_count, textures_into,
};
use crate::detail::{
    AttributeDetail, ClearDetail, CopyDetail, CopyKind, DrawArgs, DrawDetail, DynamicDetail, ProgramRef, Stage,
    StreamDetail, UboDetail,
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
                        eprintln!("DIAG target view {value:x?}"); views.push(value);
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
            ExtCode::UpdateUniform => {
                self.out.copies.push(CopyDetail::default());
                self.update_uniform(mem, x);
            }
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
                self.track.patches_of(&d.textures, self.out.draws.len(), false);
                self.out.draws.push(Arc::new(d));
            }
            ExtCode::Dispatch => {
                let groups = if x.arg == 0 {
                    [x.word(0), x.word(1), x.word(2)]
                } else {
                    [0; 3]
                };
                let d = self.dispatch(&*mem, st, groups);
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
