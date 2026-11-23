//! metal_stdlib builtins and texture methods as AIR intrinsic calls. Every name and signature
//! here was read from Apple's front end output for the same MSL (`tools/probe/probe.py`).

use msl_sema::hir::{Expr, ExprKind, PlaceKind, Query, SampleOpt, TexOp, TextureAtomicFetch};
use msl_sema::types::{is_float, is_signed, Access, Scalar, Tex, TexKind, Type};
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
    if name == "simdgroup_barrier" {
        const THREADGROUP_MEMORY: u32 = 2;
        const SEQ_CST_ORDER: u32 = 5;
        const SIMDGROUP_SCOPE: u32 = 4;
        let operand = |value: u32| ("i32".into(), value.to_string());
        let mut ops = vec![operand(THREADGROUP_MEMORY)];
        match g.cx.air_version {
            (2, 8, 0) => {}
            (2, 9, 0) => ops.push(operand(SEQ_CST_ORDER)),
            _ => return Err(Error::unsupported("simdgroup-barrier:target")),
        }
        ops.push(operand(SIMDGROUP_SCOPE));
        let r = call(
            g,
            "air.simdgroup.barrier",
            &ops,
            "void",
            Attrs::Convergent,
            false,
        );
        return Ok(V { r, ty: ret.clone() });
    }
    if name == "simd_is_helper_thread" {
        // Apple Metal 32023.921, macOS 26 (AIR 2.8): convergent i1 query,
        // not readnone. The result can change across a fragment discard.
        if g.cx.air_version != (2, 8, 0) {
            return Err(Error::unsupported("simd-helper-thread:target"));
        }
        let r = call(
            g,
            "air.simd_is_helper_thread",
            &[],
            "i1",
            Attrs::Convergent,
            false,
        );
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
        "reverse_bits" if *ret == Type::INT => {
            let [x] = args.as_slice() else {
                return Err(unsupported());
            };
            // Swap adjacent groups of 1, 2, 4, 8 and 16 bits. Each mask
            // selects the low group in each pair.
            const SWAPS: [(u32, u32); 5] = [
                (1, 0x5555_5555),
                (2, 0x3333_3333),
                (4, 0x0f0f_0f0f),
                (8, 0x00ff_00ff),
                (16, 0x0000_ffff),
            ];
            let mut v = x.r.clone();
            for (shift, mask) in SWAPS {
                let hi = g.tmp();
                g.emit(format!("{hi} = lshr i32 {v}, {shift}"));
                let lo = g.tmp();
                g.emit(format!("{lo} = and i32 {v}, {mask}"));
                let lower = g.tmp();
                g.emit(format!("{lower} = and i32 {hi}, {mask}"));
                let upper = g.tmp();
                g.emit(format!("{upper} = shl i32 {lo}, {shift}"));
                v = g.tmp();
                g.emit(format!("{v} = or i32 {lower}, {upper}"));
            }
            Ok(V {
                r: v,
                ty: ret.clone(),
            })
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
                SampleOpt::Grad2d(dx, dy)
                | SampleOpt::Grad3d(dx, dy)
                | SampleOpt::GradCube(dx, dy) => {
                    if g.cx.air_version != (2, 8, 0) {
                        match opt {
                            SampleOpt::Grad2d(..) if kind == TexKind::T2d => {
                                return Err(Error::unsupported("gradient2d:target"));
                            }
                            SampleOpt::Grad3d(..) => {
                                return Err(Error::unsupported("gradient3d:target"));
                            }
                            SampleOpt::GradCube(..) => {
                                return Err(Error::unsupported("gradientcube:target"));
                            }
                            _ => {}
                        }
                    }
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
                let (name, vt) = if cmp.is_some() {
                    if g.cx.air_version != (2, 8, 0) {
                        return Err(Error::unsupported("gather_compare:target"));
                    }
                    (format!("air.gather_compare_{an}.f32"), "<4 x float>".into())
                } else {
                    if kind != TexKind::T2d && kind != TexKind::T2dArray {
                        return Err(Error::unsupported(format!("gather:{}", kind.name())));
                    }
                    let (sfx, vt) = tex_ret(tx, false)?;
                    (format!("air.gather_{an}.{sfx}"), vt)
                };
                if !matches!(kind, TexKind::DepthCube | TexKind::DepthCubeArray) {
                    a.push(("i1".into(), "true".into()));
                    a.push(split_typed(&off));
                }
                if cmp.is_none() {
                    a.push(("i32".into(), comp.to_string()));
                }
                a.push(("i32".into(), "0".into()));
                let st = format!("{{ {vt}, i8 }}");
                // The AIR 2.8 array compare gather omits the read-only memory effect,
                // while its individual texture and sampler operands remain readonly.
                let attrs = if kind == TexKind::Depth2dArray && cmp.is_some() {
                    Attrs::Write
                } else {
                    Attrs::ReadOnly
                };
                let r = call(g, &name, &a, &st, attrs, false);
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
                if !matches!(opt, SampleOpt::GradCube(..)) {
                    a.push(("i1".into(), "true".into()));
                    a.push(split_typed(&off));
                }
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
            let buffer = kind == TexKind::Buffer;
            let multisampled = kind == TexKind::T2dMs;
            match kind {
                TexKind::T2d | TexKind::T2dArray => {}
                TexKind::T2dMs
                    if g.cx.air_version == (2, 8, 0)
                        && g.cx.read_sampler
                        && tx.elem == Scalar::Float
                        && tx.access == Access::Read => {}
                TexKind::Buffer
                    if g.cx.air_version == (2, 8, 0)
                        && g.cx.read_sampler
                        && matches!(tx.elem, Scalar::Float | Scalar::Uint)
                        && matches!(tx.access, Access::Read | Access::ReadWrite)
                        && array.is_none()
                        && lod.is_none() => {}
                TexKind::T3d
                    if g.cx.air_version == (2, 8, 0)
                        && g.cx.read_sampler
                        && tx.elem == Scalar::Float
                        && tx.access == Access::Sample => {}
                _ => return Err(Error::unsupported(format!("read:{}", kind.name()))),
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
            if g.cx.read_sampler && !multisampled && !buffer {
                a.push(split_typed(&zero_offset(kind.coord_dim())));
            }
            if !buffer {
                a.push(("i32".into(), l));
            }
            // Match the captured AIR 2.8 modes for implicit-mip 2D storage reads.
            let implicit_mip_air28 =
                g.cx.air_version == (2, 8, 0) && g.cx.read_sampler && lod.is_none();
            let plane_image = kind == TexKind::T2d && implicit_mip_air28;
            let array_image = kind == TexKind::T2dArray && implicit_mip_air28;
            let selector = match (tx.access, tx.elem) {
                (Access::ReadWrite, _) if buffer => 3,
                (Access::ReadWrite, Scalar::Float | Scalar::Int | Scalar::Uint) if plane_image => 3,
                (Access::Read, Scalar::Float | Scalar::Int | Scalar::Uint) if plane_image => 1,
                (Access::Read, Scalar::Float) if array_image => 1,
                _ => u32::from(multisampled || buffer),
            };
            a.push(("i32".into(), selector.to_string()));
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
            let buffer = kind == TexKind::Buffer;
            if buffer
                && (g.cx.air_version != (2, 8, 0)
                    || !g.cx.read_sampler
                    || tx.elem != Scalar::Float
                    || tx.access != Access::Read
                    || !matches!(what, Query::Width)
                    || lod.is_some())
            {
                return Err(Error::unsupported("texture_buffer:query"));
            }
            let multisampled = matches!(kind, TexKind::T2dMs | TexKind::T2dMsArray);
            let ms_array_len = kind == TexKind::T2dMsArray && matches!(what, Query::ArraySize);
            if multisampled
                && (g.cx.air_version != (2, 8, 0)
                    || lod.is_some()
                    || !(ms_array_len
                        || matches!(what, Query::Width | Query::Height | Query::Samples)))
            {
                return Err(Error::unsupported(format!("{}:query", kind.name())));
            }
            let q = match what {
                Query::Width => "width",
                Query::Height => "height",
                Query::Depth => "depth",
                Query::ArraySize => "array_size",
                Query::MipLevels => "num_mip_levels",
                Query::Samples => "num_samples",
            };
            let t = tex_value(g, tex)?;
            let mut a = vec![(format!("{ht} nocapture readonly"), t.r)];
            if !multisampled
                && !buffer
                && matches!(what, Query::Width | Query::Height | Query::Depth)
            {
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
            let smp = match kind {
                TexKind::T2d | TexKind::T2dArray => g.expr(sampler)?,
                TexKind::T3d | TexKind::Cube | TexKind::CubeArray
                    if g.cx.air_version == (2, 8, 0)
                        && tx.elem == Scalar::Float
                        && tx.access == Access::Sample =>
                {
                    g.load_untagged(sampler)?
                }
                _ => return Err(Error::unsupported(format!("lod:{}", kind.name()))),
            };
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
        TexOp::AtomicFetch { op, coord, value } => {
            let op = match op {
                TextureAtomicFetch::Or => "or",
                TextureAtomicFetch::Add => "add",
                TextureAtomicFetch::Max => "max",
                TextureAtomicFetch::Min => "min",
            };
            if g.cx.air_version != (2, 8, 0) {
                return Err(Error::unsupported(format!(
                    "texture2d:atomic-fetch-{op}-target"
                )));
            }
            let c = g.expr(coord)?;
            let v = g.expr(value)?;
            let t = tex_value(g, tex)?;
            let (sfx, vt) = tex_ret(tx, false)?;
            // Apple Metal 32023.921, AIR 2.8: offset, relaxed order, read-write access.
            let a = [
                (format!("{ht} nocapture"), t.r),
                (lt(g, &c.ty), c.r),
                split_typed(&zero_offset(2)),
                (vt.clone(), v.r),
                ("i32".into(), "0".into()),
                ("i32".into(), "3".into()),
            ];
            let r = call(
                g,
                &format!("air.atomic_fetch_{op}_explicit_{an}.{sfx}"),
                &a,
                &vt,
                Attrs::NoUnwindWillReturn,
                false,
            );
            Ok(V { r, ty: ret.clone() })
        }
        TexOp::Write {
            value,
            coord,
            array,
            lod,
        } => {
            let buffer = kind == TexKind::Buffer;
            let buffer_write = buffer
                && g.cx.air_version == (2, 8, 0)
                && g.cx.read_sampler
                && matches!(
                    (tx.elem, tx.access),
                    (Scalar::Float, Access::ReadWrite)
                        | (Scalar::Uint, Access::Write | Access::ReadWrite)
                )
                && lod.is_none();
            let array_write = kind == TexKind::T2dArray
                && g.cx.air_version == (2, 8, 0)
                && tx.elem == Scalar::Float
                && tx.access == Access::Write
                && array.is_some()
                && lod.is_none();
            if !array_write && ((kind != TexKind::T2d && !buffer_write) || array.is_some()) {
                return Err(Error::unsupported(format!("write:{}", kind.name())));
            }
            let (c, v) = if array_write && simple_store_arg(coord) && simple_store_arg(value) {
                // Apple's AIR 2.8 builds the payload first for these side-effect-free arguments.
                let v = g.expr(value)?;
                (g.expr(coord)?, v)
            } else {
                (g.expr(coord)?, g.expr(value)?)
            };
            let slice = array.as_ref().map(|a| g.expr(a)).transpose()?;
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
            let mut a = vec![(format!("{ht} nocapture"), t.r), (lt(g, &c.ty), c.r)];
            // Apple Metal 32023.921, AIR 2.8: a 2D array store puts its slice before the texel.
            if let Some(slice) = slice {
                a.push(("i32".into(), slice.r));
            }
            a.push((vt, v.r));
            if !buffer {
                a.push(("i32".into(), l));
            }
            a.push(("i32".into(), access.into()));
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

// Limit reordered construction to values that cannot observe or cause side effects.
fn simple_store_arg(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Zero => true,
        ExprKind::Load(place) => matches!(place.kind, PlaceKind::Local(_)),
        ExprKind::Convert(x) | ExprKind::Splat(x) => simple_store_arg(x),
        ExprKind::Vector(xs) => xs.iter().all(simple_store_arg),
        _ => false,
    }
}

fn split_typed(s: &str) -> (String, String) {
    // "<2 x i32> zeroinitializer" or "<2 x i32> %v3"
    match s.rfind(' ') {
        Some(k) => (s[..k].to_string(), s[k + 1..].to_string()),
        None => (String::new(), s.to_string()),
    }
}
