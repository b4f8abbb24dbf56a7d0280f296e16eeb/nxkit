//! The `_bf` library: the pixel shader with a framebuffer fetch ("readable tile") variant.
//!
//! The emitter writes the shader a second time with a [`State`]. A texture sample of the main function that is a
//! plain 2D float sample at a coordinate the host understands (see [`How`]) becomes
//! `FX_i == 0 ? fb0 : FX_i == 1 ? fb1 : ... : <sample>`. The host picks the colour attachment `k` that holds the
//! texture with the function constants `FX_i` (`i` is the call number), the shader reads it through the parameters
//! `fbK`. The result is one member per output and attachment, switched by `OM_j_k`.
//!
//! The entry is `main0_f`. The host asks the library for that name. The body stays `dm_body_main0`.
//! The engine's tools cut `_bf` from the one compile of the shader. Here `_bf` is a source of its own.

use super::fetch_origin::{How, Tracer};
use crate::msl::abi::Kind;
use crate::msl::args::Res;
use crate::msl::emit::{Em, Options, R, Shader, Shared};
use crate::msl::entry::{EntryName, Form, Param, early_tests, entry_text, params, params_decl};
use crate::msl::inst_tex::{
    DERIVATIVES, GATHER, INT_COORDS, LOD_BIAS, LOD_LEVEL, OFFSET, OFFSETS, Tref,
};
use crate::msl::io::{FragOut, Plan};
use crate::msl::prelude;
use crate::msl::walk::for_each_node;
use crate::structured::StructInfo;
use crate::structured::ast::{A, AstTex, Kind as NodeKind};
use crate::translation::msl_entry::Structured;
use crate::types::SamplerType;
use nxb_pyjson::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// Colour attachments a fetch reads.
pub const NFB: usize = 8;
/// Rewritten samples of one shader.
pub const MAX_CALLS: usize = 32;
/// Outputs the variant maps.
pub const MAX_OUTPUTS: usize = 4;
/// The function constants of the attachment parameters start here.
const FB_BASE: usize = 32;
/// The function constants of the output members start here.
const OM_BASE: usize = 64;

/// One rewritten sample.
#[derive(Clone, Debug)]
pub struct Call {
    /// The texture slot of the sampled texture.
    pub reg: i64,
    /// What the coordinate is.
    pub how: How,
}

/// The state of the emitter while it writes the variant.
#[derive(Debug, Default)]
pub struct State {
    /// The rewritten samples in order of appearance.
    pub calls: Vec<Call>,
    /// The operations on each texture member.
    pub uses: BTreeMap<String, u32>,
    /// The rewritten samples of each texture member.
    pub hits: BTreeMap<String, u32>,
    defs: BTreeMap<A, Vec<A>>,
}

impl State {
    /// A fresh state for a program.
    pub fn new(info: &StructInfo) -> State {
        let mut defs: BTreeMap<A, Vec<A>> = BTreeMap::new();
        for_each_node(info, &mut |_, k| {
            if let NodeKind::Assign { dest, src } = k {
                defs.entry(*dest).or_default().push(*src);
            }
        });
        State {
            defs,
            ..State::default()
        }
    }
}

/// The result of the variant.
#[derive(Debug)]
pub struct Fetched {
    /// The source of the `_bf` library.
    pub msl: String,
    /// The `_b.json` field `fetch`.
    pub meta: Value,
}

/// A sample without bias, level, offset, gradient, gather, shadow, array or multisampling.
fn plain_2d(t: &AstTex, r: &Tref) -> bool {
    let bad = GATHER | DERIVATIVES | INT_COORDS | LOD_BIAS | LOD_LEVEL | OFFSET | OFFSETS;
    !t.is_separate() && t.flags & bad == 0 && t.ty == SamplerType::TEXTURE_2D && !r.array
}

fn tex_reg(res: &Res, name: &str) -> Option<i64> {
    res.bindings
        .iter()
        .find(|b| b.msl_name == name && b.kind == Kind::Tex)
        .and_then(|b| b.reg)
}

impl Em<'_> {
    /// Count an operation on a texture.
    pub(in crate::msl) fn fetch_use(&mut self, name: &str) {
        if let Some(f) = self.fetch.as_mut() {
            *f.uses.entry(name.to_string()).or_default() += 1;
        }
    }

    /// The sample text, replaced by the framebuffer read when the host can use it. `srcs` are the coordinates.
    pub(in crate::msl) fn fetch_rewrite(
        &mut self,
        t: &AstTex,
        r: &Tref,
        srcs: &[A],
        s: String,
    ) -> String {
        let Some(st) = self.fetch.as_ref() else {
            return s;
        };
        if self.func != 0 || st.calls.len() >= MAX_CALLS || srcs.len() != 2 || !plain_2d(t, r) {
            return s;
        }
        let Some(tx) = self.res.texs.get(&(t.set, t.binding)) else {
            return s;
        };
        let Some(reg) = tex_reg(self.res, &tx.name) else {
            return s;
        };
        let name = tx.name.clone();
        let tr = Tracer {
            info: self.info,
            props: self.props,
            res: self.res,
            plan: self.plan,
            defs: &st.defs,
        };
        let Some(how) = tr.how(srcs[0], srcs[1]) else {
            return s;
        };
        let Some(st) = self.fetch.as_mut() else {
            return s;
        };
        let i = st.calls.len();
        st.calls.push(Call { reg, how });
        *st.hits.entry(name).or_default() += 1;
        let sel: String = (0..NFB)
            .map(|k| format!("FX_{i} == {k} ? fb{k} : "))
            .collect();
        format!("({sel}{s})")
    }
}

fn call_value(i: usize, c: &Call) -> Value {
    let mut v = Value::obj([("fc", Value::Int(i as i64)), ("tex_reg", Value::Int(c.reg))]);
    match &c.how {
        How::Varying { locn, proj } => {
            v.set("varying_locn", Value::Int(i64::from(*locn)));
            if *proj {
                v.set("proj", Value::Int(1));
            }
        }
        How::FragPos(p) => {
            let pair = |q: &[i64; 3]| Value::List(q.iter().map(|x| Value::Int(*x)).collect());
            v.set("frag_pos", Value::List(vec![pair(&p[0]), pair(&p[1])]));
        }
    }
    v
}

/// The registers of textures that the shader also uses in ways other than the rewritten samples.
fn mixed_regs(res: &Res, st: &State) -> Vec<Value> {
    let regs: BTreeSet<i64> = res
        .bindings
        .iter()
        .filter(|b| b.kind == Kind::Tex && b.msl_type == "texture2d<float>")
        .filter(|b| {
            let n = |m: &BTreeMap<String, u32>| m.get(&b.msl_name).copied().unwrap_or(0);
            n(&st.uses) > n(&st.hits)
        })
        .filter_map(|b| b.reg)
        .collect();
    regs.into_iter().map(Value::Int).collect()
}

fn constants(calls: usize, outs: usize) -> String {
    let mut t = String::new();
    for i in 0..calls {
        let _ = writeln!(t, "constant int FX_{i} [[function_constant({i})]];");
    }
    for k in 0..NFB {
        let _ = writeln!(
            t,
            "constant bool FB_{k} [[function_constant({})]];",
            FB_BASE + k
        );
    }
    for j in 0..outs {
        for k in 0..NFB {
            let _ = writeln!(
                t,
                "constant bool OM_{j}_{k} [[function_constant({})]];",
                OM_BASE + 8 * j + k
            );
        }
    }
    t
}

/// The colour outputs the host maps, and nothing else in the struct.
fn outputs(plan: &Plan) -> R<Vec<&FragOut>> {
    let plain = plan.frag_outs.iter().all(|m| m.color.is_some());
    let mut v: Vec<&FragOut> = plan
        .frag_outs
        .iter()
        .filter(|m| m.color.is_some() && !m.name.ends_with("_pad"))
        .collect();
    v.sort_by_key(|m| m.color);
    if !plain || v.is_empty() || v.len() > MAX_OUTPUTS {
        return Err(format!(
            "the outputs are not 1 to {MAX_OUTPUTS} plain colours"
        ));
    }
    Ok(v)
}

fn result_struct(outs: &[&FragOut]) -> String {
    let mut t = String::from("struct main0_out_f\n{\n");
    for (j, o) in outs.iter().enumerate() {
        for k in 0..NFB {
            let _ = writeln!(
                t,
                "    {} {}_at{k} [[color({k}), function_constant(OM_{j}_{k})]];",
                o.ty, o.name
            );
        }
    }
    t.push_str("};\n");
    t
}

/// The attachment parameters: the declaration of the body and the entry attribute.
fn attachments() -> Vec<Param> {
    (0..NFB)
        .map(|k| Param {
            decl: format!("float4 fb{k}"),
            attr: format!("color({k}), function_constant(FB_{k})"),
            name: format!("(FB_{k} ? fb{k} : float4(0.0))"),
        })
        .collect()
}

fn entry(outs: &[&FragOut], ps: &[Param], early: bool) -> String {
    let full: Vec<String> = ps.iter().map(Param::full).collect();
    let mut t = format!(
        "{}fragment main0_out_f main0_f({})\n{{\n    main0_out dm_r = dm_body_main0({});\n    main0_out_f out = {{}};\n",
        if early {
            "[[early_fragment_tests]] "
        } else {
            ""
        },
        full.join(", "),
        params_decl(ps).1
    );
    for (j, o) in outs.iter().enumerate() {
        for k in 0..NFB {
            let _ = writeln!(
                t,
                "    if (OM_{j}_{k}) out.{}_at{k} = dm_r.{};",
                o.name, o.name
            );
        }
    }
    t.push_str("    return out;\n}\n");
    t
}

/// Write the `_bf` library. `Ok(None)` when the shader has no rewritable sample and is not in the output only list.
pub fn build(
    s: &Structured,
    res: &Res,
    plan: &Plan,
    shared: &Shared,
    opt: &Options,
) -> R<Option<Fetched>> {
    let mut em = Em::new(&s.info, &s.props, res, opt, shared, plan, Shader::Fragment);
    em.fetch = Some(State::new(&s.info));
    let mut bodies = Vec::new();
    for i in 0..s.info.functions.len() {
        bodies.push(em.body(i)?);
    }
    let Some(st) = em.fetch.take() else {
        return Ok(None);
    };
    if st.calls.is_empty() && !opt.fetch_outputs_only {
        return Ok(None);
    }
    let outs = outputs(plan)?;
    let mut msl = prelude::header(s.props.host.reduced_precision);
    msl.push_str(&constants(st.calls.len(), outs.len()));
    msl.push_str(&prelude::helpers(&em.helpers, res.border));
    msl.push_str(&res.text);
    msl.push_str(&plan.in_struct());
    msl.push_str(&plan.out_struct());
    msl.push_str(&result_struct(&outs));
    msl.push_str(&crate::msl::library::helper_functions(&em, &bodies)?);
    let main = bodies.first().ok_or("program without a main function")?;
    let extra = attachments();
    let name = EntryName {
        form: Form::Body { inline: false },
        extra: &extra,
        ..EntryName::new("main0")
    };
    msl.push_str(&entry_text(s, &name, Shader::Fragment, res, plan, main)?);
    let mut ps = params(res, plan);
    ps.extend(extra);
    msl.push_str(&entry(&outs, &ps, early_tests(s, res)));
    let meta = Value::obj([
        (
            "calls",
            Value::List(
                st.calls
                    .iter()
                    .enumerate()
                    .map(|(i, c)| call_value(i, c))
                    .collect(),
            ),
        ),
        ("mixed_regs", Value::List(mixed_regs(res, &st))),
        (
            "outputs",
            Value::List(
                outs.iter()
                    .map(|o| {
                        Value::obj([
                            ("color", Value::Int(i64::from(o.color.unwrap_or(0)))),
                            ("type", Value::str(o.ty.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    Ok(Some(Fetched { msl, meta }))
}
