//! The translation pipeline up to the phase dumps of the probe, used by the corpus tests (`emit` now, later `ssa`, `opt`...).

use super::emit_shader::{emit_shader, emit_shader_mode};
use crate::accessor::Accessor;
use crate::ir::{Ir, text};
use crate::job::Job;
use crate::translation::context::{Options, TranslatorContext, set_current_memories};
use crate::translation::resources::ResourceManager;

/// `Options` of a job as `dmsh_xlate` builds the flags.
pub fn options_of(job: &Job) -> Options {
    Options {
        debug_mode: job.debug,
        compute: job.stage == "cs",
        ampere: job.arch_sm86,
    }
}

/// `TranslatorContext.Translate`'s resource manager set-up (`CreateResourceManager`, local and shared memory).
pub fn create_resource_manager(tc: &TranslatorContext, acc: &mut Accessor) -> ResourceManager {
    let mut rm = ResourceManager::new(tc.stage().into(), None);
    set_current_memories(tc, acc, &mut rm);
    rm
}

/// The text of the `emit` phase for a job (what `Probe.Ops("emit", ...)` prints for the first attempt).
pub fn emit_text(job: &Job, code: &[u8]) -> Result<String, String> {
    let mut acc = Accessor::new(job);
    let tc = TranslatorContext::create(code, &mut acc, options_of(job))?;
    let mut rm = create_resource_manager(&tc, &mut acc);
    let (ir, funcs) = emit_shader(&tc, &mut acc, &mut rm, Ir::default(), true)?;
    let mut out = String::new();
    for (i, f) in funcs.iter().enumerate() {
        text::print_ops(&ir, i, &f.code, &mut out);
    }
    Ok(out)
}

/// Lenient emit: the text and the emitter errors, for the per-instruction comparison.
pub fn emit_text_lenient(
    job: &Job,
    code: &[u8],
) -> Result<(String, Vec<(String, String)>), String> {
    let mut acc = Accessor::new(job);
    let tc = TranslatorContext::create(code, &mut acc, options_of(job))?;
    let mut rm = create_resource_manager(&tc, &mut acc);
    let (ir, funcs, errs) = emit_shader_mode(&tc, &mut acc, &mut rm, Ir::default(), true, true)?;
    let mut out = String::new();
    for (i, f) in funcs.iter().enumerate() {
        text::print_ops(&ir, i, &f.code, &mut out);
    }
    Ok((out, errs))
}

/// Split an `emit` dump into per-instruction chunks (name, text with locals and labels renumbered within the chunk).
pub fn chunks(text: &str) -> Vec<(String, String)> {
    fn renumber(chunk: &[&str]) -> String {
        let mut map: std::collections::HashMap<String, usize> = Default::default();
        let mut out = String::new();
        for line in chunk {
            let mut tok = String::new();
            let flush =
                |tok: &mut String,
                 out: &mut String,
                 map: &mut std::collections::HashMap<String, usize>| {
                    let b = tok.as_bytes();
                    if b.len() > 1
                        && (b[0] == b'l' || b[0] == b'L')
                        && b[1..].iter().all(u8::is_ascii_digit)
                    {
                        let n = map.len();
                        let k = *map.entry(tok.clone()).or_insert(n);
                        out.push(b[0] as char);
                        out.push_str(&k.to_string());
                    } else {
                        out.push_str(tok);
                    }
                    tok.clear();
                };
            if line.trim_start().starts_with("cm ") {
                out.push_str(line);
            } else {
                for c in line.chars() {
                    if c.is_ascii_alphanumeric() {
                        tok.push(c);
                    } else {
                        flush(&mut tok, &mut out, &mut map);
                        out.push(c);
                    }
                }
                flush(&mut tok, &mut out, &mut map);
            }
            out.push('\n');
        }
        out
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut res = Vec::new();
    let mut start: Option<usize> = None;
    let mut push = |s: usize, e: usize| {
        let name = lines[s].rsplit(' ').next().unwrap_or("").to_string();
        res.push((name, renumber(&lines[s..e])));
    };
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("cm ") {
            if let Some(s) = start {
                push(s, i);
            }
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push(s, lines.len());
    }
    res
}

/// One function after `TransformPasses.RunPass`: `Function` without the name (it is `fun<index>`).
pub struct TransformedFunction {
    pub blocks: Vec<crate::ir::Block>,
    pub in_arguments: usize,
    pub out_arguments: usize,
}

/// The state after `TransformPasses.RunPass` of every function (the input of `StructuredProgram`).
pub struct Transformed {
    pub ir: Ir,
    /// The functions of the shader in id order (function 0 is the main function).
    pub funcs: Vec<TransformedFunction>,
    pub rm: ResourceManager,
    pub used_features: u32,
    pub hfm: crate::helper::Hfm,
}

/// Decode, emit, CFG, SSA, optimiser and transforms of a shader (`Translate` up to `StructuredProgram`).
pub fn run_to_xform(tc: &TranslatorContext, acc: &mut Accessor) -> Result<Transformed, String> {
    run_to_xform_mode(tc, acc, false)
}

/// `run_to_xform` for `Translate(asCompute)`: a geometry shader is emulated by a compute shader.
pub fn run_to_xform_mode(
    tc: &TranslatorContext,
    acc: &mut Accessor,
    as_compute: bool,
) -> Result<Transformed, String> {
    use crate::cfg::{Cfg, register_usage, ssa};
    use crate::helper::Hfm;
    use crate::opt::{Env, PassCtx};
    use crate::translation::props::ShaderStage;
    use crate::xform::XformCtx;
    let mut rm = if as_compute {
        crate::translation::vac::create_geometry_resource_manager(tc, acc)?
    } else {
        create_resource_manager(tc, acc)
    };
    let (mut ir, codes) = if as_compute {
        super::emit_shader::emit_shader_compute(tc, acc, &mut rm, Ir::default(), true)?
    } else {
        emit_shader(tc, acc, &mut rm, Ir::default(), true)?
    };
    if codes.is_empty() {
        return Err("no function".into());
    }
    let mut cfgs = Vec::with_capacity(codes.len());
    for f in &codes {
        cfgs.push(Cfg::create(&ir, &f.code)?);
    }
    let mut frus = vec![register_usage::FunctionRegisterUsage::default(); codes.len()];
    for i in 1..codes.len() {
        frus[i] = register_usage::run_pass(&mut ir, &mut cfgs[i]);
    }
    let stage: ShaderStage = tc.stage().into();
    let mut used_features = tc.program.used_features as u32;
    if as_compute {
        used_features |= crate::xform::feature::VTG_AS_COMPUTE;
    }
    let mut hfm = Hfm::new(codes.len(), stage);
    let mut env = Env { rm: &mut rm, acc };
    let pctx = PassCtx {
        fragment: stage == ShaderStage::Fragment,
        host_supports_float64: acc_float64(env.acc),
    };
    let mut funcs = Vec::with_capacity(codes.len());
    for (i, mut cfg) in cfgs.into_iter().enumerate() {
        if !cfg.blocks.is_empty() {
            register_usage::fixup_calls(&mut ir, &cfg.blocks, &frus)?;
            cfg.find_dominators()?;
            cfg.find_dominance_frontiers()?;
            ssa::rename(&mut ir, &mut cfg)?;
            crate::opt::run_pass(&mut ir, &mut cfg.blocks, &mut env, &pctx, &mut hfm)?;
            let mut x = XformCtx {
                stage,
                used_features: &mut used_features,
                hfm: &mut hfm,
                definitions: Some(&tc.definitions),
                idom: &cfg.idom,
            };
            crate::xform::run_pass(&mut ir, &mut cfg.blocks, &mut env, &mut x)?;
        }
        funcs.push(TransformedFunction {
            blocks: cfg.blocks,
            in_arguments: frus[i].in_arguments.len(),
            out_arguments: frus[i].out_arguments.len(),
        });
    }
    Ok(Transformed {
        ir,
        funcs,
        rm,
        used_features,
        hfm,
    })
}

fn acc_float64(acc: &Accessor) -> bool {
    acc.query_host_supports_shader_float64()
}

/// The text of the `xform` phase for a job.
pub fn xform_text(job: &Job, code: &[u8]) -> Result<String, String> {
    let mut acc = Accessor::new(job);
    let tc = TranslatorContext::create(code, &mut acc, options_of(job))?;
    let t = run_to_xform(&tc, &mut acc)?;
    let mut out = String::new();
    for (i, f) in t.funcs.iter().enumerate() {
        text::print_blocks(&t.ir, i, &f.blocks, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Identical count of the `xform` phase when the whole pipeline runs from the machine code.
    #[test]
    fn xform_phase() {
        let Some(entries) = crate::corpus::load() else {
            return;
        };
        let (mut same, mut total) = (0, 0);
        let mut why: BTreeMap<String, usize> = BTreeMap::new();
        for e in &entries {
            let Some(want) = e.first_attempt("xform") else {
                continue;
            };
            total += 1;
            match xform_text(&e.job, &e.code) {
                Ok(got) if got == want => same += 1,
                Ok(_) => *why.entry("differs".into()).or_default() += 1,
                Err(m) => *why.entry(m).or_default() += 1,
            }
        }
        println!("xform phase identical: {same} of {total}");
        for (m, n) in why.iter().take(10) {
            println!("  {n:5} {m}");
        }
    }

    /// Per instruction name: chunks identical to the probe's `emit` dump and chunks total (lenient emit: unported emitters
    /// leave an empty chunk). This is the progress meter for the instruction emitters.
    #[test]
    fn emit_chunks() {
        let Some(entries) = crate::corpus::load() else {
            return;
        };
        let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for e in &entries {
            let Some(want) = e.first_attempt("emit") else {
                continue;
            };
            let Ok((got, _)) = emit_text_lenient(&e.job, &e.code) else {
                continue;
            };
            let (g, w) = (chunks(&got), chunks(&want));
            if g.len() != w.len() {
                continue;
            }
            for (a, b) in g.iter().zip(w.iter()) {
                let ent = per.entry(b.0.clone()).or_default();
                ent.1 += 1;
                if a.1 == b.1 {
                    ent.0 += 1;
                }
            }
        }
        let (ok, all) = per.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
        println!("emit chunks identical: {ok} of {all}");
        let mut v: Vec<_> = per.into_iter().collect();
        v.sort_by_key(|(_, (a, t))| std::cmp::Reverse(t - a));
        for (n, (a, t)) in v.iter().filter(|(_, (a, t))| a != t) {
            println!("  {n:10} {a:6} / {t}");
        }
    }

    /// Prints the identical count of the `emit` phase and the most frequent failures. Asserts the count only when
    /// `NXB_XLATE_REQUIRE_EMIT` is set to a minimum.
    #[test]
    fn emit_phase() {
        let Some(entries) = crate::corpus::load() else {
            return;
        };
        let mut same = 0;
        let mut total = 0;
        let mut why: BTreeMap<String, usize> = BTreeMap::new();
        for e in &entries {
            let Some(want) = e.first_attempt("emit") else {
                continue;
            };
            total += 1;
            match emit_text(&e.job, &e.code) {
                Ok(got) if got == want => same += 1,
                Ok(got) => {
                    let line = got
                        .lines()
                        .zip(want.lines())
                        .position(|(a, b)| a != b)
                        .map_or("length".to_string(), |i| format!("line {i}"));
                    *why.entry(format!("differs at {line}")).or_default() += 1;
                }
                Err(m) => *why.entry(m).or_default() += 1,
            }
        }
        let mut top: Vec<_> = why.into_iter().collect();
        top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!("emit phase identical: {same} of {total}");
        for (m, n) in top.iter().take(25) {
            println!("  {n:5} {m}");
        }
        if let Some(min) = std::env::var("NXB_XLATE_REQUIRE_EMIT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            assert!(same >= min, "emit identical {same} < {min}");
        }
    }
}
