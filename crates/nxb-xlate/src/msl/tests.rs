//! End to end tests of the emitter on small structured programs, and the Metal compile test of the samples.

use super::emit::Options;
use super::library::build;
use super::{Context, Translated};
use crate::attr::Stage;
use crate::driver::{BufferInfo, Program, TexInfo};
use crate::ir::{Inst, IoVariable, StorageKind};
use crate::job::parse_jobs;
use crate::props::{BufferDef, Field, MemoryDef, Props, TextureDef};
use crate::structured::ast::{A, AstTex, BlockType};
use crate::structured::types::AggType;
use crate::structured::{StructFunction, StructInfo};
use crate::translation::msl_entry::Structured;
use crate::types::SamplerType;

pub(super) const F4: AggType = AggType(AggType::VECTOR4.0 | AggType::FP32.0);
pub(super) const ONE: i32 = 0x3f80_0000;

/// A program of one function under construction.
pub(super) struct B {
    pub(super) s: Structured,
    pub(super) main: A,
}

impl B {
    pub(super) fn new(stage: Stage, name: &str) -> B {
        let job = parse_jobs(&format!(
            r#"[{{"key": "k", "stage": "{name}", "local": [8, 1, 1]}}]"#
        ))
        .expect("job")
        .remove(0);
        let mut info = StructInfo::default();
        let main = info.ast.new_block(BlockType::Main, None);
        info.functions.push(StructFunction {
            main_block: main,
            name: "main".into(),
            return_type: AggType::VOID,
            in_arguments: vec![],
            out_arguments: vec![],
            locals: vec![],
        });
        let mut props = Props::default();
        props.definitions.stage = Some(stage);
        B {
            s: Structured {
                job,
                info,
                props,
                program: Program::default(),
                notes: vec![],
                fetch1d: Default::default(),
                geometry_reader: false,
            },
            main,
        }
    }

    pub(super) fn k(&mut self, v: i32) -> A {
        self.s.info.ast.new_const(v)
    }

    pub(super) fn local(&mut self, ty: AggType) -> A {
        let l = self.s.info.ast.new_local(ty);
        self.s.info.functions[0].locals.push(l);
        l
    }

    pub(super) fn put(&mut self, n: A) {
        self.s.info.ast.add(self.main, n).expect("add");
    }

    pub(super) fn op(&mut self, inst: Inst, kind: StorageKind, index: i32, src: Vec<A>) -> A {
        self.s.info.ast.new_op(inst, kind, false, index, src)
    }

    /// `local = op(...)` and the local.
    pub(super) fn def(&mut self, ty: AggType, op: A) -> A {
        let l = self.local(ty);
        let a = self.s.info.ast.new_assign(l, op);
        self.put(a);
        l
    }

    pub(super) fn load_io(
        &mut self,
        kind: StorageKind,
        var: IoVariable,
        loc: Option<i32>,
        comp: i32,
        ty: AggType,
    ) -> A {
        let mut src = vec![self.k(var.0 as i32)];
        if let Some(l) = loc {
            src.push(self.k(l));
        }
        src.push(self.k(comp));
        let op = self.op(Inst::Load, kind, 0, src);
        self.def(ty, op)
    }

    /// A load of a scalar input builtin, which takes no component index.
    pub(super) fn load_scalar(&mut self, var: IoVariable, ty: AggType) -> A {
        let src = vec![self.k(var.0 as i32)];
        let op = self.op(Inst::Load, StorageKind::Input, 0, src);
        self.def(ty, op)
    }

    pub(super) fn store_io(&mut self, var: IoVariable, loc: Option<i32>, comp: i32, v: A) {
        let mut src = vec![self.k(var.0 as i32)];
        if let Some(l) = loc {
            src.push(self.k(l));
        }
        src.push(self.k(comp));
        src.push(v);
        let op = self.op(Inst::Store, StorageKind::Output, 0, src);
        self.put(op);
    }

    pub(super) fn io(&mut self, kind: StorageKind, var: IoVariable, loc: i32) {
        self.s.info.add_io(crate::structured::IoDef {
            storage_kind: kind,
            io_variable: var,
            location: loc,
            component: 0,
        });
    }

    pub(super) fn translate(&self) -> Translated {
        let ctx = Context {
            chain: "V",
            source: "p#0".into(),
            pc_paired: "",
            ..Context::default()
        };
        build(&self.s, &ctx, &Options::default()).expect("build")
    }

    pub(super) fn msl(&self) -> String {
        self.translate().libraries[0].msl.clone()
    }
}

pub(super) fn vertex() -> B {
    let mut b = B::new(Stage::Vertex, "vs");
    b.s.props.definitions.attribute_types = vec![0];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::Position, 0);
    b.io(StorageKind::Output, IoVariable::UserDefined, 1);
    let x = b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        0,
        AggType::FP32,
    );
    let one = b.k(ONE);
    let add = b.op(
        Inst(Inst::Add.0 | Inst::FP32),
        StorageKind::None,
        0,
        vec![x, one],
    );
    let y = b.def(AggType::FP32, add);
    b.store_io(IoVariable::Position, None, 0, y);
    b.store_io(IoVariable::UserDefined, Some(1), 2, y);
    let r = b.op(Inst::Return, StorageKind::None, 0, vec![]);
    b.put(r);
    b
}

pub(super) fn fragment() -> B {
    let mut b = B::new(Stage::Fragment, "ps");
    b.s.props.definitions.imap_types = vec![[2, 2, 2, 2]];
    b.s.props.definitions.fragment_output_types = vec![F4, F4];
    b.io(StorageKind::Input, IoVariable::UserDefined, 0);
    b.io(StorageKind::Output, IoVariable::FragmentOutputColor, 1);
    let u = b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        0,
        AggType::FP32,
    );
    let v = b.load_io(
        StorageKind::Input,
        IoVariable::UserDefined,
        Some(0),
        1,
        AggType::FP32,
    );
    b.s.props.properties.textures.push(TextureDef {
        set: 0,
        binding: 0,
        array_length: 1,
        separate: false,
        name: "fp_t2d_3".into(),
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
    });
    b.s.program.textures.push(TexInfo {
        binding: 0,
        ty: SamplerType::TEXTURE_2D.0,
        format: 0,
        cbuf_slot: -1,
        handle: i64::from((crate::msl::abi::C2_TEX + 3 * crate::msl::abi::C2_ROW) / 4),
        array_length: 1,
        separate: false,
        flags: 0,
    });
    let tex = AstTex {
        ty: SamplerType::TEXTURE_2D,
        format: 0,
        flags: 0,
        set: 0,
        binding: 0,
        sampler_set: 0,
        sampler_binding: -1,
    };
    let s =
        b.s.info
            .ast
            .new_tex_op(Inst::TextureSample, tex, 15, vec![u, v]);
    let _ = b.def(F4, s);
    let lt = b.op(Inst::CompareLess, StorageKind::None, 0, vec![u, v]);
    let c = b.def(AggType::BOOL, lt);
    let blk = b.s.info.ast.new_block(BlockType::If, Some(c));
    let d = b.s.info.ast.new_simple_op(Inst::Discard, vec![]);
    b.s.info.ast.add(blk, d).expect("add");
    b.put(blk);
    b.store_io(IoVariable::FragmentOutputColor, Some(1), 0, u);
    b
}

pub(super) fn compute() -> B {
    let mut b = B::new(Stage::Compute, "cs");
    b.s.props.definitions.compute_local_size = [8, 1, 1];
    b.io(StorageKind::Input, IoVariable::ThreadId, 0);
    b.s.props.properties.shared_memories.push(MemoryDef {
        id: 0,
        name: "shm".into(),
        ty: AggType(AggType::U32.0 | AggType::ARRAY.0),
        array_length: 8,
        zero_init: false,
    });
    b.s.props.properties.storage_buffers.push(BufferDef {
        layout: 1,
        set: 0,
        binding: 0,
        name: "fp_s0".into(),
        fields: vec![Field {
            ty: AggType(AggType::U32.0 | AggType::ARRAY.0),
            array_length: 0,
            name: "data".into(),
        }],
    });
    b.s.program.sbufs.push(BufferInfo {
        binding: 0,
        slot: 0,
        cb_slot: 0,
        cb_offset: i64::from(crate::msl::abi::C0_SSBO_CS / 4),
        flags: 1,
    });
    let t = b.load_io(
        StorageKind::Input,
        IoVariable::ThreadId,
        None,
        0,
        AggType::U32,
    );
    let zero = b.k(0);
    let st = b.op(Inst::Store, StorageKind::SharedMemory, 0, vec![zero, t, t]);
    b.put(st);
    let bar = b.op(Inst::Barrier, StorageKind::None, 0, vec![]);
    b.put(bar);
    let one = b.k(1);
    let at = b.op(
        Inst::AtomicAdd,
        StorageKind::SharedMemory,
        0,
        vec![zero, t, one],
    );
    let old = b.def(AggType::U32, at);
    let ld = b.op(Inst::Load, StorageKind::SharedMemory, 0, vec![zero, t]);
    let v = b.def(AggType::U32, ld);
    let sum = b.op(Inst::Add, StorageKind::None, 0, vec![v, old]);
    let s = b.def(AggType::U32, sum);
    let st = b.op(
        Inst::Store,
        StorageKind::StorageBuffer,
        0,
        vec![zero, zero, t, s],
    );
    b.put(st);
    b
}

#[test]
fn vertex_library_and_reflection() {
    let t = vertex().translate();
    let l = &t.libraries[0];
    assert_eq!(l.suffix, "_b");
    for want in [
        "vertex main0_out main0(main0_in in [[stage_in]]",
        "float4 loc0 [[attribute(0)]];",
        "float4 gl_Position [[position]];",
        "float4 loc1 [[user(locn1)]];",
        "out.loc1 = float4(0.0, 0.0, 0.0, 1.0);",
        "out.gl_Position.z = fma(out.gl_Position.z, 0.5, out.gl_Position.w * 0.5);",
        "return out;",
        "#pragma clang fp contract(off)",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
    let r = l.reflection.as_ref().expect("reflection");
    assert_eq!(r.key, "k_b");
    assert_eq!(r.msl_entry_kind, "vertex");
    assert_eq!(r.vertex_inputs.len(), 1);
    assert_eq!(r.vertex_inputs[0].location, 0);
    assert!(r.threadgroup_size.is_none());
}

#[test]
fn fragment_library_and_reflection() {
    let t = fragment().translate();
    let l = &t.libraries[0];
    for want in [
        "fragment main0_out main0(main0_in in [[stage_in]], constant spvDescriptorSetBuffer0& spvDescriptorSet0 [[buffer(0)]])",
        "float4 loc0 [[user(locn0)]];",
        "float4 c0_pad [[color(0)]];",
        "float4 c1 [[color(1)]];",
        "discard_fragment();",
        ".sample(",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
    let r = l.reflection.as_ref().expect("reflection");
    assert_eq!(r.raster.colors, Some(vec![0, 1]));
    assert_eq!(r.raster.discard, Some(true));
    assert!(
        r.bindings
            .iter()
            .any(|b| b.msl_name == "fp_t2d_3" && b.msl_type.starts_with("texture2d<float>"))
    );
    assert_eq!(t.inputs.varyings.len(), 1);
    assert_eq!(t.inputs.varyings[0].location, 0);
}

#[test]
fn compute_library_and_reflection() {
    let t = compute().translate();
    let l = &t.libraries[0];
    for want in [
        "kernel void main0(",
        "uint3 gl_LocalInvocationID [[thread_position_in_threadgroup]]",
        "threadgroup uint sm0[8];",
        "threadgroup_barrier(mem_flags::mem_threadgroup);",
        "atomic_fetch_add_explicit",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
    let r = l.reflection.as_ref().expect("reflection");
    assert_eq!(r.threadgroup_size, Some([8, 1, 1]));
    assert_eq!(r.msl_entry_kind, "kernel");
    assert!(
        r.bindings
            .iter()
            .any(|b| b.msl_name == "fp_s0" && b.rw == Some(true))
    );
}

#[test]
fn unported_stages_are_reported() {
    let job = parse_jobs(r#"[{"key": "k", "stage": "hs"}]"#)
        .expect("job")
        .remove(0);
    let e = crate::msl::translate(&job, &[], &Context::default()).expect_err("hs");
    assert_eq!(e.reason, "not ported");
}

#[test]
fn the_context_sets_the_pixel_options() {
    let ctx = Context {
        dual_source: true,
        fetch_outputs_only: true,
        ..Context::default()
    };
    let o = crate::msl::options("ps", &[], &ctx);
    assert!(o.dual_source && o.fetch_outputs_only && !o.visibility_counter);
    let o = crate::msl::options("ps", &[], &Context::default());
    assert!(!o.dual_source && !o.fetch_outputs_only);
}

fn fp(i: Inst) -> Inst {
    Inst(i.0 | Inst::FP32)
}

/// Subgroup, fence, conversion and builtin constructs.
pub(super) fn compute_subgroup() -> B {
    let mut b = compute();
    b.io(StorageKind::Input, IoVariable::SubgroupLaneId, 0);
    let t = b.load_scalar(IoVariable::SubgroupLaneId, AggType::U32);
    let zero = b.k(0);
    let p = b.op(Inst::CompareEqual, StorageKind::None, 0, vec![t, zero]);
    let c = b.def(AggType::BOOL, p);
    let bl = b.op(Inst::Ballot, StorageKind::None, 0, vec![c]);
    let _ = b.def(AggType::U32, bl);
    let va = b.op(Inst::VoteAll, StorageKind::None, 0, vec![c]);
    let _ = b.def(AggType::BOOL, va);
    let sh = b.op(Inst::ShuffleXor, StorageKind::None, 0, vec![t, t]);
    let _ = b.def(AggType::U32, sh);
    let mb = b.op(Inst::MemoryBarrier, StorageKind::None, 0, vec![]);
    b.put(mb);
    let gb = b.op(Inst::GroupMemoryBarrier, StorageKind::None, 0, vec![]);
    b.put(gb);
    let cv = b.op(Inst::ConvertU32ToFP32, StorageKind::None, 0, vec![t]);
    let f = b.def(AggType::FP32, cv);
    let fl = b.op(fp(Inst::Floor), StorageKind::None, 0, vec![f]);
    let fl = b.def(AggType::FP32, fl);
    let fm = b.op(
        fp(Inst::FusedMultiplyAdd),
        StorageKind::None,
        0,
        vec![fl, f, f],
    );
    let _ = b.def(AggType::FP32, fm);
    b
}

pub(super) fn fragment_builtins() -> B {
    let mut b = fragment();
    for v in [
        IoVariable::PrimitiveId,
        IoVariable::FragmentCoord,
        IoVariable::FrontFacing,
    ] {
        b.io(StorageKind::Input, v, 0);
    }
    b.load_scalar(IoVariable::PrimitiveId, AggType::S32);
    b.load_io(
        StorageKind::Input,
        IoVariable::FragmentCoord,
        None,
        0,
        AggType::FP32,
    );
    b.load_scalar(IoVariable::FrontFacing, AggType::BOOL);
    b
}

#[test]
fn compute_subgroup_constructs() {
    let l = compute_subgroup().translate().libraries.remove(0);
    for want in [
        "simd_ballot(",
        "simd_all(",
        "simd_shuffle_xor(",
        "threadgroup_barrier(mem_flags::mem_device",
        "floor(",
    ] {
        assert!(l.msl.contains(want), "missing {want}\n{}", l.msl);
    }
}

#[test]
fn fragment_builtins_are_parameters() {
    let t = fragment_builtins().translate();
    let r = t.libraries[0].reflection.as_ref().expect("reflection");
    assert!(r.raster.primitive_id);
}

/// The samples compile with Apple's compiler. Run by hand: `cargo test -p nxb-xlate -- --ignored metal`.
#[test]
#[ignore]
fn metal_compiles_the_samples() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scratch");
    std::fs::create_dir_all(&dir).expect("scratch");
    let samples = [
        ("vs", vertex()),
        ("ps", fragment()),
        ("cs", compute()),
        ("cs_subgroup", compute_subgroup()),
        ("ps_builtins", fragment_builtins()),
    ];
    for (n, b) in samples {
        let src = dir.join(format!("sample_{n}.metal"));
        let air = dir.join(format!("sample_{n}.air"));
        std::fs::write(&src, b.msl()).expect("write");
        let o = std::process::Command::new("xcrun")
            .args([
                "-sdk",
                "macosx",
                "metal",
                "-c",
                "-O1",
                "-fpreserve-invariance",
                "-fwrapv",
                "-std=metal3.0",
            ])
            .arg(&src)
            .arg("-o")
            .arg(&air)
            .output()
            .expect("run metal");
        assert!(
            o.status.success(),
            "{n}: {}\n{}",
            String::from_utf8_lossy(&o.stderr),
            b.msl()
        );
    }
}
