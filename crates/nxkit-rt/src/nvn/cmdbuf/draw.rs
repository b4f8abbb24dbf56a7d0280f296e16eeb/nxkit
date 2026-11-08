//! The draw, dispatch and transform feedback functions of the command buffer.
//!
//! A draw is one extension record that carries its parameters. The calls are many thousand per frame, so a draw
//! reads no guest memory but its own arguments and goes straight to the writer.

use nxkit_nvn::ext::{DrawKind, ExtCode};

use super::args::{hi, lo, range, span};
use super::bind::{adjust, bases};
use super::rec::emit;
use crate::nvn::abi::{AbiResult, Obj, guard, table};

/// The most transform feedback buffers.
const TF_BUFFERS: u32 = 4;

fn draw(name: &'static str, c: u64, kind: DrawKind, words: &[u32]) {
    emit(name, c, ExtCode::Draw, kind as u16, words);
}

extern "C" fn draw_arrays(c: u64, mode: i32, first: i32, n: i32) {
    draw(
        "nvnCommandBufferDrawArrays",
        c,
        DrawKind::Arrays,
        &[mode as u32, first as u32, n as u32],
    );
}

extern "C" fn draw_arrays_instanced(
    c: u64,
    mode: i32,
    first: i32,
    n: i32,
    base_instance: i32,
    instances: i32,
) {
    draw(
        "nvnCommandBufferDrawArraysInstanced",
        c,
        DrawKind::ArraysInstanced,
        &[
            mode as u32,
            first as u32,
            n as u32,
            base_instance as u32,
            instances as u32,
        ],
    );
}

extern "C" fn draw_arrays_indirect(c: u64, mode: i32, indirect: u64) {
    draw(
        "nvnCommandBufferDrawArraysIndirect",
        c,
        DrawKind::ArraysIndirect,
        &[mode as u32, lo(indirect), hi(indirect)],
    );
}

fn elements(
    name: &'static str,
    c: u64,
    a: [i32; 3],
    index: u64,
    vertex: i32,
    (base, count): (i32, i32),
) {
    draw(
        name,
        c,
        DrawKind::Elements,
        &[
            a[0] as u32,
            a[1] as u32,
            a[2] as u32,
            lo(index),
            hi(index),
            vertex as u32,
            base as u32,
            count as u32,
        ],
    );
}

extern "C" fn draw_elements(c: u64, mode: i32, ty: i32, n: i32, index: u64) {
    elements(
        "nvnCommandBufferDrawElements",
        c,
        [mode, ty, n],
        index,
        0,
        (0, 1),
    );
}

extern "C" fn draw_elements_base_vertex(
    c: u64,
    mode: i32,
    ty: i32,
    n: i32,
    index: u64,
    vertex: i32,
) {
    elements(
        "nvnCommandBufferDrawElementsBaseVertex",
        c,
        [mode, ty, n],
        index,
        vertex,
        (0, 1),
    );
}

extern "C" fn draw_elements_instanced(
    c: u64,
    mode: i32,
    ty: i32,
    n: i32,
    index: u64,
    vertex: i32,
    base_instance: i32,
    instances: i32,
) {
    elements(
        "nvnCommandBufferDrawElementsInstanced",
        c,
        [mode, ty, n],
        index,
        vertex,
        (base_instance, instances),
    );
}

extern "C" fn draw_elements_indirect(c: u64, mode: i32, ty: i32, index: u64, indirect: u64) {
    draw(
        "nvnCommandBufferDrawElementsIndirect",
        c,
        DrawKind::ElementsIndirect,
        &[
            mode as u32,
            ty as u32,
            lo(index),
            hi(index),
            lo(indirect),
            hi(indirect),
        ],
    );
}

extern "C" fn multi_draw_arrays_indirect_count(
    c: u64,
    mode: i32,
    indirect: u64,
    count: u64,
    max: i32,
    stride: i64,
) {
    draw(
        "nvnCommandBufferMultiDrawArraysIndirectCount",
        c,
        DrawKind::MultiArraysIndirectCount,
        &[
            mode as u32,
            lo(indirect),
            hi(indirect),
            lo(count),
            hi(count),
            max as u32,
            stride as u32,
            (stride >> 32) as u32,
        ],
    );
}

extern "C" fn multi_draw_elements_indirect_count(
    c: u64,
    mode: i32,
    ty: i32,
    index: u64,
    indirect: u64,
    count: u64,
    max: i32,
    stride: i64,
) {
    draw(
        "nvnCommandBufferMultiDrawElementsIndirectCount",
        c,
        DrawKind::MultiElementsIndirectCount,
        &[
            mode as u32,
            ty as u32,
            lo(index),
            hi(index),
            lo(indirect),
            hi(indirect),
            lo(count),
            hi(count),
            max as u32,
            stride as u32,
            (stride >> 32) as u32,
        ],
    );
}

extern "C" fn draw_mesh_tasks(c: u64, first: u32, count: u32) {
    draw(
        "nvnCommandBufferDrawMeshTasks",
        c,
        DrawKind::MeshTasks,
        &[first, count],
    );
}

extern "C" fn draw_mesh_tasks_indirect(c: u64, indirect: u64) {
    draw(
        "nvnCommandBufferDrawMeshTasksIndirect",
        c,
        DrawKind::MeshTasksIndirect,
        &[lo(indirect), hi(indirect)],
    );
}

extern "C" fn draw_transform_feedback(c: u64, mode: i32, control: u64) {
    draw(
        "nvnCommandBufferDrawTransformFeedback",
        c,
        DrawKind::TransformFeedback,
        &[mode as u32, lo(control), hi(control)],
    );
}

extern "C" fn draw_texture(c: u64, handle: u64, dst: u64, src: u64) {
    let name = "nvnCommandBufferDrawTexture";
    guard(name, || {
        let mut w = [0u32; 9];
        w[0] = adjust(handle, bases(c)?, true, true) as u32;
        for (i, a) in [dst, src].into_iter().enumerate() {
            for k in 0..4 {
                w[1 + i * 4 + k] = Obj(a).u32(k as u64 * 4)?;
            }
        }
        draw(name, c, DrawKind::Texture, &w);
        Ok(())
    });
}

extern "C" fn dispatch_compute(c: u64, x: u32, y: u32, z: u32) {
    emit(
        "nvnCommandBufferDispatchCompute",
        c,
        ExtCode::Dispatch,
        0,
        &[x, y, z],
    );
}

extern "C" fn dispatch_compute_indirect(c: u64, indirect: u64) {
    emit(
        "nvnCommandBufferDispatchComputeIndirect",
        c,
        ExtCode::Dispatch,
        1,
        &[lo(indirect), hi(indirect)],
    );
}

extern "C" fn bind_transform_feedback_buffer(c: u64, i: i32, addr: u64, n: u64) {
    let name = "nvnCommandBufferBindTransformFeedbackBuffer";
    guard(name, || {
        let (i, _) = span(i, 1, TF_BUFFERS)?;
        emit(
            name,
            c,
            ExtCode::TransformFeedbackBuffer,
            i as u16,
            &[lo(addr), hi(addr), lo(n), hi(n)],
        );
        Ok(())
    });
}

extern "C" fn bind_transform_feedback_buffers(c: u64, first: i32, n: i32, ranges: u64) {
    let name = "nvnCommandBufferBindTransformFeedbackBuffers";
    guard(name, || -> AbiResult<()> {
        let (first, n) = span(first, n, TF_BUFFERS)?;
        for k in 0..n {
            let (addr, size) = range(ranges, k)?;
            emit(
                name,
                c,
                ExtCode::TransformFeedbackBuffer,
                (first + k) as u16,
                &[lo(addr), hi(addr), lo(size), hi(size)],
            );
        }
        Ok(())
    });
}

fn feedback(name: &'static str, c: u64, what: u16, control: u64) {
    emit(
        name,
        c,
        ExtCode::TransformFeedback,
        what,
        &[lo(control), hi(control)],
    );
}

extern "C" fn begin_transform_feedback(c: u64, control: u64) {
    feedback("nvnCommandBufferBeginTransformFeedback", c, 0, control);
}

extern "C" fn end_transform_feedback(c: u64, control: u64) {
    feedback("nvnCommandBufferEndTransformFeedback", c, 1, control);
}

extern "C" fn pause_transform_feedback(c: u64, control: u64) {
    feedback("nvnCommandBufferPauseTransformFeedback", c, 2, control);
}

extern "C" fn resume_transform_feedback(c: u64, control: u64) {
    feedback("nvnCommandBufferResumeTransformFeedback", c, 3, control);
}

pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnCommandBufferDrawArrays" => draw_arrays as extern "C" fn(u64, i32, i32, i32),
        "nvnCommandBufferDrawArraysInstanced" => draw_arrays_instanced as extern "C" fn(u64, i32, i32, i32, i32, i32),
        "nvnCommandBufferDrawArraysIndirect" => draw_arrays_indirect as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferDrawElements" => draw_elements as extern "C" fn(u64, i32, i32, i32, u64),
        "nvnCommandBufferDrawElementsBaseVertex" => draw_elements_base_vertex as extern "C" fn(u64, i32, i32, i32, u64, i32),
        "nvnCommandBufferDrawElementsInstanced" => draw_elements_instanced as extern "C" fn(u64, i32, i32, i32, u64, i32, i32, i32),
        "nvnCommandBufferDrawElementsIndirect" => draw_elements_indirect as extern "C" fn(u64, i32, i32, u64, u64),
        "nvnCommandBufferMultiDrawArraysIndirectCount" => multi_draw_arrays_indirect_count as extern "C" fn(u64, i32, u64, u64, i32, i64),
        "nvnCommandBufferMultiDrawElementsIndirectCount" => multi_draw_elements_indirect_count as extern "C" fn(u64, i32, i32, u64, u64, u64, i32, i64),
        "nvnCommandBufferDrawMeshTasks" => draw_mesh_tasks as extern "C" fn(u64, u32, u32),
        "nvnCommandBufferDrawMeshTasksIndirect" => draw_mesh_tasks_indirect as extern "C" fn(u64, u64),
        "nvnCommandBufferDrawTransformFeedback" => draw_transform_feedback as extern "C" fn(u64, i32, u64),
        "nvnCommandBufferDrawTexture" => draw_texture as extern "C" fn(u64, u64, u64, u64),
        "nvnCommandBufferDispatchCompute" => dispatch_compute as extern "C" fn(u64, u32, u32, u32),
        "nvnCommandBufferDispatchComputeIndirect" => dispatch_compute_indirect as extern "C" fn(u64, u64),
        "nvnCommandBufferBindTransformFeedbackBuffer" => bind_transform_feedback_buffer as extern "C" fn(u64, i32, u64, u64),
        "nvnCommandBufferBindTransformFeedbackBuffers" => bind_transform_feedback_buffers as extern "C" fn(u64, i32, i32, u64),
        "nvnCommandBufferBeginTransformFeedback" => begin_transform_feedback as extern "C" fn(u64, u64),
        "nvnCommandBufferEndTransformFeedback" => end_transform_feedback as extern "C" fn(u64, u64),
        "nvnCommandBufferPauseTransformFeedback" => pause_transform_feedback as extern "C" fn(u64, u64),
        "nvnCommandBufferResumeTransformFeedback" => resume_transform_feedback as extern "C" fn(u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draw_table_has_every_draw_function() {
        let names: Vec<_> = procs().into_iter().map(|(n, _)| n).collect();
        for n in [
            "DrawArrays",
            "DrawElementsInstanced",
            "DispatchComputeIndirect",
            "DrawTexture",
        ] {
            assert!(
                names.contains(&format!("nvnCommandBuffer{n}").as_str()),
                "{n}"
            );
        }
    }
}
