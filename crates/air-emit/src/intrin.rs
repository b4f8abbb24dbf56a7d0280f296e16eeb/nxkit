//! metal_stdlib builtins and texture methods as AIR intrinsic calls. Every name and signature
//! here was read from Apple's front end output for the same MSL (`tools/probe/probe.py`).

use msl_sema::hir::{Expr, ExprKind, PlaceKind, Query, SampleOpt, TexOp};
use msl_sema::types::{is_float, is_signed, Scalar, Tex, TexKind, Type};
use msl_sema::Error;

use crate::func::{Addr, FnGen, FM, V};
use crate::ir::suffix;
use crate::Attrs;

type R<T> = Result<T, Error>;

/// Emits a call `name(args)` returning `ret`, declaring the intrinsic.
fn call(
    g: &mut FnGen<'_>,
    name: &str,
    args: &[(String, String)],
    ret: &str,
    attrs: Attrs,
    fm: bool,
) -> String {
    let tys: Vec<&str> = args.iter().map(|(t, _)| t.as_str()).collect();
    let d =
        g.cx.declare_wrapped(name, &format!("{ret} @{name}({})", tys.join(", ")), attrs);
    let ops: Vec<String> = args.iter().map(|(t, v)| format!("{t} {v}")).collect();
    let flags = if fm { format!("{FM} ") } else { String::new() };
    if ret == "void" {
        g.emit(format!(
            "call void @{d}({}) #{}",
            ops.join(", "),
            attrs.group()
        ));
        return "undef".into();
    }
    let r = g.tmp();
    g.emit(format!(
        "{r} = call {flags}{ret} @{d}({}) #{}",
        ops.join(", "),
        attrs.group()
    ));
    r
}

fn lt(g: &FnGen<'_>, t: &Type) -> String {
    crate::ir::llty(&g.cx.types, t, false)
}

fn ops(g: &FnGen<'_>, vals: &[V]) -> Vec<(String, String)> {
    vals.iter().map(|v| (lt(g, &v.ty), v.r.clone())).collect()
}

/// Lowers a builtin call.
pub fn builtin(g: &mut FnGen<'_>, name: &str, args: Vec<V>, ret: &Type) -> R<V> {
    if let Some(v) = crate::atomic::builtin(g, name, &args, ret)? {
        return Ok(v);
    }
    let s = ret.elem();
    let fl = s.is_some_and(is_float);
    let rt = lt(g, ret);
    let sfx = suffix(ret);
    let unsupported = || Error::unsupported(format!("builtin:{name}:{ret}"));
    if fl && s != Some(Scalar::Float) && !matches!(name, "isnan" | "select" | "all" | "any") {
        return Err(Error::unsupported(format!("builtin:{name}:half")));
    }
    if name == "threadgroup_barrier" {
        // Apple AIR for threadgroup_barrier(mem_flags::mem_threadgroup).
        let ops = [
            ("i32".into(), "2".into()),
            ("i32".into(), "5".into()),
            ("i32".into(), "1".into()),
        ];
        let r = call(g, "air.wg.barrier", &ops, "void", Attrs::Convergent, false);
        return Ok(V { r, ty: ret.clone() });
    }
    let simple = |n: &str| Some(n.to_string());
    let intrinsic: Option<String> = match name {
        "sqrt" | "rsqrt" | "exp2" | "log2" | "floor" | "ceil" | "trunc" | "rint" | "sin"
        | "cos" | "saturate"
            if fl =>
        {
            simple(&format!("air.fast_{name}.{sfx}"))
        }
        "abs" if fl => simple(&format!("air.fast_fabs.{sfx}")),
        "dfdx" | "dfdy" if fl => {
            let a = ops(g, &args);
            let r = call(
                g,
                &format!("air.{name}.{sfx}"),
                &a,
                &rt,
                Attrs::Convergent,
                true,
            );
            return Ok(V { r, ty: ret.clone() });
        }
        "abs" if s.is_some_and(is_signed) => simple(&format!("air.abs.s.{sfx}")),
        "min" | "max" | "fast::min" | "fast::max" if fl => simple(&format!(
            "air.fast_f{}.{sfx}",
            name.trim_start_matches("fast::")
        )),
        "precise::min" | "precise::max" if fl => simple(&format!("air.f{}.{sfx}", &name[9..])),
        "min" | "max" | "clamp" => {
            let sg = if s.is_some_and(is_signed) { "s" } else { "u" };
            if fl {
                simple(&format!("air.fast_{name}.{sfx}"))
            } else {
                simple(&format!("air.{name}.{sg}.{sfx}"))
            }
        }
        "fast::clamp" if fl => simple(&format!("air.fast_clamp.{sfx}")),
        "precise::clamp" if fl => simple(&format!("air.clamp.{sfx}")),
        "fma" if fl => simple(&format!("air.fma.{sfx}")),
        "popcount" if !fl => simple(&format!("air.popcount.{sfx}")),
        "extract_bits" | "insert_bits" if !fl => {
            let sg = if s.is_some_and(is_signed) { "s" } else { "u" };
            simple(&format!("air.{name}.{sg}.{sfx}"))
        }
        "mulhi" if !fl => {
            let sg = if s.is_some_and(is_signed) { "s" } else { "u" };
            simple(&format!("air.mul_hi.{sg}.{sfx}"))
        }
        "unpack_unorm4x8_to_float" => simple("air.unpack.unorm4x8.v4f32"),
        "unpack_snorm4x8_to_float" => simple("air.unpack.snorm4x8.v4f32"),
        "unpack_unorm2x16_to_float" => simple("air.unpack.unorm2x16.v2f32"),
        "unpack_snorm2x16_to_float" => simple("air.unpack.snorm2x16.v2f32"),
        "unpack_unorm10a2_to_float" => simple("air.unpack.unorm.rgb10a2.v4f32"),
        _ => None,
    };
    if let Some(n) = intrinsic {
        let a = ops(g, &args);
        let r = call(g, &n, &a, &rt, Attrs::ReadNone, fl);
        return Ok(V { r, ty: ret.clone() });
    }
    match name {
        "clz" if !fl => {
            let mut a = ops(g, &args);
            a.push(("i1".into(), "false".into()));
            let r = call(
                g,
                &format!("air.clz.{sfx}"),
                &a,
                &rt,
                Attrs::ReadNone,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        "ctz" if *ret == Type::UINT => {
            let [x] = args.as_slice() else {
                return Err(unsupported());
            };
            // (x - 1) & ~x is a mask of the trailing zero bits. With wrapping
            // subtraction the zero input yields an all-ones mask and returns 32.
            let prev = g.tmp();
            g.emit(format!("{prev} = sub i32 {}, 1", x.r));
            let inv = g.tmp();
            g.emit(format!("{inv} = xor i32 {}, -1", x.r));
            let mask = g.tmp();
            g.emit(format!("{mask} = and i32 {prev}, {inv}"));
            let leading = call(
                g,
                "air.clz.i32",
                &[("i32".into(), mask), ("i1".into(), "false".into())],
                "i32",
                Attrs::ReadNone,
                false,
            );
            let r = g.tmp();
            g.emit(format!("{r} = sub i32 32, {leading}"));
            Ok(V { r, ty: ret.clone() })
        }
        "simd_shuffle" => {
            let sg = match s {
                Some(Scalar::Int) => "s.",
                Some(Scalar::Uint) => "u.",
                Some(Scalar::Float) => "",
                _ => return Err(unsupported()),
            };
            let mut a = ops(g, &args);
            // Apple's front end freezes the shuffled value, not the lane
            let fr = g.tmp();
            g.emit(format!("{fr} = freeze {} {}", a[0].0, a[0].1));
            a[0].1 = fr;
            let r = call(
                g,
                &format!("air.simd_shuffle.{sg}{sfx}"),
                &a,
                &rt,
                Attrs::Convergent,
                fl,
            );
            Ok(V { r, ty: ret.clone() })
        }
        "simd_ballot" => {
            let a = ops(g, &args);
            let r = call(
                g,
                "air.simd_ballot.i64",
                &a,
                "i64",
                Attrs::Convergent,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        "simd_any" | "simd_all" => {
            if args[0].ty != Type::BOOL {
                return Err(unsupported());
            }
            let a = ops(g, &args);
            let r = call(
                g,
                &format!("air.{name}"),
                &a,
                "i1",
                Attrs::Convergent,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        "isnan" => {
            let x = &args[0];
            let Some(xs) = x.ty.elem() else {
                return Err(unsupported());
            };
            let (it, mask, inf) = match xs {
                Scalar::Float => ("i32", "2147483647", "2139095040"),
                Scalar::Half => ("i16", "32767", "31744"),
                _ => return Err(unsupported()),
            };
            let ity = match x.ty {
                Type::Vector(_, n) => format!("<{n} x {it}>"),
                _ => it.to_string(),
            };
            let splat = |v: &str| match x.ty {
                Type::Vector(_, n) => {
                    format!("<{}>", vec![format!("{it} {v}"); n as usize].join(", "))
                }
                _ => v.to_string(),
            };
            // metal::isnan is compiled without no-nans/no-infs, the inliner carries that to the caller
            g.emit("; air-emit: fp-classify".into());
            let b = g.tmp();
            g.emit(format!("{b} = bitcast {} {} to {ity}", lt(g, &x.ty), x.r));
            let m = g.tmp();
            g.emit(format!("{m} = and {ity} {b}, {}", splat(mask)));
            let r = g.tmp();
            g.emit(format!("{r} = icmp ugt {ity} {m}, {}", splat(inf)));
            Ok(V { r, ty: ret.clone() })
        }
        "all" | "any" => {
            let x = &args[0];
            if let Type::Scalar(_) = x.ty {
                return Ok(V {
                    r: x.r.clone(),
                    ty: ret.clone(),
                });
            }
            let a = ops(g, &args);
            let r = call(
                g,
                &format!("air.{name}.{}", suffix(&x.ty)),
                &a,
                "i1",
                Attrs::ReadNone,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        "select" => {
            // select(a, b, c) = c ? b : a, per component.
            let (a, b, c) = (&args[0], &args[1], &args[2]);
            let r = g.tmp();
            let fm = if fl { "nsz arcp afn " } else { "" };
            g.emit(format!(
                "{r} = select {fm}{} {}, {rt} {}, {rt} {}",
                lt(g, &c.ty),
                c.r,
                b.r,
                a.r
            ));
            Ok(V { r, ty: ret.clone() })
        }
        _ => Err(unsupported()),
    }
}

fn handle_ty(tx: Tex) -> String {
    format!("%struct._{}_t addrspace(1)*", tx.kind.air_name())
}

/// The result suffix of a texture intrinsic: `v4f32`, `u.v4i32`, `s.v4i32`, `f32` (depth).
fn tex_ret(tx: Tex, depth_scalar: bool) -> R<(String, String)> {
    if depth_scalar {
        return Ok(("f32".into(), "float".into()));
    }
    Ok(match tx.elem {
        Scalar::Float => ("v4f32".into(), "<4 x float>".into()),
        Scalar::Half => ("v4f16".into(), "<4 x half>".into()),
        Scalar::Int => ("s.v4i32".into(), "<4 x i32>".into()),
        Scalar::Uint => ("u.v4i32".into(), "<4 x i32>".into()),
        s => return Err(Error::unsupported(format!("texture-elem:{}", s.name()))),
    })
}

fn zero_offset(dim: u8) -> String {
    format!("<{dim} x i32> zeroinitializer")
}

/// The texture handle, loaded after the call's other arguments and without a TBAA tag (clang
/// loads it inside the method wrapper through `this`).
fn tex_value(g: &mut FnGen<'_>, tex: &Expr) -> R<V> {
    match g.tex_pre.last().cloned().flatten() {
        Some(a) => g.load_texture_at(&a),
        None => g.load_texture_this(tex),
    }
}

/// The address of a texture reached through a pointer index (`(&pool->e[0])[i]`). clang
/// evaluates the object expression of a method call before its arguments, which is observable
/// when the index has side effects (`dm_pool_mark`).
fn tex_pre_addr(g: &mut FnGen<'_>, tex: &Expr) -> R<Option<Addr>> {
    let ExprKind::Load(p) = &tex.kind else {
        return Ok(None);
    };
    match &p.kind {
        PlaceKind::Deref(e) if matches!(e.kind, ExprKind::PtrOffset(..)) => g.addr(p),
        _ => Ok(None),
    }
}

/// Lowers a texture operation.`slot` is its reserved place in the deferred list.
pub fn texture(
    g: &mut FnGen<'_>,
    tex: &Expr,
    tx: Tex,
    op: &TexOp,
    ret: &Type,
    _slot: usize,
) -> R<V> {
    // texture methods are members of class templates: clang instantiates them at the end
    g.cx.in_template += 1;
    let r = tex_pre_addr(g, tex).and_then(|pre| {
        g.tex_pre.push(pre);
        let depth = g.tex_pre.len();
        let r = texture_op(g, tex, tx, op, ret);
        g.tex_pre.truncate(depth - 1);
        r
    });
    g.cx.in_template -= 1;
    r
}

fn texture_op(g: &mut FnGen<'_>, tex: &Expr, tx: Tex, op: &TexOp, ret: &Type) -> R<V> {
    let ht = handle_ty(tx);
    let kind = tx.kind;
    let an = kind.air_name();
    match op {
        TexOp::Sample {
            gather,
            sampler,
            coord,
            array,
            compare,
            opt,
            offset,
        } => {
            g.sampler_copy = true;
            let smp = g.expr(sampler);
            g.sampler_copy = false;
            let smp = smp?;
            let c = g.expr(coord)?;
            let arr = match array {
                Some(a) => Some(g.expr(a)?),
                None => None,
            };
            let cmp = match compare {
                Some(x) => Some(g.expr(x)?),
                None => None,
            };
            let mut gradients = None;
            let (is_lvl, lod) = match opt {
                SampleOpt::None => ("false", "0x0000000000000000".to_string()),
                // `level(x)`/`bias(x)` are constructed through a struct constructor that clang does
                // not mark mustprogress, inlining it drops the attribute of the caller
                SampleOpt::Level(x) => {
                    g.not_mustprogress();
                    ("true", g.expr(x)?.r)
                }
                SampleOpt::Bias(x) => {
                    g.not_mustprogress();
                    ("false", g.expr(x)?.r)
                }
                SampleOpt::Grad2d(dx, dy) => {
                    gradients = Some([g.expr(dx)?, g.expr(dy)?]);
                    ("false", "0x0000000000000000".to_string())
                }
            };
            let off = match offset {
                Some(o) => {
                    let v = g.expr(o)?;
                    format!("{} {}", lt(g, &v.ty), v.r)
                }
                None => zero_offset(kind.coord_dim()),
            };
            let has_offset = !kind.is_cube();
            let t = tex_value(g, tex)?;
            let mut a: Vec<(String, String)> = vec![
                (format!("{ht} nocapture readonly"), t.r.clone()),
                (
                    "%struct._sampler_t addrspace(2)* nocapture readonly".into(),
                    smp.r,
                ),
            ];
            if cmp.is_some() {
                a.push(("i32".into(), "1".into()));
            }
            a.push((lt(g, &c.ty), c.r));
            if let Some(x) = arr {
                a.push(("i32".into(), x.r));
            }
            if let Some(x) = &cmp {
                a.push(("float".into(), x.r.clone()));
            }
            if let Some(comp) = gather {
                if kind != TexKind::T2d && kind != TexKind::T2dArray {
                    return Err(Error::unsupported(format!("gather:{}", kind.name())));
                }
                a.push(("i1".into(), "true".into()));
                let (ty, v) = split_typed(&off);
                a.push((ty, v));
                a.push(("i32".into(), comp.to_string()));
                a.push(("i32".into(), "0".into()));
                let (sfx, vt) = tex_ret(tx, false)?;
                let name = format!("air.gather_{an}.{sfx}");
                let st = format!("{{ {vt}, i8 }}");
                let r = call(g, &name, &a, &st, Attrs::ReadOnly, false);
                let x = g.tmp();
                g.emit(format!("{x} = extractvalue {st} {r}, 0"));
                return Ok(V {
                    r: x,
                    ty: ret.clone(),
                });
            }
            let depth = kind.is_depth();
            let (sfx, vt) = tex_ret(tx, depth)?;
            let st = format!("{{ {vt}, i8 }}");
            let (name, attrs) = if let Some(gradients) = gradients {
                for v in gradients {
                    a.push((lt(g, &v.ty), v.r));
                }
                a.push(("float".into(), "0x0000000000000000".into()));
                a.push(("i1".into(), "true".into()));
                a.push(split_typed(&off));
                a.push(("i32".into(), "0".into()));
                (format!("air.sample_{an}_grad.{sfx}"), Attrs::ReadOnly)
            } else {
                if has_offset {
                    a.push(("i1".into(), "true".into()));
                    a.push(split_typed(&off));
                } else if offset.is_some() {
                    return Err(Error::unsupported("sample-offset:cube"));
                }
                a.push(("i1".into(), is_lvl.into()));
                a.push(("float".into(), lod));
                a.push(("float".into(), "0x0000000000000000".into()));
                a.push(("i32".into(), "0".into()));
                let name = if cmp.is_some() {
                    format!("air.sample_compare_{an}.{sfx}")
                } else {
                    format!("air.sample_{an}.{sfx}")
                };
                (name, Attrs::SampleConvergent)
            };
            let r = call(g, &name, &a, &st, attrs, false);
            let x = g.tmp();
            g.emit(format!("{x} = extractvalue {st} {r}, 0"));
            Ok(V {
                r: x,
                ty: ret.clone(),
            })
        }
        TexOp::Read { coord, array, lod } => {
            if kind != TexKind::T2d && kind != TexKind::T2dArray && kind != TexKind::T3d {
                return Err(Error::unsupported(format!("read:{}", kind.name())));
            }
            if kind != TexKind::T2d && kind != TexKind::T2dArray {
                return Err(Error::unsupported(format!("read:{}", kind.name())));
            }
            let c = g.expr(coord)?;
            let slice = match array {
                Some(x) => Some(g.expr(x)?),
                None => None,
            };
            let l = match lod {
                Some(x) => g.expr(x)?.r,
                None => "0".into(),
            };
            let t = tex_value(g, tex)?;
            let mut a = vec![(format!("{ht} nocapture readonly"), t.r)];
            if g.cx.read_sampler {
                let smp = call(
                    g,
                    "air.get_read_sampler",
                    &[],
                    "%struct._sampler_t addrspace(2)*",
                    Attrs::Inaccessible,
                    false,
                );
                a.push(("%struct._sampler_t addrspace(2)*".into(), smp));
            }
            a.push((lt(g, &c.ty), c.r));
            if let Some(x) = slice {
                a.push(("i32".into(), x.r));
            }
            if g.cx.read_sampler {
                a.push(("<2 x i32>".into(), "zeroinitializer".into()));
            }
            a.push(("i32".into(), l));
            a.push(("i32".into(), "0".into()));
            let (sfx, vt) = tex_ret(tx, kind.is_depth())?;
            let st = format!("{{ {vt}, i8 }}");
            let r = call(
                g,
                &format!("air.read_{an}.{sfx}"),
                &a,
                &st,
                Attrs::ReadOnly,
                false,
            );
            let x = g.tmp();
            g.emit(format!("{x} = extractvalue {st} {r}, 0"));
            Ok(V {
                r: x,
                ty: ret.clone(),
            })
        }
        TexOp::Query { what, lod } => {
            let q = match what {
                Query::Width => "width",
                Query::Height => "height",
                Query::Depth => "depth",
                Query::ArraySize => "array_size",
                Query::MipLevels => "num_mip_levels",
            };
            let t = tex_value(g, tex)?;
            let mut a = vec![(format!("{ht} nocapture readonly"), t.r)];
            if matches!(what, Query::Width | Query::Height | Query::Depth) {
                let l = match lod {
                    Some(x) => g.expr(x)?.r,
                    None => "0".into(),
                };
                a.push(("i32".into(), l));
            }
            let r = call(
                g,
                &format!("air.get_{q}_{an}"),
                &a,
                "i32",
                Attrs::ReadOnly,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        TexOp::Lod {
            clamped,
            sampler,
            coord,
        } => {
            if !matches!(kind, TexKind::T2d | TexKind::T2dArray) {
                return Err(Error::unsupported(format!("lod:{}", kind.name())));
            }
            g.sampler_copy = true;
            let smp = g.expr(sampler);
            g.sampler_copy = false;
            let smp = smp?;
            let c = g.expr(coord)?;
            let t = tex_value(g, tex)?;
            let a = vec![
                (format!("{ht} nocapture readonly"), t.r),
                (
                    "%struct._sampler_t addrspace(2)* nocapture readonly".into(),
                    smp.r,
                ),
                (lt(g, &c.ty), c.r),
                ("i32".into(), "0".into()),
            ];
            let n = if *clamped { "clamped" } else { "unclamped" };
            let r = call(
                g,
                &format!("air.calculate_{n}_lod_{an}"),
                &a,
                "float",
                Attrs::SampleConvergent,
                true,
            );
            Ok(V { r, ty: ret.clone() })
        }
        TexOp::Write {
            value,
            coord,
            array,
            lod,
        } => {
            if kind != TexKind::T2d || array.is_some() {
                return Err(Error::unsupported(format!("write:{}", kind.name())));
            }
            let c = g.expr(coord)?;
            let v = g.expr(value)?;
            let l = match lod {
                Some(x) => g.expr(x)?.r,
                None => "0".into(),
            };
            let access = match tx.access {
                msl_sema::types::Access::ReadWrite => "3",
                _ => "2",
            };
            let (sfx, vt) = tex_ret(tx, false)?;
            let t = tex_value(g, tex)?;
            let a = vec![
                (format!("{ht} nocapture"), t.r),
                (lt(g, &c.ty), c.r),
                (vt, v.r),
                ("i32".into(), l),
                ("i32".into(), access.into()),
            ];
            call(
                g,
                &format!("air.write_{an}.{sfx}"),
                &a,
                "void",
                Attrs::Write,
                false,
            );
            Ok(V {
                r: "undef".into(),
                ty: ret.clone(),
            })
        }
    }
}

fn split_typed(s: &str) -> (String, String) {
    // "<2 x i32> zeroinitializer" or "<2 x i32> %v3"
    match s.rfind(' ') {
        Some(k) => (s[..k].to_string(), s[k + 1..].to_string()),
        None => (String::new(), s.to_string()),
    }
}
