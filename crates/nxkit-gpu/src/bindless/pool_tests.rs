//! The host tables of the texture and sampler pool against the contract of the translator (`nxb-xlate`, `msl/pool.rs`).
//!
//! The tests need a Metal 4 device and return without one, as the other tests of this crate do.

use super::*;
use nxkit_metal::{
    ArchiveConfig, ArgumentTable, ColorAttachment, Format, FrameRing, LoadAction, PipelineCache,
    Primitive, RenderCommand, RenderPassDesc, RenderPipelineDesc, ShaderLibrary, Storage,
    StoreAction, TextureDesc,
};
use nxkit_nvn::descriptors::Descriptors;

fn context() -> Option<Context> {
    Context::system_default().ok()
}

fn word(b: &DataBuffer, at: u64) -> u32 {
    let mut v = [0u8; 4];
    b.read(at, &mut v).unwrap();
    u32::from_le_bytes(v)
}

fn long(b: &DataBuffer, at: u64) -> u64 {
    let mut v = [0u8; 8];
    b.read(at, &mut v).unwrap();
    u64::from_le_bytes(v)
}

fn resolved(texture: u64, sampler: u64) -> Resolved {
    Resolved {
        texture,
        sampler,
        scaled: false,
    }
}

#[test]
fn the_head_of_each_table_holds_its_bitmap_and_n_follows_the_highest_published_id() {
    let Some(ctx) = context() else { return };
    let mut b = Bindless::new(&ctx).unwrap();
    let [tex, samp, tex_seen, samp_seen] = b.pool_buffers(0).try_into().ok().unwrap();
    assert_eq!(tex_seen.len(), 131072);
    assert_eq!(samp_seen.len(), 512);
    assert_eq!(b.seen(), (tex_seen.gpu_address(), samp_seen.gpu_address()));
    assert_eq!(long(&tex, 8), tex_seen.gpu_address());
    assert_eq!(long(&samp, 8), samp_seen.gpu_address());
    assert_eq!((word(&tex, 0), word(&samp, 0)), (0, 0));

    b.publish(
        Descriptors::handle(5, 2),
        resolved(0x50, 0x20),
        (TexId(1), 1),
    );
    b.sync(0);
    assert_eq!((word(&tex, 0), word(&samp, 0)), (5, 2));
    b.publish(
        Descriptors::handle(9, 4),
        resolved(0x90, 0x40),
        (TexId(2), 1),
    );
    b.sync(0);
    assert_eq!((word(&tex, 0), word(&samp, 0)), (9, 4));
    // A lower id does not lower n.
    b.publish(
        Descriptors::handle(3, 1),
        resolved(0x30, 0x10),
        (TexId(3), 1),
    );
    b.sync(0);
    assert_eq!((word(&tex, 0), word(&samp, 0)), (9, 4));
    assert_eq!(long(&tex, POOL_HEAD + POOL_ELEMENT * 5), 0x50);
    assert_eq!(long(&tex, POOL_HEAD + POOL_ELEMENT * 9), 0x90);
    assert_eq!(long(&samp, POOL_HEAD + POOL_ELEMENT * 4), 0x40);
    // The head is untouched by the elements, and the pool limit raises n without an element.
    assert_eq!(long(&tex, 8), tex_seen.gpu_address());
    b.raise_texture_limit(1000);
    b.sync(0);
    assert_eq!(word(&tex, 0), 1000);
    b.raise_texture_limit(10);
    b.sync(0);
    assert_eq!(word(&tex, 0), 1000);
}

#[test]
fn an_element_is_cleared_when_its_texture_or_generation_goes() {
    let Some(ctx) = context() else { return };
    let mut b = Bindless::new(&ctx).unwrap();
    let tex = b.pool_buffers(0).remove(0);
    let at = |id: u64| POOL_HEAD + POOL_ELEMENT * id;
    assert!(b.publish_texture(5, 0x50, (TexId(1), 7)));
    assert!(b.publish_texture(6, 0x60, (TexId(2), 8)));
    // The same resource ID again writes nothing.
    assert!(!b.publish_texture(6, 0x60, (TexId(2), 8)));
    assert_eq!(b.release_gone(|_, _| false), 0);
    assert_eq!(b.release_gone(|t, g| t == TexId(1) && g == 7), 1);
    b.sync(0);
    assert_eq!(long(&tex, at(5)), 0);
    assert_eq!(long(&tex, at(6)), 0x60);
    assert_eq!(b.owners(), 1);
    // A new Metal texture of the same texture: the generation changed.
    assert_eq!(b.release_gone(|t, g| t == TexId(2) && g != 9), 1);
    b.sync(0);
    assert_eq!(long(&tex, at(6)), 0);
    assert_eq!(b.owners(), 0);
    // The element is filled again by the next frame.
    assert!(b.publish_texture(6, 0x61, (TexId(2), 9)));
    b.sync(0);
    assert_eq!(long(&tex, at(6)), 0x61);
    assert!(!b.clear_texture(99));
}

/// A frame in flight keeps the table of its slot: what a later frame clears or rewrites reaches only the slots that
/// take it, and each slot catches up on everything it missed.
#[test]
fn each_batch_slot_has_its_own_table_that_catches_up_when_it_syncs() {
    let Some(ctx) = context() else { return };
    let mut b = Bindless::new(&ctx).unwrap();
    let at = |id: u64| POOL_HEAD + POOL_ELEMENT * id;
    let t: Vec<_> = (0..3).map(|s| b.pool_buffers(s).remove(0)).collect();
    let s: Vec<_> = (0..3).map(|s| b.pool_buffers(s).remove(1)).collect();
    assert_ne!(b.pools(0), b.pools(1));
    assert_ne!(b.pools(1), b.pools(2));
    assert_eq!(long(&t[1], 8), long(&t[0], 8), "one shared bitmap");

    b.publish(
        Descriptors::handle(5, 2),
        resolved(0x50, 0x20),
        (TexId(1), 1),
    );
    b.sync(0);
    assert_eq!(long(&t[0], at(5)), 0x50);
    assert_eq!(long(&t[1], at(5)), 0, "slot 1 has not taken it yet");
    // Frame 2 clears the element and rewrites the sampler while frame 1 (slot 0) is still on the GPU.
    b.release_gone(|id, _| id == TexId(1));
    b.publish_sampler(2, 0x21);
    b.sync(1);
    assert_eq!(
        long(&t[0], at(5)),
        0x50,
        "slot 0 still holds what frame 1 reads"
    );
    assert_eq!(long(&s[0], at(2)), 0x20);
    assert_eq!((long(&t[1], at(5)), long(&s[1], at(2))), (0, 0x21));
    assert_eq!(word(&t[1], 0), 5);
    // Slot 0 comes round and catches up.
    b.sync(0);
    assert_eq!((long(&t[0], at(5)), long(&s[0], at(2))), (0, 0x21));
    // Slot 2 takes the whole history.
    b.sync(2);
    assert_eq!((long(&t[2], at(5)), long(&s[2], at(2))), (0, 0x21));
    assert_eq!(word(&t[2], 0), 5);
}

#[test]
fn the_bitmap_gives_the_marked_ids_and_a_sweep_clears_it() {
    let Some(ctx) = context() else { return };
    let mut b = Bindless::new(&ctx).unwrap();
    let seen = b.pool_buffers(0).remove(2);
    seen.write(0, &((1u32 << 3) | (1 << 7)).to_le_bytes())
        .unwrap();
    // An ID past the limit is not read.
    seen.write(4 * 40, &1u32.to_le_bytes()).unwrap();
    assert_eq!(b.wanted(100), vec![3, 7]);
    assert_eq!(word(&seen, 0), (1 << 3) | (1 << 7));
    for _ in 1..marks::SWEEP_EVERY {
        b.wanted(100);
    }
    assert_eq!(word(&seen, 0), 0, "the sweep clears the bits it took");
    assert_eq!(word(&seen, 4 * 40), 1, "a word past the limit stays");
    assert_eq!(
        b.wanted(100),
        vec![3, 7],
        "and the IDs stay for one more window"
    );
}

/// The shape of the generated code (`strukt`, `element` with the mark, and `MARK` in `msl/pool.rs`), written by hand.
const SHADER: &str = r#"
#include <metal_stdlib>
using namespace metal;

template<typename P> static inline __attribute__((always_inline)) uint dm_pool_mark(constant P* p, uint i)
{
    device atomic_uint* w = reinterpret_cast<device atomic_uint*>(p->seen) + (i >> 5);
    uint b = 1u << (i & 31u);
    if (!(atomic_load_explicit(w, memory_order_relaxed) & b)) atomic_fetch_or_explicit(w, b, memory_order_relaxed);
    return i;
}

struct spvPool_t
{
    uint n;
    ulong seen;
    array<texture2d<float>, 1> e;
};

struct spvDescriptorSetBuffer0
{
    constant spvPool_t* t [[id(0)]];
};

struct V { float4 pos [[position]]; };

vertex V vs(uint vid [[vertex_id]])
{
    float2 p = float2(float((vid << 1u) & 2u), float(vid & 2u));
    return V{float4(p * 2.0 - 1.0, 0.0, 1.0)};
}

fragment float4 fs(V in [[stage_in]], constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]],
                   constant uint& id [[buffer(1)]])
{
    constexpr sampler smp(coord::normalized, filter::nearest);
    return (&spvDescriptorSet0.t->e[0])[dm_pool_mark(spvDescriptorSet0.t, min(uint(id), spvDescriptorSet0.t->n))]
        .sample(smp, float2(0.5));
}
"#;

fn target(ctx: &Context, size: u32) -> Texture {
    let mut d = TextureDesc::render_target_2d(size, size, Format::Rgba8Unorm);
    d.storage = Storage::Shared;
    Texture::new(ctx, d).unwrap()
}

fn pass(color: &Texture, clear: [f64; 4]) -> RenderPassDesc {
    RenderPassDesc {
        colors: vec![Some(ColorAttachment {
            depth_plane: 0,
            texture: color.clone(),
            load: LoadAction::Clear,
            store: StoreAction::Store,
            clear,
            memoryless: false,
        })],
        depth: None,
        stencil: None,
    }
}

fn pixels(t: &Texture, size: u32) -> Vec<[u8; 4]> {
    let mut v = vec![0u8; (size * size * 4) as usize];
    t.read((size * 4) as usize, &mut v).unwrap();
    v.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}

/// A fragment function in the shape of the generated code reads the textures of two IDs through the table of
/// `Bindless`, an ID above `n` reads the texture of `n`, and the bitmap holds the bits of the IDs that were read.
#[test]
fn a_shader_reads_the_texture_of_its_id_clamped_to_n_and_marks_the_bit() {
    let Some(ctx) = context() else { return };
    const SIZE: u32 = 4;
    let mut bindless = Bindless::new(&ctx).unwrap();
    let [_, _, tex_seen, _] = bindless.pool_buffers(0).try_into().ok().unwrap();
    // Two 1x1 textures with different colours: a clear pass writes each.
    let (red, green) = (target(&ctx, 1), target(&ctx, 1));
    bindless.publish_texture(3, red.gpu_resource_id(), (TexId(1), 1));
    bindless.publish_texture(7, green.gpu_resource_id(), (TexId(2), 1));
    bindless.sync(0);
    let set = DataBuffer::new(&ctx, 16).unwrap();
    set.write(0, &bindless.pools(0).0.to_le_bytes()).unwrap();
    let ids = [3u32, 7, 1000];
    let id_buffers: Vec<DataBuffer> = ids
        .iter()
        .map(|id| {
            let b = DataBuffer::new(&ctx, 16).unwrap();
            b.write(0, &id.to_le_bytes()).unwrap();
            b
        })
        .collect();
    let lib = ShaderLibrary::from_source(&ctx, "pool-contract", SHADER).unwrap();
    let cache = PipelineCache::new(&ctx, &ArchiveConfig::default()).unwrap();
    let pipeline = cache
        .render(
            1,
            &RenderPipelineDesc::new((&lib, "vs"), Some((&lib, "fs")), &[Format::Rgba8Unorm]),
        )
        .unwrap();
    let targets: Vec<Texture> = ids.iter().map(|_| target(&ctx, SIZE)).collect();
    let table = ArgumentTable::new(&ctx, 31, 0).unwrap();
    let mut ring = FrameRing::new(&ctx).unwrap();

    let mut frame = ring.begin().unwrap();
    for (t, c) in [(&red, [1.0, 0.0, 0.0, 1.0]), (&green, [0.0, 1.0, 0.0, 1.0])] {
        frame
            .commands()
            .begin_render_pass(&pass(t, c), &table)
            .unwrap()
            .end();
    }
    frame.submit().unwrap().wait().expect("the clears run");

    let mut frame = ring.begin().unwrap();
    for (t, id) in targets.iter().zip(&id_buffers) {
        let mut p = frame
            .commands()
            .begin_render_pass(&pass(t, [0.0, 0.0, 0.0, 1.0]), &table)
            .unwrap();
        p.encode(&RenderCommand::SetPipeline(pipeline.clone()))
            .unwrap();
        for (index, gpu_addr) in [(0, set.gpu_address()), (1, id.gpu_address())] {
            p.encode(&RenderCommand::SetBuffer { index, gpu_addr })
                .unwrap();
        }
        p.encode(&RenderCommand::Draw {
            primitive: Primitive::Triangle,
            first_vertex: 0,
            vertex_count: 3,
            instance_count: 1,
            base_instance: 0,
        })
        .unwrap();
        p.end();
    }
    // A GPU address fault (a bad `seen`) fails the command buffer, and `wait` reports it.
    frame.submit().unwrap().wait().expect("no GPU fault");

    let all = |t: &Texture, px: [u8; 4]| pixels(t, SIZE).iter().all(|p| *p == px);
    assert!(all(&targets[0], [255, 0, 0, 255]), "ID 3 reads its texture");
    assert!(all(&targets[1], [0, 255, 0, 255]), "ID 7 reads its texture");
    assert!(
        all(&targets[2], [0, 255, 0, 255]),
        "ID 1000 is clamped to n, which is 7"
    );
    assert_eq!(
        tex_seen.load_u32(0).unwrap(),
        (1 << 3) | (1 << 7),
        "the shader marked the IDs it read, the clamped one for ID 1000"
    );
    assert_eq!(bindless.wanted(100), vec![3, 7]);
}
