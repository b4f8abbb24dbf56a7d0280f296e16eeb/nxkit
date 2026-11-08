use super::*;
use crate::cfg::{Cfg, ssa};
use crate::ir::text;

pub(crate) fn to_ssa(ir: &mut Ir, emit: &str) -> Result<Cfg, String> {
    let funcs = text::parse(ir, emit)?;
    let mut cfg = Cfg::create(ir, &funcs[0].blocks[0].ops)?;
    crate::cfg::register_usage::fixup_calls(ir, &cfg.blocks, &[])?;
    cfg.find_dominators()?;
    cfg.find_dominance_frontiers()?;
    ssa::rename(ir, &mut cfg)?;
    Ok(cfg)
}

/// Rebuild the resource manager of a translation by replaying the probe's call log up to `until`.
pub(crate) fn replay_res<'j>(
    job: &'j crate::job::Job,
    text: &str,
    until: &str,
) -> (
    crate::translation::resources::ResourceManager,
    crate::accessor::Accessor<'j>,
) {
    use crate::accessor::Accessor;
    use crate::translation::props::{ShaderStage, TextureFlags};
    use crate::translation::resources::ResourceManager;
    use crate::translation::resources_res::ResourceReservations;
    let mut acc = Accessor::new(job);
    let mut rm: Option<ResourceManager> = None;
    let mut seen_new = 0;
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let n = |i: usize| f[i].parse::<i32>().expect("number");
        match f[0] {
            "new" => {
                seen_new += 1;
                if seen_new > 1 {
                    break;
                }
                let stage = match n(1) {
                    0 => ShaderStage::Compute,
                    1 => ShaderStage::Vertex,
                    2 => ShaderStage::TessellationControl,
                    3 => ShaderStage::TessellationEvaluation,
                    4 => ShaderStage::Geometry,
                    _ => ShaderStage::Fragment,
                };
                rm = Some(ResourceManager::new(
                    stage,
                    Some(ResourceReservations::new_counts(false, false)),
                ));
            }
            m if m.starts_with('@') => {
                if &m[1..] == until {
                    break;
                }
            }
            _ => {
                let r = rm.as_mut().expect("rm");
                match f[0] {
                    "local" => r.set_current_local_memory(n(1), n(2) != 0),
                    "shared" => r.set_current_shared_memory(n(1), n(2) != 0),
                    "cb" => {
                        r.get_constant_buffer_binding(&mut acc, n(1)).expect("cb");
                    }
                    "sb" => {
                        r.try_get_storage_buffer_binding(&mut acc, n(1), n(2), n(3) != 0);
                    }
                    "tex" => {
                        r.get_texture_or_image_binding(
                            &mut acc,
                            crate::ir::Inst(n(1) as u32),
                            crate::types::SamplerType(n(2) as u32),
                            n(3) as usize,
                            TextureFlags(n(4) as u32),
                            n(5),
                            n(6),
                            n(7),
                            n(8) != 0,
                        )
                        .expect("tex");
                    }
                    "q" => {
                        r.set_usage_flags_for_texture_query(
                            n(1),
                            crate::types::SamplerType(n(2) as u32),
                        )
                        .expect("q");
                    }
                    "ucb" => r.set_used_constant_buffer_binding(n(1)),
                    other => panic!("unsupported resource call {other}"),
                }
            }
        }
    }
    (rm.expect("rm"), acc)
}

#[test]
fn full_opt_vs_dump() {
    let Some(entries) = crate::corpus::load() else {
        return;
    };
    let Some(dump) = std::env::var_os("NXB_XLATE_DUMP") else {
        return;
    };
    let (mut keys, mut ok, mut errs) = (0, 0, 0);
    let mut seen = std::collections::HashSet::new();
    for e in &entries {
        if !seen.insert(e.job.key.clone()) {
            continue;
        }
        let res_path = std::path::Path::new(&dump).join(format!("{}.res.txt", e.job.key));
        let (Some(emit), Some(opt_t), Ok(res_t)) = (
            e.first_attempt("emit"),
            e.first_attempt("opt"),
            std::fs::read_to_string(res_path),
        ) else {
            continue;
        };
        keys += 1;
        let mut ir = Ir::default();
        let mut cfg = to_ssa(&mut ir, &emit).expect("ssa");
        let (mut rm, mut acc) = replay_res(&e.job, &res_t, "emit");
        let mut env = Env {
            rm: &mut rm,
            acc: &mut acc,
        };
        let ctx = PassCtx {
            fragment: e.job.stage == "ps",
            host_supports_float64: true,
        };
        let mut out = String::new();
        let mut hfm =
            crate::xform::helper::Hfm::new(1, crate::translation::props::ShaderStage::Vertex);
        match run_pass(&mut ir, &mut cfg.blocks, &mut env, &ctx, &mut hfm) {
            Ok(()) => text::print_blocks(&ir, 0, &cfg.blocks, &mut out),
            Err(m) => {
                errs += 1;
                if std::env::var_os("NXB_LIST").is_some() {
                    eprintln!("err {} {m}", e.job.key);
                }
            }
        }
        if out == opt_t {
            ok += 1;
        } else if std::env::var_os("NXB_LIST").is_some() {
            eprintln!("fail {}", e.job.key);
        } else if std::env::var_os("NXB_SHOW").is_some_and(|k| k == *e.job.key) {
            let _ = std::fs::write(std::env::temp_dir().join("mine.txt"), &out);
            let _ = std::fs::write(std::env::temp_dir().join("ref.txt"), &opt_t);
        }
    }
    eprintln!("full opt identical {ok}/{keys} ({errs} errors)");
}

#[test]
fn removing_a_use_keeps_a_producer_whose_other_result_is_in_use() {
    use crate::ir::{Block, StorageKind};
    let mut ir = Ir::default();
    let one = ir.const_(1);
    let coord = ir.local();
    let n0 = ir.new_op(Inst::Copy, StorageKind::None, Some(coord), &[one]);
    let (d0, d1) = (ir.local(), ir.local());
    let read = ir.new_tex_op(
        Inst::TextureSample,
        crate::types::SamplerType::TEXTURE_2D,
        0,
        0,
        0,
        0,
        0,
        &[Some(d0), Some(d1)],
        &[coord],
    );
    let (x, y) = (ir.local(), ir.local());
    let u0 = ir.new_op(Inst::Copy, StorageKind::None, Some(x), &[d0]);
    let u1 = ir.new_op(Inst::Copy, StorageKind::None, Some(y), &[d1]);
    let mut block = Block {
        ops: vec![n0, read, u0, u1],
        ..Block::default()
    };
    remove_node(&mut ir, &mut block, 2);
    // the read stays for its second result, so its coordinate stays in use
    assert!(!ir.data(coord).use_ops.is_empty());
    remove_node(&mut ir, &mut block, 2);
    assert!(ir.data(coord).use_ops.is_empty());
}

#[test]
fn the_is_nan_of_a_constant_is_folded_and_drops_out_of_an_or() {
    use crate::ir::{Block, StorageKind};
    use crate::opt::fold::fp;
    struct NoRes;
    impl crate::opt::Resources for NoRes {
        fn try_get_constant_buffer_slot(&self, _: i32) -> Option<i32> {
            None
        }
    }
    let mut ir = Ir::default();
    let x = ir.local();
    let src = ir.const_(7);
    let n0 = ir.new_op(Inst::Copy, StorageKind::None, Some(x), &[src]);
    let c = ir.const_f(0.96);
    let (nan_c, or) = (ir.local(), ir.local());
    let n1 = ir.new_op(fp(Inst::IsNan), StorageKind::None, Some(nan_c), &[c]);
    let n2 = ir.new_op(Inst::BitwiseOr, StorageKind::None, Some(or), &[x, nan_c]);
    let sink = ir.local();
    let n3 = ir.new_op(Inst::Copy, StorageKind::None, Some(sink), &[or]);
    let mut blocks = [Block {
        ops: vec![n0, n1, n2, n3],
        ..Block::default()
    }];
    run_optimization_passes(&mut ir, &mut blocks, &NoRes).expect("passes");
    assert!(
        !blocks[0]
            .ops
            .iter()
            .any(|n| ir.is_op(*n) && ir.op(*n).inst == fp(Inst::IsNan)),
        "the is-nan of a constant is gone"
    );
}
