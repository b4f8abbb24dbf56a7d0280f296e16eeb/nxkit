//! The `Backend` of `nxkit-vk`.

use std::sync::Arc;

use nxkit_metal::{GuestMemory, Sampler, Texture};
use nxkit_vk::backend::{Backend, BackendError, BackendResult, Present, Submission};
use nxkit_vk::desc::{
    BufferViewDesc, ComputePipelineDesc, GraphicsPipelineDesc, ImageDesc, MemoryId, PipelineId,
    QueryPoolId, QueryType, SamplerDesc, SamplerId, ShaderKey, TextureId, ViewDesc,
};
use nxkit_vk::device::DeviceCaps;
use nxkit_vk::format::PixelFormat;

use super::{ADDRESS_LIMIT, MEMORY_HINTS, VkGpu, locked};
use crate::objects::Pipe;
use crate::{formats, make};

fn err(e: impl std::fmt::Display) -> BackendError {
    BackendError(e.to_string())
}

impl Backend for VkGpu {
    fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            // a render pass of `nxkit-metal` sets one viewport
            max_viewports: 1,
            rw_texture_tier: 2,
            surface_extent: self.settings.surface_extent,
        }
    }

    fn linear_alignment(&self, pixel: PixelFormat) -> u64 {
        formats::pixel(pixel)
            .and_then(|f| Texture::linear_alignment(&self.ctx, f))
            .unwrap_or(256)
    }

    fn create_memory(&self, id: MemoryId, size: u64, memory_type: u32) -> BackendResult<u64> {
        nxkit_metal::autorelease_pool(|| {
            let mem = GuestMemory::map_below(&self.ctx, size, ADDRESS_LIMIT, &MEMORY_HINTS)
                .map_err(|e| {
                    BackendError(format!(
                        "memory {:#x} of {size} bytes (type {memory_type}): {e}",
                        id.0
                    ))
                })?;
            let host = mem.host_base();
            locked(&self.objects).memories.insert(id, Arc::new(mem));
            Ok(host)
        })
    }

    fn destroy_memory(&self, id: MemoryId) {
        let mem = locked(&self.objects).memories.remove(&id);
        if let Some(m) = mem {
            let serial = locked(&self.queue).committed;
            locked(&self.retired).push((serial, m));
        }
        self.drop_retired();
    }

    fn create_image(&self, id: TextureId, desc: &ImageDesc) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| {
            let memory = desc
                .linear
                .and_then(|l| locked(&self.objects).memories.get(&l.memory).cloned());
            let tex = make::image(&self.ctx, desc, memory).map_err(err)?;
            let mut o = locked(&self.objects);
            if tex.converted.is_some() {
                o.converted.push(id);
            }
            o.textures.insert(id, Arc::new(tex));
            Ok(())
        })
    }

    fn create_view(&self, id: TextureId, image: TextureId, desc: &ViewDesc) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| {
            let img = locked(&self.objects)
                .textures
                .get(&image)
                .cloned()
                .ok_or_else(|| err("the image of the view has no texture"))?;
            let tex = make::view(&img, desc).map_err(err)?;
            locked(&self.objects).textures.insert(id, Arc::new(tex));
            Ok(())
        })
    }

    fn create_buffer_view(&self, _id: TextureId, desc: &BufferViewDesc) -> BackendResult<()> {
        Err(BackendError(format!(
            "texel buffer views (format {}) are not implemented",
            desc.format
        )))
    }

    fn destroy_texture(&self, id: TextureId) {
        let mut o = locked(&self.objects);
        if o.textures
            .remove(&id)
            .is_some_and(|t| t.converted.is_some())
        {
            o.converted.retain(|c| *c != id);
        }
    }

    fn create_sampler(&self, id: SamplerId, desc: &SamplerDesc) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| {
            if desc.unnormalized {
                self.log.once(
                    "unnormalized sampler",
                    "[RED] samplers with unnormalized coordinates are not implemented (normalized ones are made)",
                );
            }
            let s = Sampler::new(&self.ctx, &formats::sampler(desc)).map_err(err)?;
            locked(&self.objects).samplers.insert(id, s);
            Ok(())
        })
    }

    fn destroy_sampler(&self, id: SamplerId) {
        locked(&self.objects).samplers.remove(&id);
    }

    fn shader_module(&self, key: ShaderKey, code: &[u8]) {
        if !self.shaders.has(key) {
            self.log.once(
                format!("module {}", key.to_hex()),
                format_args!(
                    "[RED] shader {} ({} bytes of SPIR-V) is not in the shader table",
                    key.to_hex(),
                    code.len()
                ),
            );
        }
    }

    fn create_graphics_pipeline(
        &self,
        id: PipelineId,
        desc: &GraphicsPipelineDesc,
    ) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| {
            let (g, lacking) = make::graphics(&self.ctx, &self.shaders, &self.cache, desc)
                .map_err(|e| {
                    self.log.once(
                        format!("pipeline {e}"),
                        format_args!("[RED] graphics pipeline refused: {e}"),
                    );
                    err(e)
                })?;
            for l in lacking {
                self.log.once(
                    format!("lacking {l}"),
                    format_args!("[RED] not implemented: {l} (pipeline {})", g.name),
                );
            }
            locked(&self.objects)
                .pipelines
                .insert(id, Pipe::Graphics(Arc::new(g)));
            Ok(())
        })
    }

    fn create_compute_pipeline(
        &self,
        id: PipelineId,
        desc: &ComputePipelineDesc,
    ) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| {
            let c = make::compute(&self.shaders, &self.cache, desc).map_err(|e| {
                self.log.once(
                    format!("pipeline {e}"),
                    format_args!("[RED] compute pipeline refused: {e}"),
                );
                err(e)
            })?;
            locked(&self.objects)
                .pipelines
                .insert(id, Pipe::Compute(Arc::new(c)));
            Ok(())
        })
    }

    fn destroy_pipeline(&self, id: PipelineId) {
        locked(&self.objects).pipelines.remove(&id);
    }

    fn create_query_pool(&self, id: QueryPoolId, ty: QueryType, count: u32) -> BackendResult<()> {
        match ty {
            QueryType::Occlusion => {
                self.log.once(
                    "occlusion",
                    "[RED] occlusion queries are not counted: every query reports 1 (visible)",
                );
            }
            QueryType::Other(t) => {
                self.log.once(
                    format!("query {t}"),
                    format_args!("[RED] query type {t} is not implemented: its queries report 0"),
                );
            }
            QueryType::Timestamp => {}
        }
        locked(&self.queries).insert(id, vec![0; count as usize]);
        Ok(())
    }

    fn destroy_query_pool(&self, id: QueryPoolId) {
        locked(&self.queries).remove(&id);
    }

    fn query_value(&self, id: QueryPoolId, index: u32) -> u64 {
        locked(&self.queries)
            .get(&id)
            .and_then(|v| v.get(index as usize))
            .copied()
            .unwrap_or(0)
    }

    fn submit(&self, submission: &Submission) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| self.submit_now(submission))
    }

    fn completed(&self) -> u64 {
        self.done.completed()
    }

    fn wait(&self, serial: u64, timeout_ns: u64) -> bool {
        self.done.wait(serial, timeout_ns)
    }

    fn acquire(&self, image: TextureId, timeout_ns: u64) -> bool {
        self.acquire_now(image, timeout_ns)
    }

    fn present(&self, present: &Present) -> BackendResult<()> {
        nxkit_metal::autorelease_pool(|| self.present_now(present))
    }
}
