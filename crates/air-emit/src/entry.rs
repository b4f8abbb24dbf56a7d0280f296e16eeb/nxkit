//! Entry points: the AIR calling convention (flattened stage I/O, buffers, built-ins) and the
//! argument metadata of `!air.vertex`, `!air.fragment` and `!air.kernel`.

use msl_parse::ast::{Attr, Stage};
use msl_sema::hir::Func;
use msl_sema::types::{Access, FieldDef, Scalar, Space, Type};
use msl_sema::Error;

use crate::func::{Addr, FnGen, RetKind, V};
use crate::ir::{llty, space_num, space_suffix};
use crate::meta::{i, s, Md};
use crate::ModCx;

type R<T> = Result<T, Error>;

fn attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Attr> {
    attrs.iter().find(|a| a.name == name)
}

fn attr_arg(attrs: &[Attr], name: &str) -> Option<String> {
    attr(attrs, name).and_then(|a| a.args.first().cloned())
}

fn attr_u32(attrs: &[Attr], name: &str) -> R<u32> {
    attr_arg(attrs, name)
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| Error::invalid(format!("attribute {name} without index")))
}

/// Metadata of the struct layout (`air.struct_type_info`). In an argument buffer every field
/// carries an `air.indirect_argument` (resources by kind, data as `air.indirect_constant`), ids
/// without `[[id(n)]]` follow the previous id.
pub fn struct_info(cx: &mut ModCx, sid: usize) -> R<usize> {
    let fields: Vec<FieldDef> = cx.types.structs[sid].fields.clone();
    let indirect = cx.types.is_argument_buffer(sid);
    let mut ops = Vec::new();
    let mut next_id: u32 = 0;
    for (fi, f) in fields.iter().enumerate() {
        let (elem, count) = match &f.ty {
            Type::Array(e, n) => ((**e).clone(), *n as i64),
            t => (t.clone(), 0),
        };
        let nested_ab = matches!(elem, Type::Struct(inner) if cx.types.is_argument_buffer(inner));
        if let Type::Struct(inner) = elem {
            if indirect && !nested_ab {
                return Err(Error::unsupported("argbuf:data-struct-member"));
            }
            let n = struct_info(cx, inner)?;
            ops.push(s("air.struct_type_info"));
            ops.push(Md::Node(n));
        }
        let (size, count, tname) = if f.metal_array {
            // `metal::array<T, n>` is one member of n elements, not a C array
            (cx.types.layout(&f.ty).size, 0, cx.types.field_msl_name(f))
        } else {
            let tname = match &elem {
                Type::Ptr(_, p) => cx.types.msl_name(p),
                t => cx.types.msl_name(t),
            };
            (cx.types.layout(&elem).size, count, tname)
        };
        ops.extend([i(f.offset), i(size), i(count), s(&tname), s(&f.name)]);
        if !indirect {
            continue;
        }
        let id = match attr_arg(&f.attrs, "id") {
            Some(a) => a.parse().map_err(|_| Error::invalid("id index"))?,
            None => next_id,
        };
        next_id = id + cx.types.id_count(&f.ty);
        ops.push(s("air.indirect_argument"));
        if nested_ab {
            ops.push(i(id));
            continue;
        }
        let arg = if f.metal_array {
            metal_array_md(cx, &elem, fi as i64, id, &f.ty, &f.name)?
        } else if matches!(elem, Type::Ptr(..) | Type::Texture(_) | Type::Sampler) {
            if count != 0 {
                return Err(Error::unsupported("argbuf:array-of-resources"));
            }
            resource_md(cx, &elem, fi as i64, id, 1, &f.name, f.is_const)?
        } else {
            let tn = cx.types.msl_name(&elem);
            cx.meta.node(&[
                i(fi as i64),
                s("air.indirect_constant"),
                s("air.location_index"),
                i(id),
                i(1),
                s("air.arg_type_name"),
                s(&tn),
                s("air.arg_name"),
                s(&f.name),
            ])
        };
        ops.push(Md::Node(arg));
    }
    Ok(cx.meta.node(&ops))
}

/// Metadata of a `metal::array<T, n>` argument-buffer member of textures or samplers: one
/// binding of n consecutive ids named by the array type.
fn metal_array_md(
    cx: &mut ModCx,
    elem: &Type,
    lead: i64,
    slot: u32,
    t: &Type,
    name: &str,
) -> R<usize> {
    let Type::Array(_, n) = t else {
        return Err(Error::invalid("metal array member without array type"));
    };
    let tn = format!("array<{}, {n}>", cx.types.msl_name(elem));
    let mut ops = vec![i(lead)];
    match elem {
        Type::Texture(tx) => {
            ops.extend([s("air.texture"), s("air.location_index"), i(slot), i(*n)]);
            ops.push(s(access_name(tx.access)));
        }
        Type::Sampler => {
            ops.extend([s("air.sampler"), s("air.location_index"), i(slot), i(*n)]);
        }
        _ => return Err(Error::unsupported(format!("metal-array:{elem}"))),
    }
    ops.extend([s("air.arg_type_name"), s(&tn), s("air.arg_name"), s(name)]);
    Ok(cx.meta.node(&ops))
}

fn access_name(a: Access) -> &'static str {
    match a {
        Access::Sample => "air.sample",
        Access::Read => "air.read",
        Access::Write => "air.write",
        Access::ReadWrite => "air.read_write",
    }
}

/// Metadata of a resource (argument-buffer member or entry argument) without the leading index.
fn resource_md(
    cx: &mut ModCx,
    t: &Type,
    lead: i64,
    slot: u32,
    count: u32,
    name: &str,
    is_const: bool,
) -> R<usize> {
    let mut ops = vec![i(lead)];
    match t {
        Type::Texture(tx) => {
            ops.extend([s("air.texture"), s("air.location_index"), i(slot), i(count)]);
            ops.push(s(access_name(tx.access)));
            let tn = cx.types.msl_name(t);
            ops.extend([s("air.arg_type_name"), s(&tn), s("air.arg_name"), s(name)]);
        }
        Type::Sampler => {
            ops.extend([s("air.sampler"), s("air.location_index"), i(slot), i(count)]);
            ops.extend([
                s("air.arg_type_name"),
                s("sampler"),
                s("air.arg_name"),
                s(name),
            ]);
        }
        Type::Ptr(sp, pointee) => {
            buffer_ops(
                cx,
                &mut ops,
                Buf {
                    sp: *sp,
                    pointee,
                    slot,
                    buffer_size: None,
                    // a pointer to an argument buffer (a descriptor pool) is itself indirect
                    indirect: matches!(**pointee, Type::Struct(sid) if cx.types.is_argument_buffer(sid)),
                    is_const,
                },
            )?;
            ops.extend([s("air.arg_name"), s(name)]);
        }
        _ => return Err(Error::unsupported(format!("resource:{t}"))),
    }
    Ok(cx.meta.node(&ops))
}

/// A buffer binding as the metadata describes it.
struct Buf<'a> {
    sp: Space,
    pointee: &'a Type,
    slot: u32,
    /// `air.buffer_size` (references only).
    buffer_size: Option<u32>,
    /// An argument buffer (`air.indirect_buffer`).
    indirect: bool,
    /// Declared `const`: read-only.
    is_const: bool,
}

/// `air.buffer ... air.arg_type_name` operands of a buffer.
fn buffer_ops(cx: &mut ModCx, ops: &mut Vec<Md>, b: Buf<'_>) -> R<()> {
    let Buf {
        sp,
        pointee,
        slot,
        buffer_size,
        indirect,
        is_const,
    } = b;
    ops.push(s(if indirect {
        "air.indirect_buffer"
    } else {
        "air.buffer"
    }));
    if let Some(n) = buffer_size {
        ops.extend([s("air.buffer_size"), i(n)]);
    }
    ops.extend([s("air.location_index"), i(slot), i(1)]);
    ops.push(s(if sp == Space::Constant || is_const {
        "air.read"
    } else {
        "air.read_write"
    }));
    ops.extend([s("air.address_space"), i(space_num(sp))]);
    if let Type::Struct(sid) = pointee {
        let n = struct_info(cx, *sid)?;
        ops.extend([s("air.struct_type_info"), Md::Node(n)]);
    }
    let l = cx.types.layout(pointee);
    let tn = cx.types.msl_name(pointee);
    ops.extend([
        s("air.arg_type_size"),
        i(l.size),
        s("air.arg_type_align_size"),
        i(l.align),
        s("air.arg_type_name"),
        s(&tn),
    ]);
    Ok(())
}

/// Function-constant predicate prefix for `[[function_constant(name)]]`.
fn fc_prefix(cx: &mut ModCx, attrs: &[Attr]) -> R<Vec<Md>> {
    let Some(name) = attr_arg(attrs, "function_constant") else {
        return Ok(Vec::new());
    };
    let fc = cx
        .fconst_by_name(&name)
        .ok_or_else(|| Error::invalid(format!("unknown function constant {name}")))?;
    let pred = cx.new_pred(fc);
    let node = cx.meta.node(&[
        Md::Raw(format!("i8 addrspace(2)* {pred}")),
        s("bool"),
        s(&name),
    ]);
    Ok(vec![s("air.function_constant"), Md::Node(node)])
}

/// One flattened input: IR type, its metadata (without the leading index), where to store it.
struct In {
    ty: String,
    attrs: String,
    md: Vec<Md>,
    /// (param index, field index or None)
    dest: (usize, Option<u32>),
}

fn interp(attrs: &[Attr]) -> Vec<Md> {
    if attr(attrs, "flat").is_some() {
        return vec![s("air.flat")];
    }
    let (loc, persp) = if attr(attrs, "center_no_perspective").is_some() {
        ("air.center", "air.no_perspective")
    } else if attr(attrs, "centroid_perspective").is_some() {
        ("air.centroid", "air.perspective")
    } else if attr(attrs, "centroid_no_perspective").is_some() {
        ("air.centroid", "air.no_perspective")
    } else if attr(attrs, "sample_perspective").is_some() {
        ("air.sample", "air.perspective")
    } else if attr(attrs, "sample_no_perspective").is_some() {
        ("air.sample", "air.no_perspective")
    } else {
        ("air.center", "air.perspective")
    };
    vec![s(loc), s(persp)]
}

const BUILTINS: &[(&str, &str)] = &[
    ("vertex_id", "air.vertex_id"),
    ("instance_id", "air.instance_id"),
    ("base_instance", "air.base_instance"),
    ("base_vertex", "air.base_vertex"),
    ("front_facing", "air.front_facing"),
    ("thread_position_in_grid", "air.thread_position_in_grid"),
    (
        "threadgroup_position_in_grid",
        "air.threadgroup_position_in_grid",
    ),
    (
        "thread_position_in_threadgroup",
        "air.thread_position_in_threadgroup",
    ),
    (
        "thread_index_in_threadgroup",
        "air.thread_index_in_threadgroup",
    ),
    ("point_coord", "air.point_coord"),
    ("sample_id", "air.sample_id"),
    ("thread_index_in_simdgroup", "air.thread_index_in_simdgroup"),
];

/// Emits an entry point, returns its definition text.
pub fn emit_entry(cx: &mut ModCx, f: &Func) -> R<String> {
    let stage = f.entry.ok_or_else(|| Error::invalid("not an entry"))?;
    // outputs
    let mut out_md: Vec<usize> = Vec::new();
    let mut out_fields: Vec<Type> = Vec::new();
    let ret_ty = match &f.ret {
        Type::Void => "void".to_string(),
        Type::Struct(sid) => {
            let fields = cx.types.structs[*sid].fields.clone();
            let mut tys = Vec::new();
            for fd in &fields {
                let fa = &fd.attrs;
                let mut md = fc_prefix(cx, fa)?;
                match stage {
                    Stage::Fragment => {
                        if let Some(c) = attr_arg(fa, "color") {
                            let c: u32 = c.parse().map_err(|_| Error::invalid("color index"))?;
                            md.extend([s("air.render_target"), i(c), i(0)]);
                        } else if let Some(d) = attr_arg(fa, "depth") {
                            md.extend([
                                s("air.depth"),
                                s("air.depth_qualifier"),
                                s(&format!("air.{d}")),
                            ]);
                        } else if attr(fa, "sample_mask").is_some() {
                            md.push(s("air.sample_mask"));
                        } else {
                            return Err(Error::unsupported("fragment-output:field"));
                        }
                    }
                    Stage::Vertex => {
                        if attr(fa, "position").is_some() {
                            md.push(s("air.position"));
                            if attr(fa, "invariant").is_some() {
                                md.push(s("air.invariant"));
                            }
                        } else if let Some(u) = attr_arg(fa, "user") {
                            md.extend([s("air.vertex_output"), s(&format!("user({u})"))]);
                        } else if attr(fa, "point_size").is_some() {
                            md.push(s("air.point_size"));
                        } else if attr(fa, "render_target_array_index").is_some() {
                            md.push(s("air.render_target_array_index"));
                        } else {
                            let names: Vec<&str> = fa.iter().map(|x| x.name.as_str()).collect();
                            return Err(Error::unsupported(format!(
                                "vertex-output:{}",
                                names.join("+")
                            )));
                        }
                    }
                    Stage::Kernel => return Err(Error::invalid("kernel returns a value")),
                }
                let ftn = cx.types.msl_name(&fd.ty);
                md.extend([
                    s("air.arg_type_name"),
                    s(&ftn),
                    s("air.arg_name"),
                    s(&fd.name),
                ]);
                out_md.push(cx.meta.node(&md));
                tys.push(llty(&cx.types, &fd.ty.unpacked(), false));
                out_fields.push(fd.ty.clone());
            }
            format!("<{{ {} }}>", tys.join(", "))
        }
        t @ (Type::Vector(..) | Type::Scalar(_)) if stage == Stage::Fragment => {
            let tn = cx.types.msl_name(t);
            out_md.push(cx.meta.node(&[
                s("air.render_target"),
                i(0),
                i(0),
                s("air.arg_type_name"),
                s(&tn),
            ]));
            llty(&cx.types, t, false)
        }
        t => return Err(Error::unsupported(format!("entry-return:{t}"))),
    };

    let mut ins: Vec<In> = Vec::new();
    for (pi, p) in f.params.iter().enumerate() {
        let a = &p.attrs;
        let tn = cx.types.msl_name(&p.ty);
        if attr(a, "stage_in").is_some() {
            let Type::Struct(sid) = p.ty else {
                return Err(Error::invalid("stage_in not a struct"));
            };
            let fields = cx.types.structs[sid].fields.clone();
            for (fi, fd) in fields.iter().enumerate() {
                let fa = &fd.attrs;
                let mut md = fc_prefix(cx, fa)?;
                match stage {
                    Stage::Vertex => {
                        let loc = attr_u32(fa, "attribute")?;
                        md.extend([s("air.vertex_input"), s("air.location_index"), i(loc), i(1)]);
                    }
                    Stage::Fragment => {
                        if attr(fa, "position").is_some() {
                            md.extend([
                                s("air.position"),
                                s("air.center"),
                                s("air.no_perspective"),
                            ]);
                        } else if let Some(u) = attr_arg(fa, "user") {
                            md.extend([s("air.fragment_input"), s(&format!("user({u})"))]);
                            md.extend(interp(fa));
                        } else if attr(fa, "front_facing").is_some() {
                            md.push(s("air.front_facing"));
                        } else if attr(fa, "sample_id").is_some() {
                            if fd.ty != Type::Scalar(Scalar::Uint) {
                                return Err(Error::invalid("sample_id field is not uint"));
                            }
                            md.push(s("air.sample_id"));
                        } else {
                            return Err(Error::unsupported("stage_in:fragment-field"));
                        }
                    }
                    Stage::Kernel => return Err(Error::unsupported("stage_in:kernel")),
                }
                let ftn = cx.types.msl_name(&fd.ty);
                md.extend([
                    s("air.arg_type_name"),
                    s(&ftn),
                    s("air.arg_name"),
                    s(&fd.name),
                ]);
                let ty = llty(&cx.types, &fd.ty.unpacked(), false);
                ins.push(In {
                    ty,
                    attrs: String::new(),
                    md,
                    dest: (pi, Some(fi as u32)),
                });
            }
            continue;
        }
        let mut md = fc_prefix(cx, a)?;
        if let Some(slot) = attr_arg(a, "buffer") {
            let slot: u32 = slot.parse().map_err(|_| Error::invalid("buffer index"))?;
            let (sp, pointee, size) = match (&p.by_ref, &p.ty) {
                (Some(sp), t) => (*sp, t.clone(), Some(cx.types.layout(t).size)),
                (None, Type::Ptr(sp, t)) => (*sp, (**t).clone(), None),
                _ => return Err(Error::unsupported("buffer:by-value")),
            };
            if !matches!(sp, Space::Constant | Space::Device) {
                return Err(Error::unsupported(format!("buffer:space:{}", sp.name())));
            }
            let indirect = matches!(pointee, Type::Struct(sid) if cx.types.is_argument_buffer(sid));
            buffer_ops(
                cx,
                &mut md,
                Buf {
                    sp,
                    pointee: &pointee,
                    slot,
                    buffer_size: size,
                    indirect,
                    is_const: p.is_const,
                },
            )?;
            md.extend([s("air.arg_name"), s(&p.name)]);
            let pt = format!(
                "{}{}*",
                llty(&cx.types, &pointee, true),
                space_suffix(space_num(sp))
            );
            let l = cx.types.layout(&pointee);
            let at = match size {
                Some(sz) => format!(
                    " noundef align {} dereferenceable({sz}) \"air-buffer-no-alias\"",
                    l.align
                ),
                None => " noundef \"air-buffer-no-alias\"".to_string(),
            };
            ins.push(In {
                ty: pt,
                attrs: at,
                md,
                dest: (pi, None),
            });
            continue;
        }
        if p.by_ref.is_some() {
            return Err(Error::unsupported("entry-param:reference-without-buffer"));
        }
        if let Some(c) = attr_arg(a, "color") {
            if stage != Stage::Fragment {
                return Err(Error::invalid("color input outside fragment"));
            }
            let c: u32 = c.parse().map_err(|_| Error::invalid("color index"))?;
            md.extend([s("air.render_target"), i(c)]);
        } else if attr(a, "position").is_some() && stage == Stage::Fragment {
            md.extend([s("air.position"), s("air.center"), s("air.no_perspective")]);
        } else if let Some((_, air)) = BUILTINS.iter().find(|(n, _)| attr(a, n).is_some()) {
            md.push(s(air));
        } else {
            let names: Vec<&str> = a.iter().map(|x| x.name.as_str()).collect();
            return Err(Error::unsupported(format!(
                "entry-param:{}",
                names.join("+")
            )));
        }
        md.extend([
            s("air.arg_type_name"),
            s(&tn),
            s("air.arg_name"),
            s(&p.name),
        ]);
        let ty = llty(&cx.types, &p.ty, false);
        ins.push(In {
            ty,
            attrs: " noundef".into(),
            md,
            dest: (pi, None),
        });
    }

    // signature and metadata
    let arg_tys: Vec<String> = ins.iter().map(|x| x.ty.clone()).collect();
    let fn_ptr_ty = format!("{ret_ty} ({})*", arg_tys.join(", "));
    let in_nodes: Vec<Md> = ins
        .iter()
        .enumerate()
        .map(|(k, x)| {
            let mut ops = vec![i(k as i64)];
            ops.extend(x.md.iter().cloned());
            Md::Node(cx.meta.node(&ops))
        })
        .collect();
    let outs = cx
        .meta
        .node(&out_md.iter().map(|n| Md::Node(*n)).collect::<Vec<_>>());
    let insn = cx.meta.node(&in_nodes);
    let mut ep = vec![
        Md::Raw(format!("{fn_ptr_ty} @{}", f.name)),
        Md::Node(outs),
        Md::Node(insn),
    ];
    if f.attrs.iter().any(|a| a.name == "early_fragment_tests") {
        ep.push(s("early_fragment_tests"));
    }
    let ep_node = cx.meta.node(&ep);
    match stage {
        Stage::Vertex => cx.vertex.push(ep_node),
        Stage::Fragment => cx.fragment.push(ep_node),
        Stage::Kernel => cx.kernel.push(ep_node),
    }

    // body
    let params_text: Vec<String> = ins
        .iter()
        .enumerate()
        .map(|(k, x)| format!("{}{} %a{k}", x.ty, x.attrs))
        .collect();
    let group = if cx.metal41 && stage == Stage::Fragment && !f.contract {
        crate::NO_LATE_CONTRACT_GROUP
    } else {
        0
    };
    let header = format!(
        "define {ret_ty} @{}({}) #{group} {{\n",
        f.name,
        params_text.join(", ")
    );
    let mut g = FnGen::new(cx, f);
    for (k, x) in ins.iter().enumerate() {
        let (pi, field) = x.dest;
        let p = &f.params[pi];
        let local = g.locals[p.local].clone();
        let lty = f.locals[p.local].ty.clone();
        match field {
            Some(fi) => {
                let st = llty(&g.cx.types, &lty, true);
                let r = g.tmp();
                g.emit(format!(
                    "{r} = getelementptr inbounds {st}, {st}* {local}, i32 0, i32 {fi}"
                ));
                let Type::Struct(sid) = lty else {
                    return Err(Error::invalid("stage_in"));
                };
                let fty = g.cx.types.structs[sid].fields[fi as usize].ty.clone();
                let v = V {
                    r: format!("%a{k}"),
                    ty: fty.unpacked(),
                };
                g.store_to(
                    &Addr {
                        ptr: r,
                        ty: fty,
                        space: 0,
                    },
                    &v,
                );
            }
            None => {
                let v = V {
                    r: format!("%a{k}"),
                    ty: lty.unpacked(),
                };
                g.store_to(
                    &Addr {
                        ptr: local,
                        ty: lty,
                        space: 0,
                    },
                    &v,
                );
            }
        }
    }
    let rt = ret_ty.clone();
    let fields = out_fields.clone();
    let types = g.cx.types.clone();
    g.set_ret(RetKind::Entry(Box::new(
        move |g: &mut FnGen<'_>, v: Option<V>| {
            match v {
                None => g.term("ret void".into()),
                Some(v) => {
                    if fields.is_empty() {
                        let t = llty(&types, &v.ty, false);
                        g.term(format!("ret {t} {}", v.r));
                        return Ok(());
                    }
                    let st = llty(&types, &v.ty, true);
                    let mut cur = "undef".to_string();
                    for (k, ft) in fields.iter().enumerate() {
                        let e = g.tmp();
                        g.emit(format!("{e} = extractvalue {st} {}, {k}", v.r));
                        let val = g.from_mem(e, ft);
                        let n = g.tmp();
                        g.emit(format!(
                            "{n} = insertvalue {rt} {cur}, {} {}, {k}",
                            llty(&types, &ft.unpacked(), false),
                            val.r
                        ));
                        cur = n;
                    }
                    g.term(format!("ret {rt} {cur}"));
                }
            }
            Ok(())
        },
    )));
    g.stmts(&f.body)?;
    g.body_done()?;
    let body = g.finish();
    Ok(format!("{header}{body}}}\n"))
}
