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
        let hit = LAST_PIPELINE.with(|l| {
            l.borrow()
                .as_ref()
                .filter(|m| m.matches(self.id, d, targets, icb))
                .map(|m| (m.pipeline.clone(), m.key))
        });
        if let Some(hit) = hit {
            return Ok(hit);
        }
        let (pipeline, key) = self.render_uncached(d, vs, fs, targets, icb)?;
        LAST_PIPELINE.with(|l| {
            *l.borrow_mut() = Some(Last {
                owner: self.id,
                icb,
                topology: d.topology,
                alpha_to_coverage: d.raster.alpha_to_coverage,
                program: d.program.clone(),
                targets: targets.clone(),
                blend: d.blend,
                write_masks: d.write_masks,
                attributes: d.attributes.clone(),
                streams: d
                    .streams
                    .iter()
                    .map(|s| (s.index, s.stride, s.divisor))
                    .collect(),
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
        let mut desc = RenderPipelineDesc::new(
            (&vs.lib, vs.entry.as_str()),
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
        desc.vertex_layout = translate::vertex_descriptor(d, &vs.inputs).map_err(|e| {
            nxkit_nvn::diag::example(
                nxkit_nvn::diag::Reason::SkipFormat,
                format_args!("vertex {e:?} vs {:?}", d.program.vertex),
            );
            Skip::Format
        })?;
        desc.topology = class;
        desc.sample_count = targets.samples;
        desc.alpha_to_coverage = d.raster.alpha_to_coverage;
        desc.support_icb = icb;
        let mut h = DefaultHasher::new();
        (
            &d.program.vertex,
            d.program.fragment.as_deref(),
            targets,
            icb,
        )
            .hash(&mut h);
        desc.colors.hash(&mut h);
        desc.vertex_layout.hash(&mut h);
        class.hash(&mut h);
        d.raster.alpha_to_coverage.hash(&mut h);
        flat.hash(&mut h);
        paint.hash(&mut h);
        paint_dir.hash(&mut h);
        let key = h.finish();
        if async_on() && !icb {
            let fallback = self
                .topology_fallbacks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains(&key);
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
            let job = Job {
                key,
                what: format!("render {} / {:?}", vs.entry, fs.map(|f| f.entry.as_str())),
                kind: JobKind::Render(RenderJob {
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
        let what = || format!("render {} / {:?}", vs.entry, fs.map(|f| f.entry.as_str()));
        let (p, source) = build_render(&self.cache, &self.topology_fallbacks, key, &mut desc)
            .map_err(|e| pipe_failed(format_args!("{}: {e}", what())))?;
        crate::coverage::record(&self.cache, source, Some(&desc), &what());
        Ok((p, key))
    }

    /// The mesh pipeline of a task and mesh draw. `ts` is the task shader, when the program has one, and `fs` the pixel
    /// shader; without one the pipeline draws depth only. The pipeline compiles in the background like a render
    /// pipeline: until it is ready the draw is left out with `Skip::Pending`.
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
        let info = |p: &StageProgram| p.mesh.ok_or(Skip::Program);
        let mi = info(ms)?;
        let ti = ts.map(info).transpose()?;
        let mut h = DefaultHasher::new();
        (
            "mesh",
            d.program.task.as_deref(),
            d.program.mesh.as_deref(),
            d.program.fragment.as_deref(),
            fragment.map(|(_, e)| e),
            targets,
        )
            .hash(&mut h);
        colors.hash(&mut h);
        d.raster.alpha_to_coverage.hash(&mut h);
        let key = h.finish();
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
            mesh: (ms.lib.clone(), ms.entry.clone()),
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
        crate::coverage::record(&self.cache, source, None, &what());
        Ok((p, key))
    }
}
