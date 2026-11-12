//! The records that set state: the extension dispatch, state objects, vertex states and settings.

use super::copy::{
    buffer_copy_detail, buffer_fill_detail, clear_detail, copy_detail, faces, stride,
};
use super::*;

/// The size of a `Dispatch` record: the direct group counts (arg 0), or the guest GPU address of the counts of an
/// indirect dispatch (arg 1, words 0 and 1) with zero counts, because the GPU reads them when the dispatch runs.
pub(super) fn dispatch_size(x: &Ext) -> ([u32; 3], Option<u64>) {
    if x.arg == 0 {
        ([x.word(0), x.word(1), x.word(2)], None)
    } else {
        ([0; 3], Some(x.word64(0)))
    }
}

impl Walker {
    pub(in crate::walk) fn ext<M: GuestMem>(&mut self, mem: &mut M, st: &mut State, x: &Ext) {
        match x.code {
            ExtCode::ReportCounter if matches!(x.word(0), 0 | 0x10) => {
                self.out.reports.push(crate::detail::CounterReport {
                    kind: x.word(0),
                    value: 0,
                    addr: x.word64(1),
                });
            }
            ExtCode::ReportValue => {
                self.out.reports.push(crate::detail::CounterReport {
                    kind: crate::detail::REPORT_VALUE,
                    value: x.word(0),
                    addr: x.word64(1),
                });
            }
            // The GPU writes the counter of the event when the work before the signal is done. The counter's address is
            // taken now, as the console takes it when the command is recorded.
            ExtCode::SignalEvent => {
                let mut b = [0u8; 8];
                let event = x.word64(0);
                let word = mem
                    .read(event.wrapping_add(EVENT_COUNTER), &mut b)
                    .map_or(0, |()| u64::from_le_bytes(b));
                if word != 0 {
                    let add = x.word(2) == nxkit_nvn::marks::EVENT_SIGNAL_ADD;
                    self.out.reports.push(crate::detail::CounterReport {
                        kind: if add {
                            crate::detail::REPORT_EVENT_ADD
                        } else {
                            crate::detail::REPORT_EVENT_WRITE
                        },
                        value: x.word(5),
                        addr: word,
                    });
                }
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
                    self.track
                        .write(cache::vertex_buffer_bit(usize::from(x.arg)));
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
            ExtCode::ViewportSwizzle if x.arg == 0 => {
                st.viewport_swizzle = [x.word(0), x.word(1), x.word(2), x.word(3)];
                self.track.write(cache::scalar_bit(sc::VIEWPORT_SWIZZLE));
                if !super::super::origin::supported(st.viewport_swizzle) {
                    static SHOWN: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!(
                            "nxkit-gpu: viewport swizzle {:?} is unsupported, drawing without the swizzle",
                            st.viewport_swizzle
                        );
                    }
                }
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
                static VIEWS: std::sync::Mutex<Vec<(u16, [u32; 4])>> =
                    std::sync::Mutex::new(Vec::new());
                let value = (x.arg, [x.word(0), x.word(1), x.word(2), x.word(3)]);
                if let Ok(mut views) = VIEWS.lock()
                    && views.len() < 80
                    && !views.contains(&value)
                {
                    eprintln!("DIAG target view {value:x?}");
                    views.push(value);
                }
            }
            ExtCode::VertexState => self.vertex_state(st, x),
            ExtCode::Setting => self.setting(st, x),
            ExtCode::DownsampleView => self.out.copies.push(CopyDetail::default()),
            ExtCode::ClearBuffer => self.out.copies.push(buffer_fill_detail(x)),
            ExtCode::CopyBufferToBuffer => self.out.copies.push(buffer_copy_detail(x)),
            ExtCode::ClearTexture => self.out.copies.push(clear_detail(x)),
            ExtCode::CopyBufferToTexture
            | ExtCode::CopyTextureToBuffer
            | ExtCode::CopyTextureToTexture => {
                self.track.read(&cache::COPY_READS);
                self.out.copies.push(copy_detail(st, x));
            }
            // Push constants: the first word is the byte offset, then `arg >> 1` bytes; bit 0 of `arg` is compute.
            ExtCode::Push => {
                let (compute, len) = (x.arg & 1 != 0, usize::from(x.arg >> 1));
                let w = x.words();
                let off = w.first().copied().unwrap_or(0) as usize;
                let bytes: Vec<u8> = w
                    .iter()
                    .skip(1)
                    .flat_map(|v| v.to_le_bytes())
                    .take(len)
                    .collect();
                let slot = &mut st.push[usize::from(compute)];
                let mut block: Vec<u8> = slot
                    .as_deref()
                    .map_or_else(|| vec![0; PUSH_BYTES], <[u8]>::to_vec);
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
                self.track
                    .patches_of(&d.textures, self.out.draws.len(), false);
                let t = self.prof.start();
                self.prof.draw_built(&d);
                self.out.draws.push(Arc::new(d));
                self.prof.lap(&self.prof.alloc_draw_ns, t);
            }
            ExtCode::Dispatch => {
                let t = self.prof.start();
                let d = self.dispatch(&*mem, st, dispatch_size(x));
                self.prof.lap(&self.prof.detail_ns, t);
                self.track
                    .patches_of(&d.textures, self.out.dispatches.len(), true);
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

    /// A chunk of vertex attribute (argument 0) or vertex stream (argument 1) states.
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

    pub(super) fn setting(&mut self, st: &mut State, x: &Ext) {
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
