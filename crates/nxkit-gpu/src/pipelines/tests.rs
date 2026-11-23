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
        entry_layer: None,
        entry_z: [None, None],
        entry_discard: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: vec![],
        pull: false,
        colors: Some(vec![0]),
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
        swap: 0,
    };
    let vs = stage("vs");
    let mut fs = stage("fs");
    let draw = DrawDetail {
        program: ProgramRef {
            vertex: "point-size".into(),
            fragment: Some("fs".into()),
            task: None,
            mesh: None,
            geometry: None,
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
        samplers: Vec::new(),
        images: vec![],
        dynamic: Default::default(),
        feedback_input: false,
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

#[test]
fn a_draw_with_a_layer_picking_geometry_shader_runs_the_layer_entry() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let lib = ShaderLibrary::from_source(
        &ctx,
        "layer",
        "#include <metal_stdlib>\nusing namespace metal;\nvertex float4 vs() { return float4(0); }\n",
    )
    .unwrap();
    let mut vs = StageProgram {
        lib,
        entry: "vs".into(),
        entry_uint: None,
        entry_mixed: None,
        entry_layer: None,
        entry_z: [None, None],
        entry_discard: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: vec![],
        pull: false,
        colors: None,
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
        swap: 0,
    };
    let mut d = crate::walk::empty_draw();
    assert!(!layered(&d, &vs));
    d.program.geometry = Some("gs".into());
    // Without a layer entry the geometry stage stays ignored.
    assert!(!layered(&d, &vs));
    assert_eq!(render::vertex_entry(&d, &vs, false), ("vs", ""));
    vs.entry_layer = Some("vs_l".into());
    assert!(layered(&d, &vs));
    assert_eq!(render::vertex_entry(&d, &vs, false), ("vs_l", ":layer"));
    d.program.geometry = None;
    assert_eq!(render::vertex_entry(&d, &vs, false), ("vs", ""));
}

#[test]
fn the_depth_mode_zero_to_one_takes_the_entries_that_keep_the_depth() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let lib = ShaderLibrary::from_source(
        &ctx,
        "depth",
        "#include <metal_stdlib>\nusing namespace metal;\nvertex float4 vs() { return float4(0); }\n",
    )
    .unwrap();
    let mut vs = StageProgram {
        lib,
        entry: "main0".into(),
        entry_uint: None,
        entry_mixed: None,
        entry_layer: Some("main0_l".into()),
        entry_z: [None, None],
        entry_discard: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: vec![],
        pull: false,
        colors: None,
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
        swap: 0,
    };
    let mut d = crate::walk::empty_draw();
    // A library without the entries keeps converting the depth.
    assert_eq!(render::vertex_entry(&d, &vs, true), ("main0", ""));
    vs.entry_z = [Some("main0_z".into()), Some("main0_lz".into())];
    assert_eq!(render::vertex_entry(&d, &vs, false), ("main0", ""));
    assert_eq!(render::vertex_entry(&d, &vs, true), ("main0_z", ":z"));
    d.program.geometry = Some("gs".into());
    assert_eq!(
        render::vertex_entry(&d, &vs, true),
        ("main0_lz", ":layer:z")
    );
}

#[test]
fn a_swapped_library_gets_pipelines_of_its_own_that_are_never_captured() {
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let src = |c: &str| {
        format!(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             vertex float4 vs() {{ return float4(0, 0, 0, 1); }}\n\
             fragment float4 fs() {{ return float4({c}); }}\n"
        )
    };
    let lib = |c: &str| ShaderLibrary::from_source(&ctx, "swap", &src(c)).unwrap();
    let stage = |entry: &str, lib: ShaderLibrary, swap: u32| StageProgram {
        lib,
        entry: entry.into(),
        entry_uint: None,
        entry_mixed: None,
        entry_layer: None,
        entry_z: [None, None],
        entry_discard: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: vec![],
        pull: false,
        colors: Some(vec![0]),
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
        swap,
    };
    let draw = DrawDetail {
        program: ProgramRef {
            vertex: "swap-vs".into(),
            fragment: Some("swap-fs".into()),
            task: None,
            mesh: None,
            geometry: None,
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
        samplers: Vec::new(),
        images: vec![],
        dynamic: Default::default(),
        feedback_input: false,
    };
    let targets = Targets {
        colors: vec![Some(Format::Rgba8Unorm)],
        samples: 1,
    };
    let archive = ArchiveConfig {
        collect: true,
        ..ArchiveConfig::default()
    };
    let pipelines = Pipelines::new(&ctx, &archive).unwrap();
    let vs = stage("vs", lib("1"), 0);
    let shipped = stage("fs", lib("1"), 0);
    let key_of = |fs: &StageProgram| {
        pipelines
            .render(&draw, &vs, Some(fs), &targets, false)
            .unwrap()
            .1
    };
    let k0 = key_of(&shipped);
    assert_eq!(
        pipelines.cache().captured(),
        1,
        "a shipped pipeline is captured"
    );
    let one = stage("fs", lib("0"), 1);
    let k1 = key_of(&one);
    assert_ne!(k0, k1, "the swap has a key of its own");
    assert_eq!(key_of(&one), k1);
    let two = stage("fs", lib("0.5"), 2);
    let k2 = key_of(&two);
    assert!(k2 != k1 && k2 != k0, "a new swap builds again");
    assert_eq!(pipelines.cache().captured(), 1, "a swapped pipeline is not");
    assert_eq!(key_of(&shipped), k0, "a reset finds the shipped pipeline");
    assert_eq!(
        pipelines.counts().0,
        3,
        "the shipped one was not built again"
    );
    // a swapped compute program has a pipeline of its own too
    let cs = ShaderLibrary::from_source(
        &ctx,
        "swap-cs",
        "#include <metal_stdlib>\nusing namespace metal;\nkernel void cs() {}\n",
    )
    .unwrap();
    let kernel = stage("cs", cs, 1);
    assert!(pipelines.compute("swap-cs", &kernel).is_ok());
    assert_eq!(pipelines.cache().captured(), 1);
}
