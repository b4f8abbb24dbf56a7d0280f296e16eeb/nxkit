//! Render passes, framebuffers, shader modules, pipeline caches, pipelines, set layouts, pipeline layouts,
//! descriptor pools, descriptor sets and their updates, and update templates.
//!
//! A pipeline cache holds nothing (the backend keeps a cache of its own): its data is 0 bytes and a merge does nothing.

use nxkit_vk::info;
use nxkit_vk::mem::Mem;
use nxkit_vk::result::Status;

use super::abi::{M, call, code, handle_out, handles_out, ok, procs};

extern "C" fn create_render_pass(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateRenderPass", |vk| {
        handle_out(out, vk.create_render_pass(&info::render_pass(&M, ci)?))
    })
}

extern "C" fn destroy_render_pass(_device: u64, pass: u64, _alloc: u64) {
    call("vkDestroyRenderPass", |vk| {
        vk.destroy_render_pass(pass);
        Ok(())
    })
}

extern "C" fn create_framebuffer(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateFramebuffer", |vk| {
        handle_out(out, vk.create_framebuffer(&info::framebuffer(&M, ci)?))
    })
}

extern "C" fn destroy_framebuffer(_device: u64, framebuffer: u64, _alloc: u64) {
    call("vkDestroyFramebuffer", |vk| {
        vk.destroy_framebuffer(framebuffer);
        Ok(())
    })
}

extern "C" fn create_shader_module(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateShaderModule", |vk| {
        handle_out(
            out,
            Ok(vk.create_shader_module(&info::shader_code(&M, ci)?)),
        )
    })
}

extern "C" fn destroy_shader_module(_device: u64, module: u64, _alloc: u64) {
    call("vkDestroyShaderModule", |vk| {
        vk.destroy_shader_module(module);
        Ok(())
    })
}

extern "C" fn create_pipeline_cache(_device: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreatePipelineCache", |vk| {
        handle_out(out, Ok(vk.create_pipeline_cache()))
    })
}

extern "C" fn destroy_pipeline_cache(_device: u64, cache: u64, _alloc: u64) {
    call("vkDestroyPipelineCache", |vk| {
        vk.destroy_pipeline_cache(cache);
        Ok(())
    })
}

/// `vkGetPipelineCacheData(device, cache, size_t *pDataSize, void *pData)`: 0 bytes.
extern "C" fn get_pipeline_cache_data(_device: u64, _cache: u64, size: u64, _data: u64) -> i32 {
    code("vkGetPipelineCacheData", |_| {
        M.put_u64(size, 0)?;
        Ok(Status::Success)
    })
}

extern "C" fn merge_pipeline_caches(_device: u64, _dst: u64, _count: u32, _src: u64) -> i32 {
    code("vkMergePipelineCaches", |_| Ok(Status::Success))
}

extern "C" fn create_graphics_pipelines(
    _device: u64,
    _cache: u64,
    count: u32,
    infos: u64,
    _alloc: u64,
    out: u64,
) -> i32 {
    code("vkCreateGraphicsPipelines", |vk| {
        let infos = info::graphics_pipelines(&M, infos, count)?;
        handles_out(out, vk.create_graphics_pipelines(&infos))
    })
}

extern "C" fn create_compute_pipelines(
    _device: u64,
    _cache: u64,
    count: u32,
    infos: u64,
    _alloc: u64,
    out: u64,
) -> i32 {
    code("vkCreateComputePipelines", |vk| {
        let infos = info::compute_pipelines(&M, infos, count)?;
        handles_out(out, vk.create_compute_pipelines(&infos))
    })
}

extern "C" fn destroy_pipeline(_device: u64, pipeline: u64, _alloc: u64) {
    call("vkDestroyPipeline", |vk| {
        vk.destroy_pipeline(pipeline);
        Ok(())
    })
}

extern "C" fn create_pipeline_layout(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreatePipelineLayout", |vk| {
        let (sets, ranges) = info::pipeline_layout(&M, ci)?;
        handle_out(out, vk.create_pipeline_layout(&sets, &ranges))
    })
}

extern "C" fn destroy_pipeline_layout(_device: u64, layout: u64, _alloc: u64) {
    call("vkDestroyPipelineLayout", |vk| {
        vk.destroy_pipeline_layout(layout);
        Ok(())
    })
}

extern "C" fn create_set_layout(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateDescriptorSetLayout", |vk| {
        handle_out(out, vk.create_set_layout(&info::set_layout(&M, ci)?))
    })
}

extern "C" fn destroy_set_layout(_device: u64, layout: u64, _alloc: u64) {
    call("vkDestroyDescriptorSetLayout", |vk| {
        vk.destroy_set_layout(layout);
        Ok(())
    })
}

extern "C" fn create_descriptor_pool(_device: u64, _ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateDescriptorPool", |vk| {
        handle_out(out, Ok(vk.create_descriptor_pool()))
    })
}

extern "C" fn destroy_descriptor_pool(_device: u64, pool: u64, _alloc: u64) {
    call("vkDestroyDescriptorPool", |vk| {
        vk.destroy_descriptor_pool(pool);
        Ok(())
    })
}

extern "C" fn reset_descriptor_pool(_device: u64, pool: u64, _flags: u32) -> i32 {
    code("vkResetDescriptorPool", |vk| {
        ok(vk.reset_descriptor_pool(pool))
    })
}

extern "C" fn allocate_descriptor_sets(_device: u64, ai: u64, out: u64) -> i32 {
    code("vkAllocateDescriptorSets", |vk| {
        let (pool, layouts) = info::set_allocate(&M, ai)?;
        handles_out(out, vk.allocate_sets(pool, &layouts))
    })
}

extern "C" fn free_descriptor_sets(_device: u64, _pool: u64, count: u32, sets: u64) -> i32 {
    code("vkFreeDescriptorSets", |vk| {
        vk.free_sets(&info::handles(&M, sets, count)?);
        Ok(Status::Success)
    })
}

extern "C" fn update_descriptor_sets(
    _device: u64,
    write_count: u32,
    writes: u64,
    copy_count: u32,
    copies: u64,
) {
    call("vkUpdateDescriptorSets", |vk| {
        let w = info::writes(&M, writes, write_count)?;
        let c = info::copies(&M, copies, copy_count)?;
        vk.update_sets(&w, &c);
        Ok(())
    })
}

extern "C" fn create_update_template(_device: u64, ci: u64, _alloc: u64, out: u64) -> i32 {
    code("vkCreateDescriptorUpdateTemplate", |vk| {
        let (ty, entries) = info::update_template(&M, ci)?;
        handle_out(out, Ok(vk.create_update_template(ty, entries)))
    })
}

extern "C" fn destroy_update_template(_device: u64, template: u64, _alloc: u64) {
    call("vkDestroyDescriptorUpdateTemplate", |vk| {
        vk.destroy_update_template(template);
        Ok(())
    })
}

extern "C" fn update_set_with_template(_device: u64, set: u64, template: u64, data: u64) {
    call("vkUpdateDescriptorSetWithTemplate", |vk| {
        vk.update_set_with_template(&M, set, template, data)
    })
}

pub(super) fn procs() -> Vec<super::Proc> {
    procs![
        "vkCreateRenderPass" => create_render_pass,
        "vkDestroyRenderPass" => destroy_render_pass,
        "vkCreateFramebuffer" => create_framebuffer,
        "vkDestroyFramebuffer" => destroy_framebuffer,
        "vkCreateShaderModule" => create_shader_module [big],
        "vkDestroyShaderModule" => destroy_shader_module,
        "vkCreatePipelineCache" => create_pipeline_cache,
        "vkDestroyPipelineCache" => destroy_pipeline_cache,
        "vkGetPipelineCacheData" => get_pipeline_cache_data,
        "vkMergePipelineCaches" => merge_pipeline_caches,
        "vkCreateGraphicsPipelines" => create_graphics_pipelines [big],
        "vkCreateComputePipelines" => create_compute_pipelines [big],
        "vkDestroyPipeline" => destroy_pipeline [big],
        "vkCreatePipelineLayout" => create_pipeline_layout,
        "vkDestroyPipelineLayout" => destroy_pipeline_layout,
        "vkCreateDescriptorSetLayout" => create_set_layout,
        "vkDestroyDescriptorSetLayout" => destroy_set_layout,
        "vkCreateDescriptorPool" => create_descriptor_pool,
        "vkDestroyDescriptorPool" => destroy_descriptor_pool,
        "vkResetDescriptorPool" => reset_descriptor_pool,
        "vkAllocateDescriptorSets" => allocate_descriptor_sets,
        "vkFreeDescriptorSets" => free_descriptor_sets,
        "vkUpdateDescriptorSets" => update_descriptor_sets [big],
        "vkCreateDescriptorUpdateTemplate" => create_update_template,
        "vkCreateDescriptorUpdateTemplateKHR" => create_update_template,
        "vkDestroyDescriptorUpdateTemplate" => destroy_update_template,
        "vkDestroyDescriptorUpdateTemplateKHR" => destroy_update_template,
        "vkUpdateDescriptorSetWithTemplate" => update_set_with_template [big],
        "vkUpdateDescriptorSetWithTemplateKHR" => update_set_with_template [big],
    ]
}
