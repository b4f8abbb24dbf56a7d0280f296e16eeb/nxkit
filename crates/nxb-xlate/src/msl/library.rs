//! The libraries of a shader: the base library `_b` with its reflection, and the variant libraries that belong to it.
//!
//! A vertex shader adds `_bv` and `_bp`. A pixel shader adds `_bf` and the side units. The entries of each stage are
//! written by `variant::vertex` and `variant::pixel`. `entry` writes the functions.

use super::args;
use super::emit::{Em, Options, R, Shader};
use super::entry::{EntryName, Form, StageIn, entry_text, params, shared_params};
use super::refl::Reflection;
use super::usage::{self, Usage};
use super::variant::pixel::Built;
use super::{
    Context, Library, Translated, facts, first_store, io, mesh, prelude, spill, variant, wide,
};
use crate::translation::msl_entry::Structured;
use nxb_pyjson::Value;
use std::fmt::Write;

/// The kind keyword of a shader.
fn kind(s: Shader) -> &'static str {
    match s {
        Shader::Vertex => "vertex",
        Shader::Fragment => "fragment",
        Shader::Compute => "kernel",
    }
}

fn tg_bytes(s: &Structured) -> i64 {
    s.props
        .properties
        .shared_memories
        .iter()
        .map(|m| i64::from(m.ty.size_in_bytes()) * i64::from(m.array_length.max(1)))
        .sum()
}

/// Build the base library `_b` and its reflection.
pub fn build(s: &Structured, ctx: &Context<'_>, opt: &Options) -> R<Translated> {
    build_with(s, ctx, opt, &[])
}

/// What the entries of a library add to its text and to the reflection of `_b`.
#[derive(Default)]
struct Entries {
    /// The text of all entries.
    text: String,
    /// The reflection fields that name the entries.
    entries: Vec<(String, Value)>,
    /// Further reflection fields.
    extra: Vec<(String, Value)>,
}

/// Build with the vertex inputs at `ints` declared `uint`.
fn build_with(s: &Structured, ctx: &Context<'_>, opt: &Options, ints: &[u32]) -> R<Translated> {
    let job = &s.job;
    let patched = (!ints.is_empty()).then(|| variant::int_inputs::declare(&s.props, ints));
    let props = patched.as_ref().unwrap_or(&s.props);
    let shader = match job.stage.as_str() {
        "vs" => Shader::Vertex,
        "ps" => Shader::Fragment,
        "cs" | "gs" => Shader::Compute,
        other => return Err(format!("unported: {other} stage")),
    };
    // A task or mesh shader is a compute job, and its tables and reflection are those of its own stage.
    let stage_name = job
        .mesh_stage
        .as_ref()
        .map_or(job.stage.as_str(), |m| m.kind.as_str());
    if s.props.definitions.transform_feedback_enabled {
        return Err("unported: transform feedback layout in the shader definitions".into());
    }
    // A task or mesh shader keeps its memory in the threadgroup, so past the limit it is refused.
    let spill = if shader != Shader::Compute {
        None
    } else if job.mesh_stage.is_some() {
        if tg_bytes(s) > i64::from(super::abi::TG_MAX) {
            return Err("unported: threadgroup memory past 32 KB (tg_spill)".into());
        }
        None
    } else {
        spill::plan(&s.props.properties.shared_memories)
    };
    let usage: Usage = usage::collect(&s.info);
    let res = args::build(props, &s.program, &usage, stage_name, &s.notes, &s.fetch1d)?;
    let base_instance = shader == Shader::Vertex
        && res
            .cbs
            .values()
            .any(|b| b.name == "vp_c0" && usage.base_inst.contains(&b.def.binding));
    let lane_zero = shader == Shader::Vertex && opt.visibility_counter;
    let mut plan = io::build(
        &s.info,
        props,
        shader,
        usage.lane_id,
        base_instance,
        lane_zero,
    )?;
    let vs_pad = if shader == Shader::Vertex {
        variant::vertex_pad::apply(&mut plan, ctx.partners)
    } else {
        Vec::new()
    };
    let mut shared = shared_params(s, &res, &plan, shader, spill.as_ref())?;
    let first = (shader == Shader::Compute && job.mesh_stage.is_none())
        .then(|| first_store::find(&s.info, &res, local_size(s)))
        .flatten();
    if let Some(f) = &first {
        shared.skip = f.skip.clone();
    }
    shared.wide = wide::plan(&s.info, &res, &shared.skip);
    let mut em = Em::new(&s.info, props, &res, opt, &shared, &plan, shader);
    em.probe = (shader == Shader::Vertex && ints.is_empty()).then(Vec::new);
    let mut bodies = Vec::new();
    for i in 0..s.info.functions.len() {
        bodies.push(em.body(i)?);
    }
    if let Some(uses) = em.probe.take() {
        let declared = s
            .job
            .attr
            .iter()
            .filter_map(|(k, _)| u32::try_from(*k).ok())
            .collect();
        let float = |l: u32| props.definitions.attribute_types.get(l as usize) == Some(&0);
        let found = variant::int_inputs::find(&s.info, &uses, &declared, &float);
        if !found.is_empty() {
            return build_with(s, ctx, opt, &found);
        }
    }
    if opt.visibility_counter && shader == Shader::Vertex {
        em.notes.push(
            "visibility counter lowered to singleton groups (one atomic add per passing vertex)"
                .into(),
        );
    }
    let main = bodies.first().ok_or("program without a main function")?;
    let mut notes = s.notes.clone();
    let mut libraries = Vec::new();
    let mut side = None;
    let mut entries = Entries::default();
    let mut fetch = None;
    let vx = match shader {
        Shader::Vertex => {
            let w = res.bindings.iter().any(facts::writes);
            Some(variant::vertex::bodies(&mut em, &plan, main.clone(), w)?)
        }
        _ => None,
    };
    let capture = match (&job.tf, shader) {
        (Some(tf), Shader::Vertex) => Some((tf, variant::tf::body(&mut em, &plan, tf)?)),
        _ => None,
    };
    let lib = Lib {
        in_struct: plan.in_struct(),
        fetch: false,
    };
    match (&vx, shader) {
        (Some(vx), _) => {
            let frame = variant::vertex::Frame::default();
            entries.text = variant::vertex::entries(s, &res, &plan, vx, &frame)?;
            entries.entries = variant::vertex::fields(vx);
            if let Some((tf, body)) = &capture {
                let reader = s.geometry_reader;
                entries
                    .text
                    .push_str(&variant::tf::entry(s, &res, &plan, body, tf, reader)?);
                entries.entries.push(variant::tf::entry_field());
                entries.extra.push(variant::tf::strides_field(tf));
            }
        }
        (None, Shader::Fragment) => {
            let derived = variant::pixel_derived::derived(&s.info, &plan, opt.dual_source);
            let built = variant::pixel::assemble(s, &res, &plan, main, &derived)?;
            side = built.side.clone();
            entries = entries_of(built);
            match variant::fetch::build(s, &res, &plan, &shared, opt) {
                Ok(Some(f)) => {
                    entries.extra.push(("fetch".into(), f.meta));
                    fetch = Some(f.msl);
                }
                Ok(None) => {}
                Err(e) => notes.push(format!("no fetch variant: {e}")),
            }
        }
        (None, _) => match &job.mesh_stage {
            Some(m) => {
                let body = EntryName {
                    form: Form::Body { inline: true },
                    ..EntryName::new("main0")
                };
                entries.text = entry_text(s, &body, shader, &res, &plan, main)?;
                let words = (job.shared.max(0) + 3) / 4;
                let shared = s.props.properties.shared_memories.len();
                entries.text.push('\n');
                entries.text.push_str(&mesh::wrap(
                    m,
                    &params(&res, &plan),
                    shared,
                    words,
                    ctx.partners,
                )?);
            }
            None => {
                let name = EntryName {
                    spill: spill.as_ref(),
                    ..EntryName::new("main0")
                };
                entries.text = entry_text(s, &name, shader, &res, &plan, main)?;
            }
        },
    }
    let msl = assemble(
        s,
        &em,
        &res,
        &plan,
        &bodies,
        vx.as_ref(),
        &lib,
        &entries.text,
    )?;
    libraries.push(Library {
        suffix: "_b".into(),
        msl,
        reflection: None,
    });
    if let Some(vx) = &vx {
        for (l, (k, v)) in vertex_libraries(s, &em, &res, &plan, &bodies, vx)? {
            entries.extra.push((k.to_string(), v));
            libraries.push(l);
        }
    }
    if let Some(msl) = fetch {
        libraries.push(Library {
            suffix: "_bf".into(),
            msl,
            reflection: None,
        });
    }

    notes.extend(plan.notes.iter().cloned());
    notes.extend(em.notes.iter().cloned());
    if ctx.no_facts {
        notes.push("not in the answer table, queries use defaults".into());
    }
    let reads = facts::drv_reads(&em.drv);
    let mut r = Reflection {
        key: format!("{}_b", job.key),
        chain: ctx.chain.to_string(),
        dm_ctl: job.key.clone(),
        source: ctx.source.clone(),
        pc_paired: ctx.pc_paired.to_string(),
        stage: stage_name.to_string(),
        entry: "main0".into(),
        msl_entry_kind: kind(shader).into(),
        threadgroup_size: (shader == Shader::Compute).then(|| local_size(s)),
        c1: facts::hex(&job.c1),
        notes,
        drv_unfillable: super::refl::unfillable(&reads, stage_name),
        drv_reads: reads,
        pass_scale: usage.pass_scale,
        bindings: res.bindings.clone(),
        vertex_inputs: facts::vertex_inputs(&plan),
        vs_pad,
        entries: entries.entries,
        ..Reflection::default()
    };
    for (k, v) in entries.extra {
        r.set_extra(&k, v);
    }
    if let Some(sp) = &spill {
        r.set_extra("tg_spill", Value::Int(sp.total));
    }
    if let Some(f) = first.filter(|f| !f.stores.is_empty()) {
        r.set_extra("first_store", first_store::value(&f));
    }
    if let Some(m) = &job.mesh_stage {
        mesh::reflect(m, &mut r);
    }
    r.raster = if shader == Shader::Vertex {
        super::facts_vs::raster(&s.info, props, &plan, &res)
    } else {
        facts::raster(shader, &plan, usage.discard, &r.bindings)
    };
    libraries[0].reflection = Some(r);
    let mut varyings = plan.varyings.clone();
    varyings.sort_by_key(|v| v.location);
    Ok(Translated {
        key: job.key.clone(),
        stage: stage_name.to_string(),
        libraries,
        inputs: super::StageInputs { varyings },
        side,
    })
}

fn entries_of(b: Built) -> Entries {
    Entries {
        text: b.text,
        entries: b.entries,
        extra: b.extra,
    }
}

/// The pieces of a library that differ between the base library and its vertex variants.
struct Lib {
    in_struct: String,
    /// The fetch helpers follow `using namespace metal;`.
    fetch: bool,
}

/// The text of a library: the common part, then the entries.
#[allow(clippy::too_many_arguments)]
fn assemble(
    s: &Structured,
    em: &Em<'_>,
    res: &args::Res,
    plan: &io::Plan,
    bodies: &[String],
    vx: Option<&variant::vertex::Bodies>,
    lib: &Lib,
    entries: &str,
) -> R<String> {
    let mut msl = prelude::header(s.props.host.reduced_precision);
    if lib.fetch {
        msl = msl.replacen(
            "using namespace metal;\n",
            &format!("using namespace metal;\n\n{}", variant::pull::FETCH),
            1,
        );
    }
    msl.push_str(&prelude::helpers(&em.helpers, res.border));
    msl.push_str(&res.text);
    msl.push_str(&lib.in_struct);
    msl.push_str(&plan.out_struct());
    if let Some(vx) = vx {
        msl.push_str(&variant::vertex::decls(plan, vx)?);
    }
    msl.push_str(&helper_functions(em, bodies)?);
    msl.push_str(entries);
    Ok(msl)
}

/// The helper functions `fn1` and up, each one after the functions it calls. The own front end rejects
/// prototypes, so they are written only when the calls form a cycle, and then for every helper in index order.
pub(crate) fn helper_functions(em: &Em<'_>, bodies: &[String]) -> R<String> {
    let mut t = String::new();
    let order = callee_order(bodies);
    if order.is_none() {
        for i in 1..bodies.len() {
            let _ = writeln!(t, "{};", em.signature(i)?);
        }
    }
    let order = order.unwrap_or_else(|| (1..bodies.len()).collect());
    for i in order {
        let b = bodies.get(i).ok_or("unknown function")?;
        let _ = write!(t, "{}\n{{\n{b}}}\n", em.signature(i)?);
    }
    Ok(t)
}

/// The helpers `1..bodies.len()` with every callee before its callers, `None` when the calls form a cycle.
fn callee_order(bodies: &[String]) -> Option<Vec<usize>> {
    let n = bodies.len();
    let calls = |i: usize, j: usize| {
        let name = format!("fn{j}(");
        bodies[i].match_indices(&name).any(|(at, _)| {
            !bodies[i][..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    };
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n.saturating_sub(1));
    while order.len() + 1 < n {
        let next =
            (1..n).find(|&i| !done[i] && (1..n).all(|j| j == i || done[j] || !calls(i, j)))?;
        if calls(next, next) {
            return None;
        }
        done[next] = true;
        order.push(next);
    }
    Some(order)
}

/// The libraries `_bv` and `_bp` of a vertex shader with the reflection fields that describe them.
fn vertex_libraries(
    s: &Structured,
    em: &Em<'_>,
    res: &args::Res,
    plan: &io::Plan,
    bodies: &[String],
    vx: &variant::vertex::Bodies,
) -> R<Vec<(Library, (&'static str, Value))>> {
    let mut out = Vec::new();
    let mut add = |suffix: &str,
                   lib: &Lib,
                   frame: &variant::vertex::Frame<'_>,
                   field: (&'static str, Value)|
     -> R<()> {
        let entries = variant::vertex::entries(s, res, plan, vx, frame)?;
        let msl = assemble(s, em, res, plan, bodies, Some(vx), lib, &entries)?;
        out.push((
            Library {
                suffix: suffix.into(),
                msl,
                reflection: None,
            },
            field,
        ));
        Ok(())
    };
    if let Some(v) = variant::vin::build(plan) {
        let lib = Lib {
            in_struct: v.in_struct.clone(),
            fetch: false,
        };
        let frame = variant::vertex::Frame {
            stage_in: StageIn::Custom(&v.in_param),
            extra: &[],
            pro: &v.pro,
            plain_only: true,
        };
        add("_bv", &lib, &frame, ("vin", variant::vin::field(&v)))?;
    }
    if let Some(p) = variant::pull::build(plan) {
        let lib = Lib {
            in_struct: p.in_struct.clone(),
            fetch: true,
        };
        let frame = variant::vertex::Frame {
            stage_in: StageIn::None,
            extra: &p.params,
            pro: &p.pro,
            plain_only: true,
        };
        add("_bp", &lib, &frame, ("pull", variant::pull::field(&p)))?;
    }
    Ok(out)
}

fn local_size(s: &Structured) -> [u32; 3] {
    let d = s.props.definitions.compute_local_size;
    let from_job = s.job.local.as_deref().unwrap_or(&[]);
    let at = |i: usize| {
        let j = from_job.get(i).copied().unwrap_or(0);
        u32::try_from(if j > 0 { j } else { i64::from(d[i]) })
            .unwrap_or(1)
            .max(1)
    };
    [at(0), at(1), at(2)]
}

#[cfg(test)]
mod order_tests {
    use super::callee_order;

    fn bodies(b: &[&str]) -> Vec<String> {
        b.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn callees_come_before_their_callers() {
        let b = bodies(&["fn1();", "x = fn3(a);", "", "fn2(); dm_fn1(b);"]);
        assert_eq!(callee_order(&b), Some(vec![2, 3, 1]));
    }

    #[test]
    fn a_name_with_more_digits_is_another_function() {
        let b = bodies(&["", "fn11(a);", "", "", "", "", "", "", "", "", "", ""]);
        assert_eq!(callee_order(&b).map(|o| o[0]), Some(2));
    }

    #[test]
    fn a_cycle_gives_no_order() {
        assert_eq!(callee_order(&bodies(&["", "fn2();", "fn1();"])), None);
        assert_eq!(callee_order(&bodies(&["", "fn1();"])), None);
    }

    #[test]
    fn no_helpers_give_an_empty_order() {
        assert_eq!(callee_order(&bodies(&["main"])), Some(vec![]));
        assert_eq!(callee_order(&[]), Some(vec![]));
    }
}
