//! The text of one function around a translated body: an entry function or a body function that entries call.
//!
//! `library` builds the libraries out of these functions. The variants (`variant::*`) write further functions with
//! other names and forms around the same helper functions.

use super::args::{self, SET_NAME, SET_TYPE};
use super::emit::{R, Shader, Shared};
use super::facts_ps::{self, KILL};
use super::io;
use super::spill::Spill;
use super::types::{msl_type, scalar};
use crate::props::MemoryDef;
use crate::structured::types::AggType;
use crate::translation::msl_entry::Structured;
use std::fmt::Write;

/// One parameter of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// The declaration without the attribute (`main0_in in`).
    pub decl: String,
    /// The attribute without the brackets (`stage_in`).
    pub attr: String,
    /// The name the body uses.
    pub name: String,
}

impl Param {
    /// A parameter from its parts.
    pub fn new(decl: impl Into<String>, attr: impl Into<String>, name: impl Into<String>) -> Param {
        Param {
            decl: decl.into(),
            attr: attr.into(),
            name: name.into(),
        }
    }

    /// The declaration with its attribute.
    pub fn full(&self) -> String {
        format!("{} [[{}]]", self.decl, self.attr)
    }
}

/// The parameters and arguments of a list of parameters, each joined by `, `.
pub fn params_decl(ps: &[Param]) -> (String, String) {
    let d: Vec<&str> = ps.iter().map(|p| p.decl.as_str()).collect();
    let a: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
    (d.join(", "), a.join(", "))
}

/// The parameters of the entry functions of a shader: the stage input, the argument buffer and the builtins.
pub fn params(res: &args::Res, plan: &io::Plan) -> Vec<Param> {
    let mut v = Vec::new();
    if !plan.in_members.is_empty() {
        v.push(Param::new("main0_in in", "stage_in", "in"));
    }
    if res.has_args {
        v.push(Param::new(
            format!("constant {SET_TYPE}& {SET_NAME}"),
            "buffer(0)",
            SET_NAME,
        ));
    }
    for b in &plan.builtins {
        v.push(Param::new(format!("{} {}", b.ty, b.name), b.attr, b.name));
    }
    v
}

/// The stage input of an entry function.
#[derive(Clone, Copy, Debug, Default)]
pub enum StageIn<'a> {
    /// `main0_in in [[stage_in]]`, when the plan has inputs.
    #[default]
    Standard,
    /// No stage input. The prologue declares `in`.
    None,
    /// This parameter instead. The prologue fills `in` from it.
    Custom(&'a Param),
}

/// How a function is written.
#[derive(Clone, Copy, Debug, Default)]
pub enum Form {
    /// An entry function with the stage keyword and its attributes.
    #[default]
    Entry,
    /// `static <result> dm_body_<name>(...)`: a function with the parameters without attributes that entries call.
    /// The shared memory is a parameter, because the caller owns it. `inline` forces the inlining.
    Body {
        /// Write `__attribute__((always_inline))`.
        inline: bool,
    },
}

/// What to write for one function. Build it with `EntryName { field: .., ..EntryName::new("main0") }`.
#[derive(Clone, Copy, Debug)]
pub struct EntryName<'a> {
    /// The function name. A body function is `dm_body_<name>`.
    pub name: &'a str,
    /// The form of the function.
    pub form: Form,
    /// The return type instead of `main0_out` or `void`.
    pub ret: Option<&'a str>,
    /// The stage input.
    pub stage_in: StageIn<'a>,
    /// Parameters after the standard ones.
    pub extra: &'a [Param],
    /// Statements before the output is declared.
    pub pro: &'a [String],
    /// The device layout of the shared memories of a compute kernel that is over the threadgroup limit.
    pub spill: Option<&'a Spill>,
}

impl<'a> EntryName<'a> {
    /// An entry function with the standard signature and tail.
    pub fn new(name: &'a str) -> EntryName<'a> {
        EntryName {
            name,
            form: Form::Entry,
            ret: None,
            stage_in: StageIn::Standard,
            extra: &[],
            pro: &[],
            spill: None,
        }
    }
}

/// The kind keyword of a shader.
fn kind(s: Shader) -> &'static str {
    match s {
        Shader::Vertex => "vertex",
        Shader::Fragment => "fragment",
        Shader::Compute => "kernel",
    }
}

/// The element type of a memory and its length, `None` for a scalar.
fn memory(m: &MemoryDef) -> R<(String, Option<i32>)> {
    let len = m.ty.has(AggType::ARRAY).then(|| m.array_length.max(1));
    Ok((msl_type(scalar(m.ty))?, len))
}

/// The declaration of a memory variable and the parameter that passes it on, without the address space.
fn memory_decl(ty: &str, name: &str, len: Option<i32>) -> (String, String) {
    match len {
        Some(n) => (format!("{ty} {name}[{n}]"), format!("{ty} (&{name})[{n}]")),
        None => (format!("{ty} {name}"), format!("{ty}& {name}")),
    }
}

/// Whether the fragment entries test depth early: the shader forces it, or its texture pool marks only and the shader
/// has nothing a test before it would get wrong (`pool::early`).
pub fn early_tests(s: &Structured, res: &args::Res) -> bool {
    s.props.definitions.early_z_force || res.early
}

/// The parameters every function of the program takes besides its own. A spill makes the shared memories device arrays.
pub(super) fn shared_params(
    s: &Structured,
    res: &args::Res,
    plan: &io::Plan,
    shader: Shader,
    spill: Option<&Spill>,
) -> R<Shared> {
    let mut p = Shared {
        spill: spill.is_some(),
        ..Shared::default()
    };
    let d = |a: &str, b: &str| (a.to_string(), b.to_string());
    if res.has_args {
        p.params
            .push(d(&format!("constant {SET_TYPE}& {SET_NAME}"), SET_NAME));
    }
    if !plan.in_members.is_empty() {
        p.params.push(d("thread main0_in& in", "in"));
    }
    if !plan.out_members.is_empty() && shader != Shader::Compute {
        p.params.push(d("thread main0_out& out", "out"));
    }
    if shader == Shader::Fragment && facts_ps::helper_discards(&s.info) {
        p.params.push(d(&format!("thread bool& {KILL}"), KILL));
    }
    for b in &plan.builtins {
        p.params.push(d(&format!("{} {}", b.ty, b.name), b.name));
    }
    for m in &s.props.properties.local_memories {
        let (t, n) = memory(m)?;
        let name = format!("lm{}", m.id);
        p.params
            .push(d(&format!("thread {}", memory_decl(&t, &name, n).1), &name));
    }
    for m in &s.props.properties.shared_memories {
        let (t, n) = memory(m)?;
        let name = format!("sm{}", m.id);
        let decl = if spill.is_some() {
            Spill::param(m)?
        } else {
            format!("threadgroup {}", memory_decl(&t, &name, n).1)
        };
        p.params.push(d(&decl, &name));
    }
    Ok(p)
}

/// The parameters of a function: the standard ones, with the stage input of the entry, then its own.
pub(super) fn all_params(e: &EntryName<'_>, res: &args::Res, plan: &io::Plan) -> Vec<Param> {
    let mut ps = params(res, plan);
    match e.stage_in {
        StageIn::Standard => {}
        StageIn::None => ps.retain(|p| p.attr != "stage_in"),
        StageIn::Custom(c) => {
            ps.retain(|p| p.attr != "stage_in");
            ps.insert(0, c.clone());
        }
    }
    ps.extend(e.extra.iter().cloned());
    ps
}

/// The text of one function around a body.
pub(super) fn entry_text(
    s: &Structured,
    e: &EntryName<'_>,
    shader: Shader,
    res: &args::Res,
    plan: &io::Plan,
    body: &str,
) -> R<String> {
    let early = shader == Shader::Fragment && early_tests(s, res);
    let has_out = shader != Shader::Compute && !plan.out_members.is_empty();
    let ret = e.ret.unwrap_or(if has_out { "main0_out" } else { "void" });
    let ps = all_params(e, res, plan);
    let builtins: Vec<io::Builtin> = plan.builtins.iter().copied().collect();
    let mut t = String::new();
    let body_fn = matches!(e.form, Form::Body { .. });
    match e.form {
        Form::Entry => {
            let attr = if early {
                "[[early_fragment_tests]] "
            } else {
                ""
            };
            let mut full: Vec<String> = ps.iter().map(Param::full).collect();
            if let Some(sp) = e.spill {
                full.extend(sp.entry_params(&builtins));
            }
            let _ = writeln!(
                t,
                "{attr}{} {ret} {}({})\n{{",
                kind(shader),
                e.name,
                full.join(", ")
            );
        }
        Form::Body { inline } => {
            let mut decls: Vec<String> = ps.iter().map(|p| p.decl.clone()).collect();
            for m in &s.props.properties.shared_memories {
                let (ty, n) = memory(m)?;
                decls.push(match e.spill {
                    Some(_) => Spill::param(m)?,
                    None => format!(
                        "threadgroup {}",
                        memory_decl(&ty, &format!("sm{}", m.id), n).1
                    ),
                });
            }
            let inline = if inline {
                " inline __attribute__((always_inline))"
            } else {
                ""
            };
            let _ = writeln!(
                t,
                "static{inline} {ret} dm_body_{}({})\n{{",
                e.name,
                decls.join(", ")
            );
        }
    }
    for l in e.pro {
        let _ = writeln!(t, "    {l}");
    }
    if has_out {
        t.push_str("    main0_out out = {};\n");
    }
    if shader == Shader::Vertex {
        for m in &plan.vec4_outputs {
            let _ = writeln!(t, "    out.{m} = float4(0.0, 0.0, 0.0, 1.0);");
        }
        for p in &plan.pads {
            let _ = writeln!(t, "    {}", p.init());
        }
    }
    for m in &s.props.properties.local_memories {
        let (ty, n) = memory(m)?;
        let init = if m.zero_init { " = {}" } else { "" };
        let _ = writeln!(
            t,
            "    {}{init};",
            memory_decl(&ty, &format!("lm{}", m.id), n).0
        );
    }
    if let (false, Some(sp)) = (body_fn, e.spill) {
        t.push_str(&sp.decls(&s.props.properties.shared_memories, &builtins)?);
    } else if !body_fn {
        for m in &s.props.properties.shared_memories {
            let (ty, n) = memory(m)?;
            let _ = writeln!(
                t,
                "    threadgroup {};",
                memory_decl(&ty, &format!("sm{}", m.id), n).0
            );
        }
    }
    t.push_str(body);
    let last = body.trim_end().lines().last().map(str::trim);
    if last.is_some_and(|l| l.starts_with("return")) {
        t.push_str("}\n");
        return Ok(t);
    }
    if shader == Shader::Vertex {
        for l in plan.vertex_epilogue(true) {
            let _ = writeln!(t, "    {l}");
        }
    }
    if has_out {
        t.push_str("    return out;\n");
    }
    t.push_str("}\n");
    Ok(t)
}

/// The text of an entry function that calls the body function `dm_body_<body.name>` and then runs `tail`.
///
/// The body function is the one `entry_text` writes for `body` with `Form::Body`. The entry has the parameters of
/// `e`, passes those of `body` on and keeps the result as `out`, so `tail` can change it before the return.
/// The statements of `e.pro` come before the call.
pub(super) fn call_text(
    e: &EntryName<'_>,
    body: &EntryName<'_>,
    shader: Shader,
    res: &args::Res,
    plan: &io::Plan,
    tail: &[String],
) -> R<String> {
    let ret = e.ret.unwrap_or("main0_out");
    let full: Vec<String> = all_params(e, res, plan).iter().map(Param::full).collect();
    let (_, call) = params_decl(&all_params(body, res, plan));
    let mut t = format!(
        "{} {ret} {}({})\n{{\n",
        kind(shader),
        e.name,
        full.join(", ")
    );
    for l in e.pro {
        let _ = writeln!(t, "    {l}");
    }
    let _ = writeln!(t, "    main0_out out = dm_body_{}({call});", body.name);
    for l in tail {
        let _ = writeln!(t, "    {l}");
    }
    t.push_str("}\n");
    Ok(t)
}
