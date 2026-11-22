//! The render and mesh pipelines of a draw: the memo, the cache lookup and the background builds.

use super::*;

impl Pipelines {
    /// The pipeline of a draw. `fs` is `None` for a draw without a fragment shader. `icb` asks for a pipeline that an
    /// indirect command buffer may use. A draw whose translated state equals that of the previous draw on the same
    /// thread gets the previous pipeline without a lookup.
    pub(crate) fn render(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let zero_to_one = self.depth_zero_to_one();
        let swaps = (vs.swap, fs.map_or(0, |f| f.swap));
        let hit = LAST_PIPELINE.with(|l| {
            l.borrow()
                .as_ref()
                .filter(|m| m.matches(self.id, d, targets, icb, zero_to_one, swaps))
                .map(|m| (m.pipeline.clone(), m.key))
        });
        if let Some(hit) = hit {
            return Ok(hit);
        }
        let (pipeline, key) = self.render_uncached(d, vs, fs, targets, icb, zero_to_one)?;
        LAST_PIPELINE.with(|l| {
            *l.borrow_mut() = Some(Last {
                owner: self.id,
                icb,
                zero_to_one,
                topology: d.topology,
                alpha_to_coverage: d.raster.alpha_to_coverage,
                program: d.program.clone(),
                targets: targets.clone(),
                blend: d.blend,
                write_masks: d.write_masks,
                pull: vs.pull,
                swaps,
                // A pulling pipeline has no vertex layout: the memo does not keep it.
                attributes: if vs.pull {
                    Vec::new()
                } else {
                    d.attributes.clone()
                },
                streams: if vs.pull {
                    Vec::new()
                } else {
                    d.streams
                        .iter()
                        .map(|s| (s.index, s.stride, s.divisor))
                        .collect()
                },
                pipeline: pipeline.clone(),
                key,
            });
        });
        Ok((pipeline, key))
    }

    fn render_uncached(
        &self,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
        icb: bool,
        zero_to_one: bool,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let class = translate::primitive(d.topology).ok_or(Skip::Primitive)?.1;
        let flat = crate::knob("flat")
            && targets.colors.len() >= 4
            && targets.colors[..4].iter().all(Option::is_some)
            && fs.is_some_and(|f| f.colors.as_deref() == Some(&[0, 1, 2, 3]));
        let flat_lib = if flat {
            let mut library = self.flat.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-flat-gbuffer", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct Out { float4 c0 [[color(0)]]; float4 c1 [[color(1)]];
                        float4 c2 [[color(2)]]; float4 c3 [[color(3)]]; };
                    fragment Out flat_gbuffer() { return { float4(0.5, 0.5, 1, 1), float4(1), float4(0.5), float4(0) }; }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else {
            None
        };
        let paint_dir = crate::knob("paintdir");
        let paint = (crate::knob("paint") || paint_dir)
            && targets.colors.len() == 1
            && targets.colors[0].is_some()
            && fs.is_some()
            && {
                let keys = crate::diag::paint_programs();
                keys.is_empty()
                    || d.program
                        .fragment
                        .as_deref()
                        .is_some_and(|f| keys.iter().any(|k| k == f))
            };
        let paint_lib = if paint {
            let mut library = self.paint.lock().unwrap_or_else(PoisonError::into_inner);
            if library.is_none() {
                *library = Some(ShaderLibrary::from_source(&self.ctx, "nxkit-paint", r#"
                    #include <metal_stdlib>
                    using namespace metal;
                    struct In { float4 loc0 [[user(locn0)]]; };
                    fragment float4 paint_magenta() { return float4(8.0, 0.0, 8.0, 1.0); }
                    fragment float4 paint_dir(In in [[stage_in]]) { return float4(abs(in.loc0.xyz), 1.0); }
                "#).map_err(|_| Skip::Pipeline)?);
            }
            library.clone()
        } else {
            None
        };
        let (vertex_fn, vertex_kind) = vertex_entry(d, vs, zero_to_one);
        let mut desc = RenderPipelineDesc::new(
            (&vs.lib, vertex_fn),
            paint_lib
                .as_ref()
                .map(|lib| {
                    (
                        lib,
                        if paint_dir {
                            "paint_dir"
                        } else {
                            "paint_magenta"
                        },
                    )
                })
                .or_else(|| flat_lib.as_ref().map(|lib| (lib, "flat_gbuffer")))
                .or_else(|| fs.map(|f| (&f.lib, fragment_entry(f, targets)))),
            &[],
        );
        desc.colors = color_targets(d, fs, targets);
        // A vertex function that pulls reads the streams itself (`crate::pull`): the pipeline has no vertex layout, so
        // one base per pair serves every layout and the archive can hold it.
        desc.vertex_layout = if vs.pull {
            nxkit_metal::VertexDescriptor::default()
        } else {
            translate::vertex_descriptor(d, &vs.inputs).map_err(|e| {
                nxkit_nvn::diag::example(
                    nxkit_nvn::diag::Reason::SkipFormat,
                    format_args!("vertex {e:?} vs {:?}", d.program.vertex),
                );
                Skip::Format
            })?
        };
        desc.topology = class;
        desc.sample_count = targets.samples;
        desc.alpha_to_coverage = d.raster.alpha_to_coverage;
        desc.support_icb = icb;
        // The layer entry and the entries that keep the depth are other vertex functions: their pipelines have keys
        // of their own.
        let other_key;
        let vertex_key: &str = if vertex_kind.is_empty() {
            &d.program.vertex
        } else {
            other_key = format!("{}{vertex_kind}", d.program.vertex);
            &other_key
        };
        let key = render_key(&RenderKey {
            vertex: vertex_key,
            fragment: d.program.fragment.as_deref(),
            targets,
            icb,
            colors: &desc.colors,
            layout: &desc.vertex_layout,
            pull: vs.pull,
            class,
            alpha_to_coverage: d.raster.alpha_to_coverage,
            flat,
            paint,
            paint_dir,
        });
        let vertex = point_size_key(&d.program.vertex, desc.vertex.1);
        if vs.swap != 0 || fs.is_some_and(|f| f.swap != 0) {
            let key = swapped_key(key, vs.swap, fs.map_or(0, |f| f.swap));
            return self.render_swapped(key, d, vs, fs, &mut desc);
        }
        if async_on() && !icb {
            let fallback =
                class != TopologyClass::Point && falls_back(&self.topology_fallbacks, key, vertex);
            if let Some(p) =
                self.cache
                    .cached_render(if fallback { key ^ FALLBACK_KEY } else { key })
            {
                return Ok((p, key));
            }
            if self
                .builds
                .failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&key)
            {
                return Err(Skip::Pipeline);
            }
            stats::pending_draw();
            if self.builds.touch(key) {
                return Err(Skip::Pending);
            }
            let job = Job {
                key,
                what: render_what(d, vs, fs),
                kind: JobKind::Render(RenderJob {
                    vertex_key: vertex,
                    vertex: (desc.vertex.0.clone(), desc.vertex.1.to_string()),
                    fragment: desc.fragment.map(|(l, e)| (l.clone(), e.to_string())),
                    colors: desc.colors.clone(),
                    vertex_layout: desc.vertex_layout.clone(),
                    topology: desc.topology,
                    sample_count: desc.sample_count,
                    alpha_to_coverage: desc.alpha_to_coverage,
                }),
            };
            self.builds
                .start(&self.cache, &self.topology_fallbacks, job);
            return Err(Skip::Pending);
        }
        let what = || render_what(d, vs, fs);
        let (p, source) = build_render(
            &self.cache,
            &self.topology_fallbacks,
            key,
            vertex,
            &mut desc,
        )
        .map_err(|e| pipe_failed(format_args!("{}: {e}", what())))?;
        crate::pipelines::coverage::record(&self.cache, source, Some(&desc), &what());
        Ok((p, key))
    }

    /// The pipeline of a draw with a swapped library (W36). It compiles at once on a compiler without a serializer, so
    /// the next frame draws with the new code, and no archive and no list of recorded states gets it.
    fn render_swapped(
        &self,
        key: u64,
        d: &DrawDetail,
        vs: &StageProgram,
        fs: Option<&StageProgram>,
        desc: &mut RenderPipelineDesc<'_>,
    ) -> Result<(RenderPipeline, u64), Skip> {
        if let Some(p) = self.cache.cached_render(key) {
            return Ok((p, key));
        }
        let made = self.cache.render_scratch(key, desc).or_else(|e| {
            // The point size rule of `build_render`.
            if desc.topology == TopologyClass::Point || !e.to_string().contains("point size") {
                return Err(e);
            }
            desc.topology = TopologyClass::Unspecified;
            self.cache.render_scratch(key, desc)
        });
        made.map(|(p, _)| (p, key))
            .map_err(|e| pipe_failed(format_args!("{} (swapped): {e}", render_what(d, vs, fs))))
    }

    /// The mesh pipeline of a task and mesh draw. `ts` is the task shader, when the program has one, and `fs` the pixel
    /// shader. Without a pixel shader, the pipeline draws depth only. The pipeline compiles in the background like a
    /// render pipeline: until it is ready the draw is left out with `Skip::Pending`.
    pub(crate) fn mesh(
        &self,
        d: &DrawDetail,
        ts: Option<&StageProgram>,
        ms: &StageProgram,
        fs: Option<&StageProgram>,
        targets: &Targets,
    ) -> Result<(RenderPipeline, u64), Skip> {
        let colors = color_targets(d, fs, targets);
        let fragment = fs.map(|f| (&f.lib, fragment_entry(f, targets)));
        let mesh_fn = mesh_entry(ms, self.depth_zero_to_one());
        let info = |p: &StageProgram| p.mesh.ok_or(Skip::Program);
        let mi = info(ms)?;
        let ti = ts.map(info).transpose()?;
        let key = mesh_key(&MeshKey {
            task: d.program.task.as_deref(),
            mesh: d.program.mesh.as_deref(),
            fragment: d.program.fragment.as_deref(),
            fragment_entry: fragment.map(|(_, e)| e),
            targets,
            colors: &colors,
            alpha_to_coverage: d.raster.alpha_to_coverage,
            keeps_depth: mesh_fn != ms.entry,
        });
        if let Some(p) = self.cache.cached_render(key) {
            return Ok((p, key));
        }
        if self
            .builds
            .failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&key)
        {
            return Err(Skip::Pipeline);
        }
        let job = MeshJob {
            object: ts.map(|t| (t.lib.clone(), t.entry.clone())),
            mesh: (ms.lib.clone(), mesh_fn.to_owned()),
            fragment: fragment.map(|(l, e)| (l.clone(), e.to_owned())),
            colors,
            sample_count: targets.samples,
            alpha_to_coverage: d.raster.alpha_to_coverage,
            object_threads: ti.map_or(0, |t| t.threads),
            mesh_threads: mi.threads,
            payload_bytes: ti.map_or(0, |t| t.payload),
        };
        let what = || {
            format!(
                "mesh task {:?} mesh {:?} fragment {:?}",
                d.program.task.as_deref(),
                d.program.mesh.as_deref(),
                d.program.fragment.as_deref()
            )
        };
        if async_on() {
            stats::pending_draw();
            if self.builds.touch(key) {
                return Err(Skip::Pending);
            }
            let job = Job {
                key,
                what: what(),
                kind: JobKind::Mesh(job),
            };
            self.builds
                .start(&self.cache, &self.topology_fallbacks, job);
            return Err(Skip::Pending);
        }
        let (p, source) = self
            .cache
            .mesh_traced(key, &job.desc())
            .map_err(|e| pipe_failed(format_args!("{}: {e}", what())))?;
        record::mesh(key, &job.desc(), source);
        crate::pipelines::coverage::record(&self.cache, source, None, &what());
        Ok((p, key))
    }
}

/// True when the draw has a geometry shader that may only pick the layer and its vertex shader has a layer entry: the
/// draw runs the layer entry in a layered pass and no geometry stage (`docs/nxkit-gpu.md`, "Geometry shaders that only
/// pick the layer").
pub(crate) fn layered(d: &DrawDetail, vs: &StageProgram) -> bool {
    d.program.geometry.is_some() && vs.entry_layer.is_some()
}

/// The vertex function of the draw and the suffix of its pipeline key: the layer entry (`:layer`) for a [`layered`]
/// draw, the base entry otherwise. With the device in the depth mode 0 to 1 (`zero_to_one`) it is the form of the
/// entry that keeps the clip depth (`main0_z`, `main0_lz`, suffix `:z`). The base forms convert the -1 to 1 depth of
/// the default mode.
pub(super) fn vertex_entry<'a>(
    d: &DrawDetail,
    vs: &'a StageProgram,
    zero_to_one: bool,
) -> (&'a str, &'static str) {
    let layer = match (&d.program.geometry, &vs.entry_layer) {
        (Some(_), Some(_)) => true,
        (Some(g), None) => {
            nxkit_nvn::diag::note(
                nxkit_nvn::diag::Reason::StageIgnored,
                format_args!(
                    "geometry stage {g} of vertex {} without a layer entry",
                    d.program.vertex
                ),
            );
            false
        }
        _ => false,
    };
    let e = nxkit_pipestate::draw::vertex_entry(
        layer,
        &vs.entry,
        vs.entry_layer.as_deref(),
        [vs.entry_z[0].as_deref(), vs.entry_z[1].as_deref()],
        zero_to_one,
    );
    if e.depth_mode_ignored {
        depth_mode_ignored(&d.program.vertex);
    }
    (e.entry, e.suffix)
}

/// The mesh function of a mesh draw: `main0_z` with the device in the depth mode 0 to 1, see [`vertex_entry`].
fn mesh_entry(ms: &StageProgram, zero_to_one: bool) -> &str {
    let (entry, ignored) =
        nxkit_pipestate::draw::mesh_entry(&ms.entry, ms.entry_z[0].as_deref(), zero_to_one);
    if ignored {
        depth_mode_ignored(&ms.entry);
    }
    entry
}

/// Counts a draw that converts the depth although the device keeps it (`Reason::DepthModeIgnored`).
fn depth_mode_ignored(what: &str) {
    nxkit_nvn::diag::note(
        nxkit_nvn::diag::Reason::DepthModeIgnored,
        format_args!("{what} has no entry that keeps the clip depth"),
    );
}

/// The render pipeline of a draw for the health report and the `pipe` lines: the entries and the program keys.
fn render_what(d: &DrawDetail, vs: &StageProgram, fs: Option<&StageProgram>) -> String {
    format!(
        "render {} / {:?} (vertex {} fragment {:?})",
        vs.entry,
        fs.map(|f| f.entry.as_str()),
        d.program.vertex,
        d.program.fragment.as_deref()
    )
}
