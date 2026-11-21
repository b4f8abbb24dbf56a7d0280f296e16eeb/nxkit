//! A smoke test on a real Metal 4 GPU: memory, an image, a pipeline from a tiny library, one submit with an update,
//! a render pass, a draw with a uniform buffer and push constants, a copy back into memory and a timestamp; then a
//! present without a display. Without a device it prints a message and passes.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use nxkit_metal::{Context, ShaderLibrary};
use nxkit_vk::Backend;
use nxkit_vk::backend::{CommandBufferSnapshot, Present, SetContents, Submission};
use nxkit_vk::cmd::{BindPoint, BufferImageCopy, Cmd, RenderPassBegin, SubresourceLayers};
use nxkit_vk::desc::*;
use nxkit_vk::format::{Conversion, PixelFormat};
use nxkit_vk_gpu::{ShaderTable, VkGpu, VkGpuSettings};

const VS: &str = "1111111111111111";
const FS: &str = "2222222222222222";

fn msl() -> String {
    format!(
        "#include <metal_stdlib>
using namespace metal;
struct VOut {{ float4 pos [[position]]; }};
vertex VOut vk_{VS}(uint id [[vertex_id]]) {{
    float2 p = float2((id << 1) & 2, id & 2);
    VOut o; o.pos = float4(p * 2.0 - 1.0, 0.0, 1.0); return o;
}}
fragment float4 vk_{FS}(constant float4& u [[buffer(0)]], constant float4& pc [[buffer(1)]]) {{ return u + pc; }}
"
    )
}

fn key(hex: &str) -> ShaderKey {
    ShaderKey(nxkit_abi::vk_table::parse_key(hex).unwrap())
}

fn floats(v: [f32; 4]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn image_desc(usage: u32, swapchain: bool) -> ImageDesc {
    ImageDesc {
        image_type: 1,
        format: 37,
        pixel: PixelFormat::Rgba8Unorm,
        kind: TextureKind::D2,
        extent: [4, 4, 1],
        levels: 1,
        layers: 1,
        samples: 1,
        usage,
        flags: 0,
        format_views: false,
        linear: None,
        conversion: Conversion::None,
        swapchain,
    }
}

fn pipeline(set: &Arc<SetLayoutDesc>) -> GraphicsPipelineDesc {
    let stage = |stage, hex| StageDesc {
        stage,
        key: key(hex),
        entry: "main".into(),
        specialization: vec![],
    };
    GraphicsPipelineDesc {
        stages: vec![stage(0x1, VS), stage(0x10, FS)],
        state: PipelineState {
            topology: 3,
            viewport_count: 1,
            samples: 1,
            blend: vec![BlendAttachment {
                write_mask: 0xF,
                ..BlendAttachment::default()
            }],
            dynamic: vec![0, 1],
            ..PipelineState::default()
        },
        targets: Targets {
            colors: vec![Some(37)],
            depth: None,
        },
        layout: Arc::new(PipelineLayoutDesc {
            sets: vec![set.clone()],
            push_ranges: vec![PushRange {
                stages: 0x10,
                offset: 0,
                size: 16,
            }],
            push_size: 16,
        }),
    }
}

#[test]
fn a_submit_draws_into_an_image_and_copies_it_back() {
    let Ok(ctx) = Context::system_default() else {
        eprintln!("skipped: no usable Metal 4 device");
        return;
    };
    let lib = ShaderLibrary::from_source(&ctx, "vk_test", &msl()).unwrap();
    let tsv = format!("{VS}\tvert\t\t/t.vert\n{FS}\tfrag\tu:0.0=0 p:0.0=1\t/t.frag\n");
    let rows = nxkit_abi::vk_table::parse(&tsv).unwrap();
    let gpu = VkGpu::with_shaders(
        &ctx,
        ShaderTable::new(Some(lib), rows),
        VkGpuSettings::default(),
    )
    .unwrap();

    let mem = MemoryId(1);
    let host = gpu.create_memory(mem, 4096, 1).unwrap();
    assert!(
        host != 0 && host + 4096 <= 1 << 39,
        "host address {host:#x}"
    );
    gpu.create_image(TextureId(10), &image_desc(0x10 | 0x1, false))
        .unwrap();
    let whole = ViewDesc {
        format: 37,
        pixel: PixelFormat::Rgba8Unorm,
        kind: TextureKind::D2,
        base_level: 0,
        levels: 1,
        base_layer: 0,
        layers: 1,
        swizzle: IDENTITY_SWIZZLE,
        aspect: 1,
        whole: true,
    };
    gpu.create_view(TextureId(11), TextureId(10), &whole)
        .unwrap();
    let set_layout = Arc::new(SetLayoutDesc {
        bindings: vec![LayoutBinding {
            binding: 0,
            ty: 6,
            count: 1,
            stages: 0x10,
            base: 0,
            dynamic: None,
            immutable: vec![],
        }],
        total: 1,
        dynamic_count: 0,
    });
    gpu.create_graphics_pipeline(PipelineId(20), &pipeline(&set_layout))
        .unwrap();
    // A pipeline with a shader the table lacks is refused, and the message names the key.
    let mut missing = pipeline(&set_layout);
    missing.stages[1].key = key("3333333333333333");
    let e = gpu
        .create_graphics_pipeline(PipelineId(21), &missing)
        .unwrap_err();
    assert!(e.0.contains("3333333333333333"), "{e}");
    gpu.create_query_pool(QueryPoolId(30), QueryType::Timestamp, 2)
        .unwrap();

    let uniform = BufferRef {
        memory: mem,
        offset: 0,
        size: 16,
    };
    let begin = RenderPassBegin {
        pass: Arc::new(RenderPassDesc {
            attachments: vec![AttachmentDesc {
                format: 37,
                samples: 1,
                load: 1,
                store: 0,
                stencil_load: 2,
                stencil_store: 1,
            }],
            colors: vec![Some(0)],
            resolves: vec![],
            depth: None,
        }),
        framebuffer: Arc::new(FramebufferDesc {
            attachments: vec![FramebufferAttachment {
                view: TextureId(11),
                image: TextureId(10),
                format: 37,
                samples: 1,
                level: 0,
                layer: 0,
            }],
            width: 4,
            height: 4,
            layers: 1,
        }),
        area: Rect2D {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        },
        clear: vec![[0; 4]],
    };
    let cmds = vec![
        Cmd::ResetQueryPool {
            pool: QueryPoolId(30),
            first: 0,
            count: 2,
        },
        Cmd::UpdateBuffer {
            dst: uniform,
            data: floats([0.25, 0.5, 0.0, 0.0]),
        },
        Cmd::PipelineBarrier,
        Cmd::BeginRenderPass(Arc::new(begin)),
        Cmd::BindPipeline {
            bind: BindPoint::Graphics,
            pipeline: PipelineId(20),
        },
        Cmd::BindDescriptorSets {
            bind: BindPoint::Graphics,
            first: 0,
            sets: vec![100],
            dynamic_offsets: vec![],
        },
        Cmd::PushConstants {
            stages: 0x10,
            offset: 0,
            data: floats([0.0, 0.0, 0.5, 1.0]),
        },
        Cmd::Draw {
            vertices: 3,
            instances: 1,
            first_vertex: 0,
            first_instance: 0,
        },
        Cmd::EndRenderPass,
        Cmd::CopyImageToBuffer {
            src: ImageRef {
                texture: TextureId(10),
                format: 37,
                samples: 1,
            },
            dst: BufferRef {
                memory: mem,
                offset: 256,
                size: 64,
            },
            regions: vec![BufferImageCopy {
                buffer_offset: 0,
                row_length: 0,
                image_height: 0,
                sub: SubresourceLayers {
                    aspect: 1,
                    level: 0,
                    base_layer: 0,
                    layers: 1,
                },
                offset: [0; 3],
                extent: [4, 4, 1],
            }],
        },
        Cmd::WriteTimestamp {
            pool: QueryPoolId(30),
            query: 1,
        },
    ];
    let sub = Submission {
        serial: 1,
        command_buffers: vec![CommandBufferSnapshot {
            handle: 5,
            cmds: Arc::from(cmds),
        }],
        secondaries: HashMap::new(),
        sets: HashMap::from([(
            100,
            SetContents {
                layout: set_layout.clone(),
                descriptors: Arc::new(vec![Descriptor::Buffer(uniform)]),
            },
        )]),
    };
    gpu.submit(&sub).unwrap();
    assert!(gpu.wait(1, 5_000_000_000), "the submit did not complete");
    assert_eq!(gpu.completed(), 1);
    let mut px = [0u8; 64];
    assert!(gpu.read_memory(mem, 256, &mut px));
    for p in px.as_chunks::<4>().0 {
        let want = [64u8, 128, 128, 255];
        assert!(
            p.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 1),
            "pixel {p:?}, want {want:?}"
        );
    }
    assert!(gpu.query_value(QueryPoolId(30), 1) > 0);
    assert_eq!(gpu.query_value(QueryPoolId(30), 0), 0);

    // A fill inside the pass cuts it: the next draw resumes with a load. A clear of the whole attachment at the end
    // clears with the load action of one more segment.
    let draw = || Cmd::Draw {
        vertices: 3,
        instances: 1,
        first_vertex: 0,
        first_instance: 0,
    };
    let mut cut: Vec<Cmd> = sub.command_buffers[0].cmds.to_vec();
    let at = cut
        .iter()
        .position(|c| matches!(c, Cmd::EndRenderPass))
        .unwrap();
    cut.splice(
        at..at,
        [
            Cmd::FillBuffer {
                dst: BufferRef {
                    memory: mem,
                    offset: 1024,
                    size: 64,
                },
                data: 0x0102_0304,
            },
            draw(),
            Cmd::ClearAttachments {
                attachments: vec![nxkit_vk::cmd::ClearAttachment {
                    aspect: 1,
                    color_attachment: 0,
                    value: [1.0f32.to_bits(), 0, 0, 1.0f32.to_bits()],
                }],
                rects: vec![nxkit_vk::cmd::ClearRect {
                    rect: Rect2D {
                        x: 0,
                        y: 0,
                        width: 4,
                        height: 4,
                    },
                    base_layer: 0,
                    layers: 1,
                }],
            },
        ],
    );
    gpu.submit(&Submission {
        serial: 2,
        command_buffers: vec![CommandBufferSnapshot {
            handle: 6,
            cmds: Arc::from(cut),
        }],
        ..sub.clone()
    })
    .unwrap();
    assert!(gpu.wait(2, 5_000_000_000));
    assert!(gpu.read_memory(mem, 256, &mut px));
    assert_eq!(&px[..4], &[255, 0, 0, 255], "the clear of the attachments");
    let mut fill = [0u8; 8];
    assert!(gpu.read_memory(mem, 1024, &mut fill));
    assert_eq!(fill, [4, 3, 2, 1, 4, 3, 2, 1]);

    // An empty submit completes in order too, and a present without a pacer counts as shown.
    gpu.submit(&Submission {
        serial: 3,
        ..Submission::default()
    })
    .unwrap();
    assert!(gpu.wait(3, 5_000_000_000));
    gpu.create_image(TextureId(40), &image_desc(0x10, true))
        .unwrap();
    assert!(gpu.acquire(TextureId(40), 1_000_000_000));
    gpu.present(&Present {
        swapchain: 1,
        index: 0,
        image: TextureId(40),
        extent: (4, 4),
        format: 37,
        after: 3,
    })
    .unwrap();
    assert!(gpu.acquire(TextureId(40), 1_000_000_000));
    gpu.destroy_memory(mem);
}
