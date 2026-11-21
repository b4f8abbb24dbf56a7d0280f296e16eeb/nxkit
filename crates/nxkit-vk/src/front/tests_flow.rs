//! Tests of recording, submits, sync objects, queries, the swapchain and pipelines decoded from guest memory.

use std::sync::Arc;

use crate::cmd::{BindPoint, Cmd};
use crate::consts::{descriptor_type as dt, image_usage, query_result as qr};
use crate::desc::{Descriptor, PipelineId, QueryPoolId, Rect2D, TextureId, Viewport};
use crate::info::{
    self, FramebufferInfo, RenderPassBeginInfo, RenderPassInfo, SetLayoutBindingInfo, SwapchainInfo,
};
use crate::layout::{
    graphics_pipeline_create_info as gpc, pipeline_color_blend_attachment_state as pcba,
    pipeline_color_blend_state_create_info as pcb,
    pipeline_input_assembly_state_create_info as pia, pipeline_shader_stage_create_info as pss,
    pipeline_vertex_input_state_create_info as pvi, specialization_info as si,
    specialization_map_entry as sme, vertex_input_attribute_description as via,
    vertex_input_binding_description as vib,
};
use crate::mem::{Mem, StructBytes, TestMemory};
use crate::recording::Call;
use crate::result::{Status, VkError};

use super::tests::{image_2d, vk};

#[test]
fn a_recorded_frame_reaches_the_backend_with_its_sets_and_fence() {
    let (vk, rec, _) = vk();
    let mem = vk.allocate_memory(1 << 16, 1).unwrap_or(0);
    let vb = vk.create_buffer(&info::BufferInfo {
        size: 4096,
        usage: 0x80,
        flags: 0,
    });
    assert!(vk.bind_buffer_memory(vb, mem, 1024).is_ok());
    let img = vk
        .create_image(&image_2d(37, 64, 64, image_usage::COLOR_ATTACHMENT))
        .unwrap_or(0);
    let view = vk
        .create_image_view(&info::ImageViewInfo {
            image: img,
            view_type: 1,
            format: 37,
            range: info::SubresourceRange {
                aspect: 1,
                base_level: 0,
                levels: 1,
                base_layer: 0,
                layers: 1,
            },
            ..Default::default()
        })
        .unwrap_or(0);
    let mut rp = RenderPassInfo {
        subpasses: 1,
        ..Default::default()
    };
    rp.desc.attachments.push(crate::desc::AttachmentDesc {
        format: 37,
        samples: 1,
        load: 1,
        store: 0,
        stencil_load: 2,
        stencil_store: 1,
    });
    rp.desc.colors.push(Some(0));
    let pass = vk.create_render_pass(&rp).unwrap_or(0);
    let fb = vk
        .create_framebuffer(&FramebufferInfo {
            views: vec![view],
            width: 64,
            height: 64,
            layers: 1,
        })
        .unwrap_or(0);
    let layout = vk
        .create_set_layout(&[SetLayoutBindingInfo {
            binding: 0,
            ty: dt::UNIFORM_BUFFER_DYNAMIC,
            count: 1,
            stages: 1,
            immutable: vec![],
        }])
        .unwrap_or(0);
    let pool = vk.create_descriptor_pool();
    let set = vk
        .allocate_sets(pool, &[layout])
        .unwrap_or_default()
        .first()
        .copied()
        .unwrap_or(0);
    let cpool = vk.create_command_pool();
    let cb = vk
        .allocate_command_buffers(cpool, 0, 1)
        .unwrap_or_default()
        .first()
        .copied()
        .unwrap_or(0);
    assert!(vk.begin_command_buffer(cb).is_ok());
    vk.cmd_begin_render_pass(
        cb,
        &RenderPassBeginInfo {
            render_pass: pass,
            framebuffer: fb,
            area: Rect2D {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            },
            clear: vec![[0; 4]],
        },
    );
    vk.cmd_bind_pipeline(cb, 0, 77);
    vk.cmd_bind_descriptor_sets(cb, 0, 0, &[set], &[256]);
    vk.cmd_bind_vertex_buffers(cb, 0, &[vb], &[16]);
    vk.cmd_set_viewport(cb, 0, vec![Viewport::default()]);
    vk.cmd_set_viewport(cb, 15, vec![Viewport::default(); 2]);
    vk.cmd_push_constants(cb, 1, 250, vec![0; 8]);
    vk.cmd_draw_indexed(cb, 6, 1, 0, 0, 0);
    vk.cmd_draw_indirect_count(cb);
    vk.cmd_end_render_pass(cb);
    vk.cmd_pipeline_barrier(cb, 0);
    vk.cmd_pipeline_barrier(cb, 1);
    assert!(vk.end_command_buffer(cb).is_ok());
    let cmds = vk.recorded(cb).unwrap_or_else(|| Arc::from(Vec::new()));
    let names: Vec<&str> = cmds.iter().map(Cmd::name).collect();
    assert_eq!(
        names,
        [
            "vkCmdBeginRenderPass",
            "vkCmdBindPipeline",
            "vkCmdBindDescriptorSets",
            "vkCmdBindVertexBuffers",
            "vkCmdSetViewport",
            "vkCmdDrawIndexed",
            "vkCmdEndRenderPass",
            "vkCmdPipelineBarrier"
        ]
    );
    let Some(Cmd::BindVertexBuffers { buffers, .. }) = cmds.get(3) else {
        panic!("no vertex buffers");
    };
    assert_eq!((buffers[0].offset, buffers[0].size), (1024 + 16, 4096 - 16));
    assert!(matches!(
        cmds.get(1),
        Some(Cmd::BindPipeline {
            bind: BindPoint::Graphics,
            pipeline: PipelineId(77)
        })
    ));
    assert!(vk.log().seen("indirect count"));

    let fence = vk.create_fence(false);
    assert_eq!(vk.fence_status(fence), Ok(Status::NotReady));
    rec.hold(true);
    rec.take();
    assert!(vk.submit(&[vec![cb]], fence).is_ok());
    assert_eq!(vk.fence_status(fence), Ok(Status::NotReady));
    assert_eq!(vk.wait_for_fences(&[fence], true, 0), Ok(Status::Timeout));
    rec.complete_all();
    assert_eq!(
        vk.wait_for_fences(&[fence], true, u64::MAX),
        Ok(Status::Success)
    );
    let subs = rec.submissions();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].serial, 1);
    assert_eq!(subs[0].command_buffers[0].cmds, cmds);
    let contents = subs[0].sets.get(&set).map(|c| c.descriptors.len());
    assert_eq!(contents, Some(1));
    vk.reset_fences(&[fence]);
    assert_eq!(vk.fence_status(fence), Ok(Status::NotReady));
    // A write after the submit does not change what the submit holds (copy on write).
    vk.update_sets(
        &[info::WriteInfo {
            set,
            binding: 0,
            element: 0,
            ty: dt::UNIFORM_BUFFER_DYNAMIC,
            items: vec![info::WriteItem::Buffer {
                buffer: vb,
                offset: 0,
                range: 64,
            }],
        }],
        &[],
    );
    assert_eq!(
        subs[0].sets.get(&set).map(|c| c.descriptors[0]),
        Some(Descriptor::Empty)
    );
}

#[test]
fn freeing_command_buffers_gives_their_slots_back() {
    let (vk, _, slots) = vk();
    let pool = vk.create_command_pool();
    let cbs = vk.allocate_command_buffers(pool, 0, 3).unwrap_or_default();
    assert_eq!(slots.live(), cbs);
    vk.free_command_buffers(&cbs[..1]);
    vk.free_command_buffers(&cbs[..1]);
    assert_eq!(slots.live(), cbs[1..].to_vec());
    vk.destroy_command_pool(pool);
    vk.destroy_command_pool(pool);
    assert!(slots.live().is_empty());
    assert_eq!(vk.begin_command_buffer(cbs[1]), Err(VkError::Unknown));
}

#[test]
fn secondaries_and_events_are_resolved_at_submit() {
    let (vk, rec, _) = vk();
    let pool = vk.create_command_pool();
    let cbs = vk.allocate_command_buffers(pool, 0, 2).unwrap_or_default();
    let event = vk.create_event();
    assert!(vk.begin_command_buffer(cbs[1]).is_ok());
    vk.cmd_set_event(cbs[1], event);
    assert!(vk.end_command_buffer(cbs[1]).is_ok());
    assert!(vk.begin_command_buffer(cbs[0]).is_ok());
    vk.cmd_execute_commands(cbs[0], &[cbs[1]]);
    assert!(vk.end_command_buffer(cbs[0]).is_ok());
    assert_eq!(vk.event_status(event), Ok(Status::EventReset));
    rec.hold(true);
    assert!(vk.submit(&[vec![cbs[0]]], 0).is_ok());
    assert_eq!(vk.event_status(event), Ok(Status::EventReset));
    rec.complete_all();
    assert_eq!(vk.event_status(event), Ok(Status::EventSet));
    let sub = rec.submissions().pop().unwrap_or_default();
    assert_eq!(sub.secondaries.get(&cbs[1]).map(|c| c.len()), Some(1));
    assert!(vk.set_event(event, false).is_ok());
    assert_eq!(vk.event_status(event), Ok(Status::EventReset));
}

#[test]
fn query_results_wait_for_their_submit() {
    let (vk, rec, _) = vk();
    let m = TestMemory::new(0x3_0000, 256);
    let qp = vk.create_query_pool(2, 4).unwrap_or(0);
    let pool = vk.create_command_pool();
    let cb = vk.allocate_command_buffers(pool, 0, 1).unwrap_or_default()[0];
    assert!(vk.begin_command_buffer(cb).is_ok());
    vk.cmd_write_timestamp(cb, qp, 1);
    assert!(vk.end_command_buffer(cb).is_ok());
    rec.hold(true);
    assert!(vk.submit(&[vec![cb]], 0).is_ok());
    rec.set_query(QueryPoolId(qp), 1, 12345);
    let data = m.alloc(64);
    let flags = qr::R64 | qr::WITH_AVAILABILITY;
    assert_eq!(
        vk.query_pool_results(&m, qp, 0, 2, 64, data, 16, flags),
        Ok(Status::NotReady)
    );
    rec.complete_all();
    assert_eq!(
        vk.query_values(qp, 1, 1, false).map(|r| r.0),
        Ok(vec![Some(12345)])
    );
    assert_eq!(
        vk.query_pool_results(&m, qp, 1, 1, 16, data, 16, flags),
        Ok(Status::Success)
    );
    assert_eq!((m.u64(data), m.u64(data + 8)), (Ok(12345), Ok(1)));
}

#[test]
fn the_swapchain_hands_out_its_images_in_turn() {
    let (vk, rec, _) = vk();
    let sc = vk
        .create_swapchain(&SwapchainInfo {
            min_images: 3,
            format: 44,
            extent: (1280, 720),
            usage: 0x10,
            ..Default::default()
        })
        .unwrap_or(0);
    let images = vk.swapchain_images(sc).unwrap_or_default();
    assert_eq!(images.len(), 3);
    let fence = vk.create_fence(false);
    let got: Vec<u32> = (0..4)
        .filter_map(|_| vk.acquire_next_image(sc, u64::MAX, fence).ok().map(|r| r.1))
        .collect();
    assert_eq!(got, vec![0, 1, 2, 0]);
    assert_eq!(vk.fence_status(fence), Ok(Status::Success));
    vk.destroy_image(images[0]);
    assert!(vk.image_requirements(images[0]).is_ok());
    assert!(vk.submit(&[], 0).is_ok());
    let r = vk.present(&[(sc, 2), (sc, 9)]);
    assert_eq!(r, vec![Ok(()), Err(VkError::OutOfDate)]);
    let present = rec.calls().into_iter().find_map(|c| match c {
        Call::Present(p) => Some(p),
        _ => None,
    });
    assert_eq!(
        present.map(|p| (p.image, p.after)),
        Some((TextureId(images[2]), 1))
    );
    assert_eq!(
        vk.create_swapchain(&SwapchainInfo {
            format: 106,
            ..Default::default()
        }),
        Err(VkError::FormatNotSupported)
    );
    vk.destroy_swapchain(sc);
    assert!(vk.image_requirements(images[0]).is_err());
}

#[test]
fn update_buffer_is_checked_and_bad_records_are_dropped() {
    let (vk, _, _) = vk();
    let buf = vk.create_buffer(&info::BufferInfo {
        size: 64,
        usage: 0x2,
        flags: 0,
    });
    let pool = vk.create_command_pool();
    let cb = vk.allocate_command_buffers(pool, 0, 1).unwrap_or_default()[0];
    assert!(vk.begin_command_buffer(cb).is_ok());
    vk.cmd_update_buffer(cb, buf, 0, vec![1; 16]);
    vk.cmd_update_buffer(cb, buf, 2, vec![1; 16]);
    vk.cmd_update_buffer(cb, buf, 60, vec![1; 8]);
    vk.cmd_dispatch_base(cb, [1, 0, 0], [1, 1, 1]);
    vk.cmd_dispatch_base(cb, [0; 3], [2, 1, 1]);
    assert!(vk.end_command_buffer(cb).is_ok());
    let n = vk.recorded(cb).map(|c| c.len());
    assert_eq!(n, Some(2));
}

#[test]
fn a_graphics_pipeline_is_decoded_from_guest_memory() {
    let (vk, rec, _) = vk();
    let m = TestMemory::new(0x4_0000, 8192);
    let module = vk.create_shader_module(b"\x03\x02\x23\x07spirv");
    let name = m.put(b"main\0");
    let spec_data = m.put(&7u32.to_le_bytes());
    let entry = m.put(
        &StructBytes::new(sme::SIZE)
            .u32(sme::CONSTANT_ID, 3)
            .u32(sme::OFFSET, 0)
            .u64(sme::SIZE_, 4)
            .0,
    );
    let spec = m.put(
        &StructBytes::new(si::SIZE)
            .u32(si::MAP_ENTRY_COUNT, 1)
            .u64(si::P_MAP_ENTRIES, entry)
            .u64(si::DATA_SIZE, 4)
            .u64(si::P_DATA, spec_data)
            .0,
    );
    let stage = m.put(
        &StructBytes::new(pss::SIZE)
            .u32(pss::STAGE, 1)
            .u64(pss::MODULE, module)
            .u64(pss::P_NAME, name)
            .u64(pss::P_SPECIALIZATION_INFO, spec)
            .0,
    );
    let binding = m.put(
        &StructBytes::new(vib::SIZE)
            .u32(vib::BINDING, 0)
            .u32(vib::STRIDE, 20)
            .0,
    );
    let attr = m.put(
        &StructBytes::new(via::SIZE)
            .u32(via::LOCATION, 0)
            .u32(via::BINDING, 0)
            .u32(via::FORMAT, 106)
            .u32(via::OFFSET, 8)
            .0,
    );
    let vi = m.put(
        &StructBytes::new(pvi::SIZE)
            .u32(pvi::VERTEX_BINDING_DESCRIPTION_COUNT, 1)
            .u64(pvi::P_VERTEX_BINDING_DESCRIPTIONS, binding)
            .u32(pvi::VERTEX_ATTRIBUTE_DESCRIPTION_COUNT, 1)
            .u64(pvi::P_VERTEX_ATTRIBUTE_DESCRIPTIONS, attr)
            .0,
    );
    let ia = m.put(&StructBytes::new(pia::SIZE).u32(pia::TOPOLOGY, 4).0);
    let blend_att = m.put(
        &StructBytes::new(pcba::SIZE)
            .u32(pcba::BLEND_ENABLE, 1)
            .u32(pcba::SRC_COLOR_BLEND_FACTOR, 15)
            .u32(pcba::COLOR_WRITE_MASK, 0xF)
            .0,
    );
    let cb = m.put(
        &StructBytes::new(pcb::SIZE)
            .u32(pcb::ATTACHMENT_COUNT, 1)
            .u64(pcb::P_ATTACHMENTS, blend_att)
            .f32(pcb::BLEND_CONSTANTS + 4, 0.5)
            .0,
    );
    let mut rp = RenderPassInfo {
        subpasses: 1,
        ..Default::default()
    };
    rp.desc.attachments.push(crate::desc::AttachmentDesc {
        format: 44,
        samples: 1,
        load: 0,
        store: 0,
        stencil_load: 0,
        stencil_store: 0,
    });
    rp.desc.colors.push(Some(0));
    let pass = vk.create_render_pass(&rp).unwrap_or(0);
    let ci = m.put(
        &StructBytes::new(gpc::SIZE)
            .u32(gpc::STAGE_COUNT, 1)
            .u64(gpc::P_STAGES, stage)
            .u64(gpc::P_VERTEX_INPUT_STATE, vi)
            .u64(gpc::P_INPUT_ASSEMBLY_STATE, ia)
            .u64(gpc::P_COLOR_BLEND_STATE, cb)
            .u64(gpc::RENDER_PASS, pass)
            .0,
    );
    let infos = info::graphics_pipelines(&m, ci, 1).unwrap_or_default();
    rec.take();
    let made = vk.create_graphics_pipelines(&infos).unwrap_or_default();
    assert_eq!(made.len(), 1);
    let calls = rec.take();
    let Some(Call::CreateGraphicsPipeline(id, d)) = calls.first() else {
        panic!("no pipeline: {calls:?}");
    };
    assert_eq!(*id, PipelineId(made[0]));
    assert_eq!(
        d.stages[0].key,
        crate::desc::ShaderKey::of(b"\x03\x02\x23\x07spirv")
    );
    assert_eq!(
        (
            d.stages[0].entry.as_str(),
            d.stages[0].specialization.clone()
        ),
        ("main", vec![(3, 7u32.to_le_bytes().to_vec())])
    );
    assert_eq!(
        (
            d.state.topology,
            d.state.bindings[0].stride,
            d.state.attributes[0].offset
        ),
        (4, 20, 8)
    );
    assert_eq!(
        d.state.attributes[0].vertex,
        Some(crate::format::VertexFormat::Float3)
    );
    assert!(d.state.dual_source());
    assert_eq!(d.state.blend_constants[1], 0.5);
    assert_eq!(d.targets.colors, vec![Some(44)]);
    vk.destroy_pipeline(made[0]);
    vk.destroy_pipeline(made[0]);
    assert_eq!(rec.take(), vec![Call::DestroyPipeline(PipelineId(made[0]))]);
}
