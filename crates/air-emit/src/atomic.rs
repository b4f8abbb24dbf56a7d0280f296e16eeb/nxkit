//! Device atomics with relaxed order, as the `dm_pool_mark` helper uses them. `metal::atomic_*`
//! are library templates: clang instantiates them at the end of the unit and they lower to
//! `air.atomic.global.*` with (order relaxed = 0, scope device = 2, 0, not volatile).

use msl_sema::types::Type;
use msl_sema::Error;

use crate::func::{FnGen, V};
use crate::Attrs;

type R<T> = Result<T, Error>;

/// Order, scope and volatility operands of a relaxed device-scope atomic.
const RELAXED_DEVICE: &str = "i32 0, i32 2, i32 0, i1 false";

/// Lowers `atomic_load_explicit(w)` and `atomic_fetch_or_explicit(w, b)` on a `device uint*`,
/// or returns None for another builtin.
pub fn builtin(g: &mut FnGen<'_>, name: &str, args: &[V], ret: &Type) -> R<Option<V>> {
    let (air, sig_args, ops) = match (name, args) {
        ("atomic_fetch_add_explicit" | "atomic_fetch_min_explicit" | "atomic_fetch_max_explicit", [w, b]) => {
            let air = match name {
                "atomic_fetch_add_explicit" => "air.atomic.local.add.u.i32",
                "atomic_fetch_min_explicit" => "air.atomic.local.min.u.i32",
                _ => "air.atomic.local.max.u.i32",
            };
            (air, "i32 addrspace(3)* nocapture, i32, i32, i32, i32, i1",
             format!("i32 addrspace(3)* nocapture {}, i32 {}, i32 0, i32 1, i32 0, i1 false", w.r, b.r))
        }
        ("atomic_load_explicit", [w]) => (
            "air.atomic.global.load.i32",
            "i32 addrspace(1)* nocapture, i32, i32, i32, i1",
            format!("i32 addrspace(1)* nocapture {}, {RELAXED_DEVICE}", w.r),
        ),
        ("atomic_fetch_or_explicit", [w, b]) => (
            "air.atomic.global.or.u.i32",
            "i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1",
            format!(
                "i32 addrspace(1)* nocapture {}, i32 {}, {RELAXED_DEVICE}",
                w.r, b.r
            ),
        ),
        ("atomic_load_explicit" | "atomic_fetch_or_explicit", _) => {
            return Err(Error::invalid(format!("{name} arguments")))
        }
        _ => return Ok(None),
    };
    let attrs = Attrs::NoUnwindWillReturn;
    g.cx.in_template += 1;
    let d =
        g.cx.declare_wrapped(air, &format!("i32 @{air}({sig_args})"), attrs);
    g.cx.in_template -= 1;
    let r = g.tmp();
    g.emit(format!("{r} = call i32 @{d}({ops}) #{}", attrs.group()));
    Ok(Some(V { r, ty: ret.clone() }))
}
