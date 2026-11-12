//! The detail of a draw.

use super::*;

impl Walker {
    pub(super) fn draw<M: GuestMem>(&mut self, mem: &M, st: &State, x: &Ext) -> DrawDetail {
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
            static K: [std::sync::atomic::AtomicU32; 16] =
                [const { std::sync::atomic::AtomicU32::new(0) }; 16];
            let n = K[usize::from(x.arg) & 15].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if crate::diag("indirect") && n.is_power_of_two() {
                eprintln!(
                    "DIAG indirect kind {} dropped {} times, program {:x?}",
                    x.arg,
                    n + 1,
                    st.program
                );
            }
            return d;
        };
        self.prof.banks(st);
        self.track.program(0, st.program[0]);
        let Some(vertex) = self.programs.find(st.program[0], Kind::Vertex) else {
            let what = format_args!(
                "programs {:x?}, stages of [0]: {}",
                st.program,
                self.programs.describe(st.program[0])
            );
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
        d.attributes
            .reserve_exact(st.attribute.iter().flatten().count());
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
        d.ubos
            .reserve_exact(bound_count(&st.ubo[0]) + bound_count(&st.ubo[1]));
        self.ubos_into(mem, st, 0, Stage::Vertex, &mut d.ubos);
        self.ubos_into(mem, st, 1, Stage::Fragment, &mut d.ubos);
        d.sbufs
            .reserve_exact(bound_count(&st.sbuf[0]) + bound_count(&st.sbuf[1]));
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
        d.textures
            .reserve_exact(texture_count(st, 0) + texture_count(st, 1));
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
        d.program = ProgramRef {
            vertex: d.program.vertex,
            fragment,
            task,
            mesh: Some(mesh),
        };
        d.args = args;
        fixed_function(st, &mut d);
        let banks: &[(usize, Stage)] = if has_task {
            &[
                (TASK, Stage::Task),
                (MESH, Stage::Mesh),
                (1, Stage::Fragment),
            ]
        } else {
            &[(MESH, Stage::Mesh), (1, Stage::Fragment)]
        };
        for &(bank, stage) in banks {
            self.ubos_into(mem, st, bank, stage, &mut d.ubos);
            sbufs_into(st, bank, stage, &mut d.sbufs);
            let textures = if stage == Stage::Fragment {
                1
            } else {
                MESH_TEXTURES
            };
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
pub(super) fn fixed_function(st: &State, d: &mut DrawDetail) {
    d.blend = st.blends.detail();
    d.write_masks = st.masks;
    d.depth_stencil = objects::depth_stencil(&st.depth_stencil, &st.stencil);
    d.raster = st.raster.detail();
    d.raster.front_face = super::super::origin::front_face(st.raster.front_face, st.origins[1]);
    d.dynamic = DynamicDetail {
        viewport: super::super::origin::viewport(st.viewport, st.origins[0], st.viewport_swizzle),
        depth_range: st.depth_range,
        scissor: st.scissor,
        blend_color: st.blend_color,
        stencil_ref: st.stencil.reference,
    };
}

/// The arguments of a mesh draw record, or None for another kind.
pub(super) fn mesh_args(x: &Ext) -> Option<DrawArgs> {
    let kind = x.arg;
    if kind == DrawKind::MeshTasks as u16 {
        Some(DrawArgs::MeshTasks {
            first: x.word(0),
            count: x.word(1),
        })
    } else if kind == DrawKind::MeshTasksIndirect as u16 {
        Some(DrawArgs::MeshTasksIndirect {
            indirect: x.word64(0),
        })
    } else {
        None
    }
}

/// The topology and arguments of a draw record that is not a mesh draw. None for the kinds that the encoder has no
/// detail for (transform feedback and `DrawTexture`).
pub(super) fn draw_args(x: &Ext) -> Option<(u32, DrawArgs)> {
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
        Some((
            x.word(0),
            DrawArgs::ArraysIndirect {
                indirect: x.word64(1),
            },
        ))
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
