//! Task and mesh shaders as Metal object and mesh functions (`dmsh_meshstage.py`).
//!
//! The front end reads a task or mesh shader as a compute shader with its stage memory in shared memory. `entry` writes
//! that kernel as the function `dm_body_main0`. The wrapper of this file owns the stage memory. It fills the stage
//! inputs, calls the body and hands the stage outputs to Metal.
//!
//! The stage memory has these parts, all zero when the body starts:
//! - word 0 is the number of the workgroup. A task shader or a mesh shader without a task gets the draw's first
//!   workgroup plus its threadgroup.
//! - INPUT is the payload of the task shader. Its word 0 is the number of the mesh workgroup among the launches of the
//!   task.
//! - The outputs are the payload of a task shader (word 1 is the number of mesh workgroups to launch, the records start
//!   at +0x10) or the attributes of a mesh shader. The attributes are all the blocks of 32 vertices, then all the blocks
//!   of 32 primitives. A block has a word per vertex or primitive for each component. A vertex block has the components
//!   of the SPH (the point size first, then the position and the generic attributes). A primitive block has the
//!   components of the per-primitive SPH.
//! - The primitive bytes of a mesh shader hold the count at +3 and the index bytes from +4. A primitive has 1, 2 or 3
//!   index bytes, as the control section says.

use super::StageInputs;
use super::abi::MESH_FIRST_SLOT;
use super::emit::R;
use super::entry::Param;
use super::refl::Reflection;
use crate::job::{Job, MeshStage, POINT_SIZE};
use nxb_pyjson::Value;
use std::fmt::Write;

/// The name of the function that holds the translated body: `entry::entry_text` writes `dm_body_<name>` for the form
/// `Form::Body` of `main0`.
pub const BODY_NAME: &str = "dm_body_main0";

const INPUT: i64 = 0x10;
/// Vertices and primitives of an attribute block.
const BLOCK: u32 = 32;
const MAX_VERTICES: u32 = 256;
const MAX_PRIMITIVES: u32 = 512;
/// Mesh threadgroups an object threadgroup may launch.
const MAX_GRID: u32 = 1024;
const MAX_SHARED: i64 = 0x8000;
/// The Ampere opcodes of LDS, STS and ATOMS (`DecoderAmpere.cs`).
const SHARED_OPCODES: [u16; 6] = [0x984, 0x388, 0x988, 0x38c, 0x98c, 0xf8c];

fn align(v: i64, to: i64) -> i64 {
    (v + to - 1) & !(to - 1)
}

/// The vertices of a primitive: 1 point, 2 line, 3 triangle.
fn verts(m: &MeshStage) -> u32 {
    if m.verts == 0 { 3 } else { m.verts }
}

/// The offsets of the stage memory (the number of the workgroup, the input, the outputs, the primitive bytes) and its
/// size in bytes.
pub fn memory(m: &MeshStage) -> ([i32; 4], i64) {
    let payload = i64::from(m.payload);
    let out = INPUT + align(payload, 16);
    if m.kind == "ts" {
        return (
            [0, INPUT as i32, out as i32, (out + payload) as i32],
            out + align(payload, 4),
        );
    }
    let v_blocks = i64::from(m.max_v.max(1).div_ceil(BLOCK));
    let p_blocks = i64::from(m.max_p.max(1).div_ceil(BLOCK));
    let comps = (v_blocks * m.vertex.len() as i64 + p_blocks * m.primitive.len() as i64).max(1);
    let prim = out + comps * i64::from(BLOCK) * 4;
    // The guest writes the index bytes of primitive min(count - 1, lane + 32k) in each primitive block. A count of 0
    // wraps count - 1, so every lane of every block writes. The bytes are reserved for whole blocks of 32 primitives.
    let size = prim + align(4 + i64::from(verts(m)) * p_blocks * i64::from(BLOCK), 4);
    ([0, INPUT as i32, out as i32, prim as i32], size)
}

/// Whether the instructions use shared memory of their own. It would lie over the stage memory, and the control
/// section does not give its size.
pub fn own_shared(code: &[u8]) -> bool {
    code.as_chunks::<16>().0.iter().any(|i| {
        let op = u16::from_le_bytes([i[0], i[1]]) & 0xfff;
        SHARED_OPCODES.contains(&op)
    })
}

/// Why a task or mesh job cannot be translated, as the `mesh stage failed` detail. The job has to carry the stage
/// memory and the sizes the facts of its `mesh_stage` give.
pub fn check(job: &Job, code: &[u8]) -> Result<(), String> {
    let fail = |d: &str| Err(format!("mesh stage failed: {d}"));
    let Some(m) = &job.mesh_stage else {
        return Ok(());
    };
    if !matches!(m.kind.as_str(), "ts" | "ms") {
        return fail(&format!("mesh_stage kind {}", m.kind));
    }
    if let Some(r) = &m.refused {
        return fail(r);
    }
    if m.threads == 0 || m.threads > 1024 {
        return fail(&format!("{} threads in a workgroup", m.threads));
    }
    if m.kind == "ms" && !(1..=3).contains(&verts(m)) {
        return fail(&format!(
            "primitive vertex count {} is not 1, 2 or 3",
            m.verts
        ));
    }
    let (lay, size) = memory(m);
    if own_shared(code) || size > MAX_SHARED {
        return fail("shared memory of its own, or stage memory past Metal's limit");
    }
    if job.stage_memory.as_deref() != Some(&lay[..]) || job.shared != size {
        return fail("the stage memory of the job does not match its mesh_stage");
    }
    if m.kind == "ms" {
        if m.max_v > MAX_VERTICES || m.max_p > MAX_PRIMITIVES {
            return fail(&format!(
                "{} vertices and {} primitives exceed a Metal mesh",
                m.max_v, m.max_p
            ));
        }
        if !m.vertex.iter().any(|(a, _)| a.is_none()) {
            return fail("its SPH enables no position component");
        }
    }
    Ok(())
}

/// The reflection fields of a task or mesh library.
pub fn reflect(m: &MeshStage, r: &mut Reflection) {
    if m.kind == "ms" {
        r.entries
            .push(("entry_z".to_string(), Value::str("main0_z")));
    }
    // The entry is an object or mesh function, which has none of the kinds of a vertex, fragment or kernel function
    r.set_extra("msl_entry_kind", Value::Null);
    r.set_extra(
        "mesh_stage",
        Value::obj([
            ("kind", Value::str(m.kind.clone())),
            ("threads", Value::Int(i64::from(m.threads))),
            ("payload", Value::Int(i64::from(m.payload))),
            ("task", Value::Bool(m.task)),
        ]),
    );
}

/// The locations the paired pixel shaders read that neither the vertex nor the primitive attributes write.
fn padded(vm: &Members, pm: &Members, partners: &[StageInputs]) -> Vec<u32> {
    let written = |l: u32| {
        let l = i32::try_from(l).ok();
        vm.iter().chain(pm).any(|(n, _)| *n == l)
    };
    let mut out: Vec<u32> = partners
        .iter()
        .flat_map(|p| p.varyings.iter().map(|v| v.location))
        .filter(|l| !written(*l))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The attributes of the components: the index of each component in the attribute area, `None` for a component the
/// shader does not write.
type Members = Vec<(Option<i32>, [Option<usize>; 4])>;

fn members(comps: &[(Option<i32>, i32)], shift: usize) -> Members {
    let mut out: Members = Vec::new();
    for (n, _) in comps {
        if out.iter().any(|(m, _)| m == n) {
            continue;
        }
        let mut idx = [None; 4];
        for (k, slot) in idx.iter_mut().enumerate() {
            *slot = comps
                .iter()
                .position(|c| *c == (*n, k as i32))
                .map(|p| p + shift);
        }
        out.push((*n, idx));
    }
    out
}

/// A vector of `width` components from the words of the attribute area at `b`. A component the shader does not write is
/// 0, and 1 for w.
fn vector(idx: &[Option<usize>; 4], mem: &str, width: usize) -> String {
    let c: Vec<String> = idx
        .iter()
        .enumerate()
        .take(width)
        .map(|(k, i)| match i {
            Some(i) => format!("as_type<float>({mem}[b + {}u])", *i as u32 * BLOCK),
            None if k == 3 => "1.0".into(),
            None => "0.0".into(),
        })
        .collect();
    if width == 1 {
        c[0].clone()
    } else {
        format!("float{width}({})", c.join(", "))
    }
}

fn float_type(width: usize) -> String {
    if width == 1 {
        "float".into()
    } else {
        format!("float{width}")
    }
}

/// The components an output needs: the ones the shader writes, and a w that the pixel shaders read and the shader does
/// not write, because it must hold 1. `read` is the mask of the components they read, `None` when it is not known.
/// A Metal fragment input wider than the output it links to reads 0 in the missing components.
fn width(idx: &[Option<usize>; 4], read: Option<u8>) -> usize {
    let written = idx.iter().rposition(Option::is_some).map_or(0, |k| k + 1);
    let w = idx[3].is_none() && read.unwrap_or(0xf) & 8 != 0;
    written.max(if w { 4 } else { 0 }).max(1)
}

/// The components of a location that the paired pixel shaders read, `None` without partners.
fn read_mask(partners: &[StageInputs], location: i32) -> Option<u8> {
    if partners.is_empty() {
        return None;
    }
    let l = u32::try_from(location).ok()?;
    Some(
        partners
            .iter()
            .flat_map(|p| &p.varyings)
            .filter(|v| v.location == l)
            .fold(0, |m, v| m | v.read),
    )
}

fn topology(verts: u32) -> &'static str {
    match verts {
        1 => "point",
        2 => "line",
        _ => "triangle",
    }
}

/// The wrapper functions around the body: the object function `main0` of a task shader, or the mesh functions `main0`
/// and `main0_z` (the two depth conventions of a vertex shader) of a mesh shader. `shared` is the number of shared
/// memories of the body and `words` the words of the stage memory. A location that a paired pixel shader reads and the
/// mesh shader does not write becomes a vertex output of (0, 0, 0, 1), as for a vertex shader.
pub fn wrap(
    m: &MeshStage,
    params: &[Param],
    shared: usize,
    words: i64,
    partners: &[StageInputs],
) -> R<String> {
    if shared > 1 {
        return Err(
            "mesh stage failed: the body has shared memory besides the stage memory".into(),
        );
    }
    if m.threads == 0 {
        return Err("mesh stage failed: no threads in a workgroup".into());
    }
    let own = |attr: &str| {
        params.iter().find(|p| p.attr == attr).map(|p| {
            if p.decl.starts_with("uint3 ") {
                format!("{}.x", p.name)
            } else {
                p.name.clone()
            }
        })
    };
    let (lid, tg) = (
        own("thread_position_in_threadgroup"),
        own("threadgroup_position_in_grid"),
    );
    let mut sig: Vec<String> = params.iter().map(Param::full).collect();
    if lid.is_none() {
        sig.push("uint dm_lid [[thread_position_in_threadgroup]]".into());
    }
    if tg.is_none() {
        sig.push("uint dm_tg [[threadgroup_position_in_grid]]".into());
    }
    let lid = lid.unwrap_or_else(|| "dm_lid".into());
    let tg = tg.unwrap_or_else(|| "dm_tg".into());
    let mut args: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
    if shared == 1 {
        args.push("dm_mem".into());
    }
    let call = format!("{BODY_NAME}({});", args.join(", "));
    let (lay, _) = memory(m);
    let thr = m.threads;
    let pay = (m.payload + 3) >> 2;
    let (wg, inp, out, prim) = (lay[0] >> 2, lay[1] >> 2, lay[2] >> 2, lay[3]);
    let start = format!(
        "    threadgroup uint dm_mem[{words}];\n\
         \x20   for (uint i = {lid}; i < {words}u; i += {thr}u) dm_mem[i] = 0u;\n\
         \x20   threadgroup_barrier(mem_flags::mem_threadgroup);\n"
    );
    let first = format!("constant uint* dm_first [[buffer({MESH_FIRST_SLOT})]]");
    let mut t = format!("struct dm_payload_t {{ uint w[{}]; }};\n", pay.max(1));
    let sig = sig.join(", ");
    let sync = "    threadgroup_barrier(mem_flags::mem_threadgroup);\n";
    if m.kind == "ts" {
        let _ = write!(
            t,
            "[[object]] void main0({sig}, object_data dm_payload_t& dm_payload [[payload]], mesh_grid_properties dm_grid, {first})\n{{\n{start}\
             \x20   if ({lid} == 0u) dm_mem[{wg}] = dm_first[0] + {tg};\n{sync}    {call}\n{sync}\
             \x20   for (uint i = {lid}; i < {pay}u; i += {thr}u) dm_payload.w[i] = dm_mem[{out}u + i];\n\
             \x20   if ({lid} == 0u) dm_grid.set_threadgroups_per_grid(uint3(min(dm_mem[{}], {MAX_GRID}u), 1u, 1u));\n}}\n",
            out + 1
        );
        return Ok(t);
    }
    let vm = members(&m.vertex, 0);
    let comps: Vec<(Option<i32>, i32)> = m.primitive.iter().map(|&(n, c)| (Some(n), c)).collect();
    let pm = members(&comps, 0);
    let (max_v, max_p) = (m.max_v, m.max_p);
    let nv = verts(m);
    let v_blocks = max_v.max(1).div_ceil(BLOCK);
    let v_stride = m.vertex.len().max(1) as u32 * BLOCK;
    let p_stride = m.primitive.len().max(1) as u32 * BLOCK;
    // The primitive blocks follow all the vertex blocks.
    let p_out = out + (v_blocks * m.vertex.len() as u32 * BLOCK) as i32;
    let pads = padded(&vm, &pm, partners);
    let has_psiz = vm.iter().any(|(n, _)| *n == Some(POINT_SIZE));
    t.push_str("struct dm_vertex_t {\n    float4 position [[position]];\n");
    if has_psiz {
        t.push_str("    float point_size [[point_size]];\n");
    }
    for (n, idx) in &vm {
        if let Some(n) = n.filter(|n| *n >= 0) {
            let w = width(idx, read_mask(partners, n));
            let _ = writeln!(t, "    {} a{n} [[user(locn{n})]];", float_type(w));
        }
    }
    for n in &pads {
        let w = width(&[None; 4], read_mask(partners, *n as i32));
        let _ = writeln!(t, "    {} dm_pad{n} [[user(locn{n})]];", float_type(w));
    }
    t.push_str("};\n");
    if !pm.is_empty() {
        t.push_str("struct dm_prim_t {\n");
        for (n, idx) in &pm {
            if let Some(n) = n {
                let w = width(idx, read_mask(partners, *n));
                let _ = writeln!(t, "    {} p{n} [[user(locn{n})]];", float_type(w));
            }
        }
        t.push_str("};\n");
    }
    let prim_ty = if pm.is_empty() { "void" } else { "dm_prim_t" };
    let _ = writeln!(
        t,
        "using dm_mesh_t = metal::mesh<dm_vertex_t, {prim_ty}, {max_v}, {max_p}, metal::topology::{}>;",
        topology(nv)
    );
    let v_block = format!("{out}u + (n / {BLOCK}u) * {v_stride}u + n % {BLOCK}u");
    let p_block = format!("{p_out}u + (n / {BLOCK}u) * {p_stride}u + n % {BLOCK}u");
    let mut sets = String::new();
    for (n, idx) in &vm {
        match n {
            Some(POINT_SIZE) => {
                let w = idx[0].unwrap_or(0) as u32 * BLOCK;
                let _ = writeln!(
                    sets,
                    "        v.point_size = as_type<float>(dm_mem[b + {w}u]);"
                );
            }
            _ => {
                let member = n.map_or("position".to_string(), |n| format!("a{n}"));
                let w = n.map_or(4, |n| width(idx, read_mask(partners, n)));
                let _ = writeln!(sets, "        v.{member} = {};", vector(idx, "dm_mem", w));
            }
        }
    }
    for n in &pads {
        let w = width(&[None; 4], read_mask(partners, *n as i32));
        let _ = writeln!(
            sets,
            "        v.dm_pad{n} = {};",
            vector(&[None; 4], "dm_mem", w)
        );
    }
    let mut psets = String::new();
    for (n, idx) in &pm {
        let member = n.map_or(String::new(), |n| format!("p{n}"));
        let w = n.map_or(4, |n| width(idx, read_mask(partners, n)));
        let _ = writeln!(psets, "        p.{member} = {};", vector(idx, "dm_mem", w));
    }
    for (name, gl) in [("main0", true), ("main0_z", false)] {
        let fill = if m.task {
            format!(
                "    for (uint i = {lid}; i < {pay}u; i += {thr}u) dm_mem[{inp}u + i] = dm_payload.w[i];\n{sync}\
                 \x20   if ({lid} == 0u) {{ dm_mem[{wg}] = {tg}; dm_mem[{inp}] = {tg}; }}\n"
            )
        } else {
            format!("    if ({lid} == 0u) dm_mem[{wg}] = dm_first[0] + {tg};\n")
        };
        let input = if m.task {
            "const object_data dm_payload_t& dm_payload [[payload]]".to_string()
        } else {
            first.clone()
        };
        let _ = write!(
            t,
            "\n[[mesh]] void {name}({sig}, dm_mesh_t dm_out, {input})\n{{\n{start}{fill}{sync}    {call}\n{sync}\
             \x20   uint np = min(dm_mem[{}] >> 24, {max_p}u);\n\
             \x20   if ({lid} == 0u) dm_out.set_primitive_count(np);\n\
             \x20   for (uint n = {lid}; n < {max_v}u; n += {thr}u)\n    {{\n        uint b = {v_block};\n        dm_vertex_t v;\n{sets}",
            prim >> 2
        );
        if gl {
            t.push_str("        v.position.z = fma(v.position.z, 0.5, v.position.w * 0.5);\n");
        }
        t.push_str("        dm_out.set_vertex(n, v);\n    }\n");
        if !pm.is_empty() {
            let _ = write!(
                t,
                "    for (uint n = {lid}; n < np; n += {thr}u)\n    {{\n        uint b = {p_block};\n        dm_prim_t p;\n{psets}        dm_out.set_primitive(n, p);\n    }}\n"
            );
        }
        let _ = write!(
            t,
            "    for (uint n = {lid}; n < np * {nv}u; n += {thr}u)\n    {{\n        uint at = {}u + n;\n        dm_out.set_index(n, (dm_mem[at >> 2] >> ((at & 3u) * 8u)) & 0xffu);\n    }}\n}}\n",
            prim + 4
        );
    }
    Ok(t)
}
