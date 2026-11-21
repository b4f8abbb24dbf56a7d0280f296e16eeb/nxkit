//! Render passes, framebuffers, shader modules, pipeline caches and pipelines.

use std::sync::Arc;

use crate::cmd::BindPoint;
use crate::consts::{dynamic_state as ds, stage};
use crate::desc::{
    ComputePipelineDesc, FramebufferAttachment, FramebufferDesc, GraphicsPipelineDesc, PipelineId,
    PipelineLayoutDesc, ShaderKey, StageDesc, Targets, TextureId,
};
use crate::info::{FramebufferInfo, GraphicsPipelineInfo, RenderPassInfo, StageInfo};
use crate::result::{Log, VkError, VkResult};

use super::Vk;
use super::state::{Pipeline, State};

/// The dynamic states that the backend implements; others are reported once.
const DYNAMIC: [u32; 9] = [
    ds::VIEWPORT,
    ds::SCISSOR,
    ds::LINE_WIDTH,
    ds::DEPTH_BIAS,
    ds::BLEND_CONSTANTS,
    ds::DEPTH_BOUNDS,
    ds::STENCIL_COMPARE_MASK,
    ds::STENCIL_WRITE_MASK,
    ds::STENCIL_REFERENCE,
];

fn stage_desc(log: &Log, s: &State, st: &StageInfo) -> Option<StageDesc> {
    let Some(key) = s.modules.get(st.module).copied() else {
        log.once(
            "pipeline module",
            "[RED] a pipeline stage names an unknown shader module",
        );
        return None;
    };
    Some(StageDesc {
        stage: st.stage,
        key,
        entry: st.entry.clone(),
        specialization: st.specialization.clone(),
    })
}

fn layout_of(s: &State, h: u64) -> Arc<PipelineLayoutDesc> {
    s.pipeline_layouts.get(h).cloned().unwrap_or_default()
}

impl Vk {
    /// `vkCreateRenderPass`. One subpass only: a render pass with more gives `VK_ERROR_FEATURE_NOT_PRESENT`. At most 16
    /// attachments and 8 colour attachments.
    pub fn create_render_pass(&self, info: &RenderPassInfo) -> VkResult<u64> {
        if info.subpasses == 0 || info.desc.attachments.len() > 16 {
            self.log.once(
                "render pass shape",
                format_args!(
                    "[RED] render pass: {} attachments, {} subpasses",
                    info.desc.attachments.len(),
                    info.subpasses
                ),
            );
            return Err(VkError::OutOfHostMemory);
        }
        if info.subpasses > 1 {
            let e = VkError::FeatureNotPresent;
            return Err(self.log.unsupported(
                "subpasses",
                e,
                format_args!("render pass with {} subpasses", info.subpasses),
            ));
        }
        if info.input_attachments > 0 {
            self.log.once(
                "input attachments",
                "[RED] input attachments not implemented",
            );
        }
        let mut d = info.desc.clone();
        d.colors.truncate(8);
        d.resolves.truncate(8);
        Ok(self.lock().render_passes.insert(Arc::new(d)))
    }

    /// `vkDestroyRenderPass`: recorded commands and pipelines keep what they use.
    pub fn destroy_render_pass(&self, pass: u64) {
        self.lock().render_passes.remove(pass);
    }

    /// `vkCreateFramebuffer`: the views, resolved to their textures. At most 16 attachments.
    pub fn create_framebuffer(&self, info: &FramebufferInfo) -> VkResult<u64> {
        if info.views.len() > 16 {
            self.log.once(
                "framebuffer size",
                format_args!("[RED] framebuffer with {} attachments", info.views.len()),
            );
            return Err(VkError::OutOfHostMemory);
        }
        let mut s = self.lock();
        let attachments = info
            .views
            .iter()
            .map(|&v| match s.views.get(v) {
                Some(view) => FramebufferAttachment {
                    view: TextureId(v),
                    image: TextureId(view.image),
                    format: view.desc.format,
                    samples: view.samples,
                    level: view.desc.base_level,
                    layer: view.desc.base_layer,
                },
                None => {
                    self.log.once(
                        "framebuffer view",
                        "[RED] a framebuffer names an unknown image view",
                    );
                    FramebufferAttachment::default()
                }
            })
            .collect();
        Ok(s.framebuffers.insert(Arc::new(FramebufferDesc {
            attachments,
            width: info.width,
            height: info.height,
            layers: info.layers.max(1),
        })))
    }

    /// `vkDestroyFramebuffer`.
    pub fn destroy_framebuffer(&self, framebuffer: u64) {
        self.lock().framebuffers.remove(framebuffer);
    }

    /// `vkCreateShaderModule`: the key is the first 8 bytes of the SHA-256 of the SPIR-V.
    pub fn create_shader_module(&self, code: &[u8]) -> u64 {
        let key = ShaderKey::of(code);
        self.backend.shader_module(key, code);
        self.lock().modules.insert(key)
    }

    /// The key of a shader module.
    pub fn shader_key(&self, module: u64) -> Option<ShaderKey> {
        self.lock().modules.get(module).copied()
    }

    /// `vkDestroyShaderModule`: pipelines keep the key.
    pub fn destroy_shader_module(&self, module: u64) {
        self.lock().modules.remove(module);
    }

    /// `vkCreatePipelineCache`: caches hold nothing (the game's initial data is NVIDIA's binary cache, and Metal keeps
    /// its own). `vkGetPipelineCacheData` gives 0 bytes and `vkMergePipelineCaches` does nothing.
    pub fn create_pipeline_cache(&self) -> u64 {
        self.lock().caches.insert(())
    }

    /// `vkDestroyPipelineCache`.
    pub fn destroy_pipeline_cache(&self, cache: u64) {
        self.lock().caches.remove(cache);
    }

    /// `vkCreateGraphicsPipelines`. A pipeline that the backend refuses is still handed out; its draws are skipped.
    pub fn create_graphics_pipelines(&self, infos: &[GraphicsPipelineInfo]) -> VkResult<Vec<u64>> {
        let mut made = Vec::with_capacity(infos.len());
        for info in infos {
            let desc = {
                let s = self.lock();
                let stages = info
                    .stages
                    .iter()
                    .filter_map(|st| stage_desc(&self.log, &s, st))
                    .collect();
                let targets = match s.render_passes.get(info.render_pass) {
                    Some(rp) => Targets {
                        colors: rp.color_formats(),
                        depth: rp.depth_format(),
                    },
                    None => {
                        self.log.once(
                            "pipeline pass",
                            "[RED] a graphics pipeline names an unknown render pass",
                        );
                        Targets::default()
                    }
                };
                GraphicsPipelineDesc {
                    stages,
                    state: info.state.clone(),
                    targets,
                    layout: layout_of(&s, info.layout),
                }
            };
            self.check_graphics(info, &desc);
            let h = self.lock().pipelines.insert(Pipeline {
                bind: BindPoint::Graphics,
            });
            if let Err(e) = self.backend.create_graphics_pipeline(PipelineId(h), &desc) {
                self.log.once(
                    "graphics pipeline",
                    format_args!("[RED] cannot build a graphics pipeline: {e}"),
                );
            }
            made.push(h);
        }
        Ok(made)
    }

    /// Reports once what a graphics pipeline asks for and the layer does not do.
    fn check_graphics(&self, info: &GraphicsPipelineInfo, desc: &GraphicsPipelineDesc) {
        if info.subpass != 0 {
            self.log.once(
                "pipeline subpass",
                format_args!("[RED] pipeline uses subpass {}", info.subpass),
            );
        }
        for d in &desc.state.dynamic {
            if !DYNAMIC.contains(d) {
                self.log.once(
                    "dynamic state",
                    format_args!("[RED] dynamic state {d} not implemented"),
                );
            }
        }
        if desc.state.viewport_count > self.caps.max_viewports {
            self.log.once(
                "viewports",
                format_args!(
                    "[RED] pipeline with {} viewports, over {}",
                    desc.state.viewport_count, self.caps.max_viewports
                ),
            );
        }
        if desc.state.raster.cull_mode == 3 {
            self.log.once(
                "cull both",
                "[RED] culling both front and back faces not implemented",
            );
        }
        if desc.state.depth_stencil.is_some_and(|d| d.bounds_test) {
            self.log
                .once("depth bounds", "[RED] depth bounds test not implemented");
        }
        for st in &desc.stages {
            if st.stage != stage::VERTEX
                && st.stage != stage::FRAGMENT
                && st.stage != stage::GEOMETRY
            {
                self.log.once(
                    "graphics stage",
                    format_args!("[RED] shader stage {:#x} not implemented", st.stage),
                );
            }
        }
        if !desc.stages.iter().any(|s| s.stage == stage::VERTEX) {
            self.log.once(
                "no vertex stage",
                "[RED] a graphics pipeline has no usable vertex shader",
            );
        }
    }

    /// `vkCreateComputePipelines`: as for graphics, a pipeline that the backend refuses is handed out and its
    /// dispatches are skipped.
    pub fn create_compute_pipelines(&self, infos: &[(StageInfo, u64)]) -> VkResult<Vec<u64>> {
        let mut made = Vec::with_capacity(infos.len());
        for (st, layout) in infos {
            let (stage_d, layout) = {
                let s = self.lock();
                (stage_desc(&self.log, &s, st), layout_of(&s, *layout))
            };
            let h = self.lock().pipelines.insert(Pipeline {
                bind: BindPoint::Compute,
            });
            match stage_d {
                Some(stage_d) if stage_d.stage == stage::COMPUTE => {
                    let desc = ComputePipelineDesc {
                        stage: stage_d,
                        layout,
                    };
                    if let Err(e) = self.backend.create_compute_pipeline(PipelineId(h), &desc) {
                        self.log.once(
                            "compute pipeline",
                            format_args!("[RED] cannot build a compute pipeline: {e}"),
                        );
                    }
                }
                _ => self.log.once(
                    "compute stage",
                    "[RED] a compute pipeline has no compute shader",
                ),
            }
            made.push(h);
        }
        Ok(made)
    }

    /// `vkDestroyPipeline`.
    pub fn destroy_pipeline(&self, pipeline: u64) {
        if self.lock().pipelines.remove(pipeline).is_some() {
            self.backend.destroy_pipeline(PipelineId(pipeline));
        }
    }
}
