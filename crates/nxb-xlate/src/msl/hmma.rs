//! The Ampere HMMA.16816 helper with an f16 accumulator as `simdgroup_matrix` code, as an emitter choice.
//!
//! The tensor pass of the front end makes `dmsh_hmma16816_f16(lane, block, A0..A3, B0, B1, C0, C1, flags, A's LDSM
//! address, A0..A3's offsets, A's row stride, out D1) -> D0` and gives it a body that computes D lane by lane from the
//! subgroup's 256 word block of a scratch shared memory. The emitter writes another body over the same block: A
//! row-major (halves 0..255, 16 a row), B transposed (256..383, row n of 16 halves of k, loaded transposed) and C and D
//! row-major (384..511, 8 a row). On an M4 Pro one HMMA takes 1.08 ns a SIMD-group, against 5.4 ns for the shuffle
//! emulation.
//!
//! The tensor pass's flags (constants at each call) let a call skip the block where a matrix is already in the
//! SIMD-group's registers. Each function that calls the helper holds 8 accumulator slots (`dm_acc`, two matrices each)
//! and A's four matrices (`dm_am`) and passes them by reference, so the inlined helper keeps them in registers. A
//! chain's accumulator stays in its slot from one call to the next, a call with the previous call's A loads none, C = 0
//! is a multiply, and A read by an LDSM is loaded from shared memory where the LDSM read it. That last one needs the
//! one shared memory besides the scratch. With none or several, A goes through the block. C and D are in the call's
//! own matrices, and a slot is read only for a chain's C and written only for its D. A call outside every chain has no
//! slot bits and must not touch slot 0 (dotted rows in Ocarina of Time).
//!
//! Every HMMA this helper stands for is `HMMA.16816.F16` (an F32 destination takes the shuffle emulation instead), and
//! the hardware rounds D to half after each one, also between the steps of a chain. A slot therefore holds D already
//! rounded: before it is written, the lane's two elements of each matrix go through `half2` in the registers (no
//! scratch round trip, no barrier). The two K = 8 products are summed in FP32 first, as the hardware sums 16 products
//! before it rounds.
//!
//! A and B are half matrices. C, D and the slots are float matrices: each product uses half A and B with an FP32
//! accumulator, and D is rounded to half once, when the lanes read it from the B and C words of the block. Ampere also
//! sums in FP32. Apple's half product rounds after each step and overflows to infinity on a partial sum, which gave
//! speckles in Ocarina of Time on M1 and M2. On an M2 Max the FP32 accumulator costs about 1 %.

use super::emit::{Em, R};
use super::types::{msl_type, scalar};
use crate::ir::StorageKind;
use crate::structured::ast::{A, Ast, Kind};
use crate::structured::types::AggType;

/// The name the front end gives the helper.
pub const HELPER: &str = "dmsh_hmma16816_f16";

/// The id of the scratch shared memory the body of function `main` reads.
fn scratch(ast: &Ast, main: A) -> Option<i32> {
    let mut found = None;
    visit(ast, main, &mut found);
    found
}

fn visit(ast: &Ast, a: A, found: &mut Option<i32>) {
    match ast.kind(a) {
        Kind::Block(_) => {
            for c in ast.children(a) {
                visit(ast, c, found);
            }
        }
        Kind::Assign { src, .. } => visit(ast, *src, found),
        Kind::Operation(o) => {
            if found.is_none() && o.storage_kind == StorageKind::SharedMemory {
                *found = o
                    .sources
                    .first()
                    .and_then(|s| super::usage::const_of(ast, *s));
            }
            for s in &o.sources {
                visit(ast, *s, found);
            }
        }
        _ => {}
    }
}

/// An unsigned word from an argument of any 32 bit type.
fn word(n: usize, t: AggType) -> String {
    if scalar(t) == AggType::FP32 {
        format!("as_type<uint>(a{n})")
    } else {
        format!("uint(a{n})")
    }
}

/// The parameters the helper takes after its own arguments, and the arguments each call passes for them.
pub(super) const PARAMS: &str =
    "thread simdgroup_float8x8 (&acc)[16], thread simdgroup_half8x8 (&am)[4]";
/// The arguments of a call for `PARAMS`.
pub(super) const ARGS: &str = "dm_acc, dm_am";
/// The declaration in each function that calls the helper.
pub(super) const DECL: &str = "simdgroup_float8x8 dm_acc[16]; simdgroup_half8x8 dm_am[4];";

/// Whether function `i` is the HMMA helper.
pub(super) fn is_helper(em: &Em<'_>, i: usize) -> bool {
    i != 0 && em.info.functions.get(i).is_some_and(|f| f.name == HELPER)
}

/// The shared memory A can be loaded from directly: the one besides the scratch, in threadgroup memory.
fn direct(em: &Em<'_>, scratch: i32) -> Option<i32> {
    if em.shared.spill {
        return None;
    }
    let mut other = em
        .props
        .properties
        .shared_memories
        .iter()
        .filter(|m| m.id != scratch);
    match (other.next(), other.next()) {
        (Some(m), None) => Some(m.id),
        _ => None,
    }
}

/// The body of function `i` when it is the HMMA helper.
pub(super) fn body(em: &Em<'_>, i: usize) -> R<Option<String>> {
    let f = &em.info.functions[i];
    if !is_helper(em, i) {
        return Ok(None);
    }
    if f.in_arguments.len() != 17 || f.out_arguments.len() != 1 {
        return Err(format!("unported: {HELPER} with unexpected parameters"));
    }
    let id = scratch(&em.info.ast, f.main_block).ok_or("HMMA helper without scratch memory")?;
    let space = if em.shared.spill {
        "device"
    } else {
        "threadgroup"
    };
    let w = |n: usize| word(n, f.in_arguments[n]);
    let d1 = msl_type(scalar(f.out_arguments[0]))?;
    let rt = msl_type(scalar(f.return_type))?;
    let flags = if em.shared.spill {
        "mem_flags::mem_device | mem_flags::mem_threadgroup"
    } else {
        "mem_flags::mem_threadgroup"
    };
    let (l, blk) = (w(0), w(1));
    let (a0, a1, a2, a3) = (w(2), w(3), w(4), w(5));
    let (b0, b1, c0, c1) = (w(6), w(7), w(8), w(9));
    let fl = w(10);
    let direct = direct(em, id);
    let mut t = String::new();
    let mut put = |s: String| {
        t.push_str("    ");
        t.push_str(&s);
        t.push('\n');
    };
    put(format!("{space} uint* s = &sm{id}[{blk}];"));
    put(format!("{space} half* h = ({space} half*)s;"));
    put(format!("const int f = int({fl}), k = (f >> 8) * 2;"));
    let adirect = if direct.is_some() {
        "(f & 16) != 0"
    } else {
        "false"
    };
    put(format!(
        "const bool areuse = f & 1, cslot = f & 2, dslot = f & 4, czero = f & 8, adirect = {adirect};"
    ));
    put(format!("uint g = {l} >> 2, t = {l} & 3;"));
    put(format!(
        "if (!areuse && !adirect) {{ s[g * 8 + t] = {a0}; s[(g + 8) * 8 + t] = {a1}; s[g * 8 + 4 + t] = {a2}; s[(g + 8) * 8 + 4 + t] = {a3}; }}"
    ));
    put(format!(
        "s[128 + g * 8 + t] = {b0}; s[128 + g * 8 + 4 + t] = {b1};"
    ));
    put(format!(
        "if (!cslot && !czero) {{ s[192 + g * 4 + t] = {c0}; s[192 + (g + 8) * 4 + t] = {c1}; }}"
    ));
    put(format!("simdgroup_barrier({flags});"));
    if let Some(sm) = direct {
        let (aaddr, astr) = (w(11), w(16));
        let (o0, o1, o2, o3) = (w(12), w(13), w(14), w(15));
        put("if (!areuse && adirect) {".into());
        put(format!(
            "    threadgroup half* x = (threadgroup half*)&sm{sm}[0] + (int(simd_broadcast({aaddr}, ushort(0))) >> 1);"
        ));
        put(format!(
            "    const ulong st = ulong({astr} >> 1); const bool tr = f & 32;"
        ));
        put(format!(
            "    simdgroup_load(am[0], x + (int({o0}) >> 1), st, ulong2(0, 0), tr); simdgroup_load(am[1], x + (int({o2}) >> 1), st, ulong2(0, 0), tr);"
        ));
        put(format!(
            "    simdgroup_load(am[2], x + (int({o1}) >> 1), st, ulong2(0, 0), tr); simdgroup_load(am[3], x + (int({o3}) >> 1), st, ulong2(0, 0), tr);"
        ));
        put("} else if (!areuse) {".into());
    } else {
        put("if (!areuse) {".into());
    }
    put("    simdgroup_load(am[0], h, 16, ulong2(0, 0)); simdgroup_load(am[1], h, 16, ulong2(8, 0)); simdgroup_load(am[2], h, 16, ulong2(0, 8)); simdgroup_load(am[3], h, 16, ulong2(8, 8));".into());
    put("}".into());
    put("simdgroup_half8x8 b0, b1;".into());
    put("simdgroup_float8x8 c0, c1;".into());
    put("simdgroup_load(b0, h + 256, 16, ulong2(0, 0), true); simdgroup_load(b1, h + 256, 16, ulong2(8, 0), true);".into());
    put(
        "if (czero) { simdgroup_multiply(c0, am[0], b0); simdgroup_multiply(c1, am[2], b0); }"
            .into(),
    );
    put("else if (cslot) { simdgroup_multiply_accumulate(c0, am[0], b0, acc[k]); simdgroup_multiply_accumulate(c1, am[2], b0, acc[k + 1]); }".into());
    put("else {".into());
    put("    simdgroup_half8x8 ch0, ch1;".into());
    put("    simdgroup_load(ch0, h + 384, 8, ulong2(0, 0)); simdgroup_load(ch1, h + 384, 8, ulong2(0, 8));".into());
    put("    simdgroup_multiply_accumulate(c0, am[0], b0, ch0); simdgroup_multiply_accumulate(c1, am[2], b0, ch1);".into());
    put("}".into());
    put("simdgroup_multiply_accumulate(c0, am[1], b1, c0); simdgroup_multiply_accumulate(c1, am[3], b1, c1);".into());
    put(format!(
        "if (dslot) {{ c0.thread_elements()[0] = float(half(c0.thread_elements()[0])); c0.thread_elements()[1] = float(half(c0.thread_elements()[1])); c1.thread_elements()[0] = float(half(c1.thread_elements()[0])); c1.thread_elements()[1] = float(half(c1.thread_elements()[1])); acc[k] = c0; acc[k + 1] = c1; simdgroup_barrier({flags}); a17 = 0; return 0; }}"
    ));
    put(format!("{space} float* w = ({space} float*)(s + 128);"));
    put("simdgroup_store(c0, w, 8, ulong2(0, 0)); simdgroup_store(c1, w, 8, ulong2(0, 8));".into());
    put(format!("simdgroup_barrier({flags});"));
    put("uint d0 = as_type<uint>(half2(w[g * 8 + t * 2], w[g * 8 + t * 2 + 1]));".into());
    put(
        "uint d1 = as_type<uint>(half2(w[(g + 8) * 8 + t * 2], w[(g + 8) * 8 + t * 2 + 1]));"
            .into(),
    );
    put(format!("simdgroup_barrier({flags});"));
    put(format!("a17 = as_type<{d1}>(d1);"));
    put(format!("return as_type<{rt}>(d0);"));
    Ok(Some(t))
}
