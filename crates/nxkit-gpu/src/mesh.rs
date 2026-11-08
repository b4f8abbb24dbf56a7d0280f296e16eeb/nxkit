//! Task and mesh draws (`nvnCommandBufferDrawMeshTasks` and its indirect form).
//!
//! The shader step translates a task shader into a Metal object function and a mesh shader into a mesh function
//! (`nxb-xlate` `msl::mesh`). A draw runs them in a mesh pipeline with the pixel shader of the program. Metal needs a
//! fragment function in a mesh pipeline, so a program without a pixel shader gets an empty one and draws depth only.
//!
//! The functions read their argument buffer at `[[buffer(0)]]` like the other stages. The mesh stage shares the
//! argument table of the vertex stage and the object stage has its own. A task shader, or a mesh shader without one,
//! reads the first workgroup of the draw at `[[buffer(MESH_FIRST_SLOT)]]` and adds its threadgroup in the grid.
//!
//! Without a task shader the grid is the mesh grid, which Metal limits to [`MAX_GRID`] threadgroups in a dimension.
//! The translated mesh function takes the x position only, so a larger draw becomes several grids, each with its own
//! first workgroup. An indirect draw does the same on the GPU, see `indirect`.

use crate::bindless::{Sink, StageInputs};
use crate::detail::{DrawArgs, DrawDetail, Stage};
use crate::draws::{Geom, PassCtx, Prep, Shared, cull_of, fixed};
use crate::programs::MeshKind;
use crate::stats::Skip;

/// The buffer slot of the first workgroup of the draw in the object and mesh functions (`nxb-xlate`
/// `abi::MESH_FIRST_SLOT`).
pub(crate) const MESH_FIRST_SLOT: u32 = 29;
/// The most threadgroups in one dimension of a mesh grid.
pub(crate) const MAX_GRID: u32 = 1024;
/// The bytes from the first workgroup of one grid to that of the next, in the buffer of a split draw.
pub(crate) const FIRST_STRIDE: u64 = 16;
/// The most grids of a draw without a task shader: 64 Ki workgroups. A larger draw loses the rest.
pub(crate) const MAX_SPLITS: u32 = 64;
/// The grids of an indirect draw without a task shader. The CPU does not know the count, so the pre-pass writes this
/// many grids and the ones past the count are empty.
pub(crate) const INDIRECT_SPLITS: u32 = 16;

/// The storage buffer tables of the task and mesh stages in the driver constant segment c0 (`nxb-xlate`
/// `abi::C0_SSBO_TS` and `C0_SSBO_MS`): a row of address (64 bits) and size (32 bits) for each of 16 slots.
const C0_SSBO_TS: usize = 0x670;
const C0_SSBO_MS: usize = 0x770;
const C0_ROW: usize = 0x10;
const C0_SSBO_SLOTS: u32 = 16;

/// Writes the storage buffer table of a task or mesh stage into its driver block `b`. The shaders read the address of
/// a storage buffer from there. Other stages are left as they are.
pub(crate) fn storage_table(b: &mut [u8], inp: &StageInputs<'_>) {
    let base = match inp.stage {
        Stage::Task => C0_SSBO_TS,
        Stage::Mesh => C0_SSBO_MS,
        _ => return,
    };
    for s in inp
        .sbufs
        .iter()
        .filter(|s| s.stage == inp.stage && s.slot < C0_SSBO_SLOTS)
    {
        let at = base + C0_ROW * s.slot as usize;
        if let Some(row) = b.get_mut(at..at + 12) {
            row[..8].copy_from_slice(&s.address.to_le_bytes());
            row[8..].copy_from_slice(&s.size.to_le_bytes());
        }
    }
}

/// The number of grids of a direct draw of `count` workgroups.
pub(crate) fn splits(count: u32, task: bool) -> u32 {
    if task {
        1
    } else {
        count.div_ceil(MAX_GRID).min(MAX_SPLITS)
    }
}

/// Translates a task and mesh draw.
pub(crate) fn prepare(
    sh: &Shared<'_>,
    pc: &PassCtx,
    d: &DrawDetail,
    sink: &mut dyn Sink,
) -> Result<Prep, Skip> {
    let cull = cull_of(d)?;
    let key = d.program.mesh.as_deref().ok_or(Skip::Program)?;
    let ms = sh.programs.stage(key, false)?;
    let mi = ms
        .mesh
        .filter(|m| m.kind == MeshKind::Mesh)
        .ok_or(Skip::Program)?;
    let ts = match &d.program.task {
        Some(k) => Some(sh.programs.stage(k, false)?),
        None => None,
    };
    let ti = match &ts {
        Some(t) => Some(
            t.mesh
                .filter(|m| m.kind == MeshKind::Task)
                .ok_or(Skip::Program)?,
        ),
        None => None,
    };
    // A mesh shader that reads a payload needs its task shader, and one without a task stage needs the first workgroup.
    if mi.task != ts.is_some() {
        nxkit_nvn::diag::example(
            nxkit_nvn::diag::Reason::SkipProgram,
            format_args!(
                "mesh {key} reads a payload: {}, task shader {:?}",
                mi.task, d.program.task
            ),
        );
        return Err(Skip::Program);
    }
    let fs = match &d.program.fragment {
        Some(k) => Some(sh.programs.stage(k, false)?),
        None => None,
    };
    let (pipeline, pkey) = sh
        .pipelines
        .mesh(d, ts.as_deref(), &ms, fs.as_deref(), &pc.targets)?;
    let f = fixed(sh, pc, d, cull)?;
    let task = ts.is_some();
    let (object_threads, mesh_threads) = (ti.map_or(1, |t| t.threads), mi.threads);
    let geom = match d.args {
        DrawArgs::MeshTasks { first, count } => {
            let n = splits(count, task).max(1);
            let mut words = Vec::with_capacity(n as usize * FIRST_STRIDE as usize);
            for k in 0..n {
                words.extend(first.wrapping_add(k * MAX_GRID).to_le_bytes());
                words.extend([0u8; FIRST_STRIDE as usize - 4]);
            }
            Geom::Mesh {
                first: sink.put(&words)?,
                count,
                object_threads,
                mesh_threads,
                task,
            }
        }
        DrawArgs::MeshTasksIndirect { indirect } => {
            crate::draws::indirect_args(sh, indirect, 8)?;
            Geom::MeshIndirect {
                splits: if task { 1 } else { INDIRECT_SPLITS },
                object_threads,
                mesh_threads,
                task,
            }
        }
        _ => return Err(Skip::Detail),
    };
    let inputs = |stage| StageInputs {
        stage,
        ubos: &d.ubos,
        sbufs: &d.sbufs,
        textures: &d.textures,
        images: &d.images,
        base_instance: 0,
        groups: None,
    };
    let mesh_args = crate::bindless::fill_args(&ms, &inputs(Stage::Mesh), &sh.env, sink)?;
    let object_args = match &ts {
        Some(t) => Some(crate::bindless::fill_args(
            t,
            &inputs(Stage::Task),
            &sh.env,
            sink,
        )?),
        None => None,
    };
    let fragment_args = match &fs {
        Some(f) => Some(crate::bindless::fill_args_scaled(
            f,
            &inputs(Stage::Fragment),
            &sh.env,
            pc.scale,
            sink,
        )?),
        None => None,
    };
    Ok(Prep {
        pipeline,
        key: pkey,
        depth_stencil: f.depth_stencil,
        depth_stencil_desc: f.depth_stencil_desc,
        vertex_args: mesh_args,
        fragment_args,
        object_args,
        streams: Vec::new(),
        cull: f.cull,
        winding: f.winding,
        fill: f.fill,
        bias: f.bias,
        clip: f.clip,
        geom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::UboDetail;

    #[test]
    fn a_draw_without_a_task_shader_is_cut_into_grids_of_1024() {
        assert_eq!(splits(0, false), 0);
        assert_eq!(splits(1024, false), 1);
        assert_eq!(splits(1025, false), 2);
        assert_eq!(splits(5000, true), 1);
        assert_eq!(splits(u32::MAX, false), MAX_SPLITS);
    }

    #[test]
    fn the_storage_buffers_of_the_mesh_stage_go_to_its_c0_table() {
        let sbufs = [
            UboDetail {
                stage: Stage::Mesh,
                slot: 2,
                address: 0x1234_5678_9abc,
                size: 0x40,
                snapshot: None,
            },
            UboDetail {
                stage: Stage::Task,
                slot: 0,
                address: 0x99,
                size: 4,
                snapshot: None,
            },
        ];
        let inp = StageInputs {
            stage: Stage::Mesh,
            ubos: &[],
            sbufs: &sbufs,
            textures: &[],
            images: &[],
            base_instance: 0,
            groups: None,
        };
        let mut b = vec![0u8; 0xA00];
        storage_table(&mut b, &inp);
        let at = C0_SSBO_MS + 2 * C0_ROW;
        assert_eq!(&b[at..at + 8], &0x1234_5678_9abcu64.to_le_bytes());
        assert_eq!(&b[at + 8..at + 12], &0x40u32.to_le_bytes());
        assert!(b[C0_SSBO_TS..C0_SSBO_TS + 16].iter().all(|v| *v == 0));
    }
}
