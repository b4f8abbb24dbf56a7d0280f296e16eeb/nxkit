//! Tests for the pipeline cache and the fragment entry choice.

use super::*;
use crate::detail::{DrawArgs, ProgramRef};

#[test]
fn a_rejected_topology_uses_the_cached_successful_variant() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let lib = ShaderLibrary::from_source(
        &ctx,
        "point-size",
        r#"
#include <metal_stdlib>
using namespace metal;
struct V { float4 position [[position]]; float size [[point_size]]; };
vertex V vs(uint id [[vertex_id]]) { return {float4(0,0,0,1), 1}; }
fragment float4 fs() { return float4(1); }
fragment uint4 fs_u() { return uint4(1); }
struct Mixed { uint4 integer [[color(0)]]; float4 color [[color(1)]]; };
fragment Mixed fs_m() { return {uint4(1), float4(1)}; }
"#,
    )
    .unwrap();
    let stage = |entry: &str| StageProgram {
        lib: lib.clone(),
        entry: entry.into(),
        entry_uint: None,
        entry_mixed: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: vec![],
        colors: Some(vec![0]),
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
    };
    let vs = stage("vs");
    let mut fs = stage("fs");
    let draw = DrawDetail {
        program: ProgramRef {
            vertex: "point-size".into(),
            fragment: Some("fs".into()),
            task: None,
            mesh: None,
        },
        push: None,
        blend: Default::default(),
        write_masks: [15; 8],
        depth_stencil: Default::default(),
        raster: Default::default(),
        topology: 4,
        attributes: vec![],
        streams: vec![],
        args: DrawArgs::Arrays {
            first: 0,
            count: 3,
            instances: 1,
            base_instance: 0,
        },
        ubos: vec![],
        sbufs: vec![],
        textures: vec![],
        images: vec![],
        dynamic: Default::default(),
    };
    let targets = Targets {
        colors: vec![Some(Format::Rgba8Unorm)],
        samples: 1,
    };
    let pipelines = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
    let (_, key) = pipelines
        .render(&draw, &vs, Some(&fs), &targets, false)
        .unwrap();
    assert!(pipelines.topology_fallbacks.lock().unwrap().contains(&key));
    // the vertex function is remembered too: its other states skip the class without a refused compile first
    let vertex = point_size_key(&draw.program.vertex, &vs.entry);
    assert!(
        pipelines
            .topology_fallbacks
            .lock()
            .unwrap()
            .contains(&vertex)
    );
    for _ in 0..10 {
        assert_eq!(
            pipelines
                .render(&draw, &vs, Some(&fs), &targets, false)
                .unwrap()
                .1,
            key
        );
    }
    // The ten repeats are answered by the thread memo and never reach the cache.
    assert_eq!(pipelines.counts(), (1, 0));
    fs.entry_uint = Some("fs_u".into());
    let integer = Targets {
        colors: vec![Some(Format::Rgba16Uint)],
        samples: 1,
    };
    assert_eq!(fragment_entry(&fs, &integer), "fs_u");
    pipelines
        .render(&draw, &vs, Some(&fs), &integer, false)
        .unwrap();
    fs.colors = Some(vec![0, 1]);
    fs.entry_mixed = Some(("fs_m".into(), 1));
    let mixed = Targets {
        colors: vec![Some(Format::Rgba16Uint), Some(Format::Rgba8Unorm)],
        samples: 1,
    };
    assert_eq!(fragment_entry(&fs, &mixed), "fs_m");
    pipelines
        .render(&draw, &vs, Some(&fs), &mixed, false)
        .unwrap();
}
