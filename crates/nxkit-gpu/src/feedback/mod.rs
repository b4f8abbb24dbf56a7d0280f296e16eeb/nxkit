//! Transform feedback captures, with and without a geometry shader.
//!
//! A draw while a capture is on ([`crate::detail::CaptureDetail`]) rasterizes nothing that the game keeps: its records
//! go into the feedback buffers. The encoder leaves it out of its render pass and runs the capture after the pass
//! ([`run`]), in the order of the pass:
//!
//! 1. A pass without attachments captures the vertex outputs of the draws with a geometry shader (`main0_gs` of the
//!    vertex library, 144 words a vertex), with rasterization off.
//! 2. A compute pass, per capture: the state step (a begin zeroes NVN's control state, a resume loads it from the
//!    control block), the geometry kernel `_bg<t>` over the captured vertices and its compaction into the order of the
//!    emitted vertices, the records that fit into the buffers, and the save of the state into the control block of an
//!    end or a pause (`kernels.metal`).
//! 3. A pass without attachments draws the capture entry `main0_tf` (of the vertex library, or of `_bgv` for a geometry
//!    shader) with as many vertices as there are records, an indirect draw from the state of step 2.
//!
//! The control block is NVN's: the byte offsets of the four buffers, then the draw arguments of what the capture wrote
//! (records, 1, 0, 0). `DrawTransformFeedback` draws from there (`walk::ext::draw`), so the counts stay on the GPU.

mod run;

pub(crate) use run::run;

use crate::detail::DrawArgs;

/// Whether the encoder can run a capture of a draw with `args` and the `NVNdrawPrimitive` `topology`. A record is a
/// vertex of the draw in its order, so only arrays (direct or indirect, which `DrawTransformFeedback` is) of lists:
/// points, lines and triangles. An indexed draw would capture the index as the vertex, a strip its vertices once.
pub(crate) fn runnable(args: &DrawArgs, topology: u32) -> bool {
    matches!(
        args,
        DrawArgs::Arrays { .. } | DrawArgs::ArraysIndirect { .. }
    ) && matches!(topology, 0 | 1 | 4)
}

/// The input topology of a geometry kernel (`_bg<t>`) for an `NVNdrawPrimitive`: 0 points, 1 lines, 3 triangles.
pub(crate) fn kernel_topology(topology: u32) -> Option<u32> {
    match topology {
        0 => Some(0),
        1 => Some(1),
        4 => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod kernel_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_arrays_of_lists_capture() {
        let arrays = DrawArgs::Arrays {
            first: 0,
            count: 3,
            instances: 1,
            base_instance: 0,
        };
        assert!(runnable(&arrays, 0) && runnable(&arrays, 4));
        assert!(!runnable(&arrays, 5), "a strip");
        assert!(runnable(&DrawArgs::ArraysIndirect { indirect: 0x100 }, 0));
        let indexed = DrawArgs::Indexed {
            index_type: 1,
            address: 0,
            count: 3,
            instances: 1,
            base_vertex: 0,
            base_instance: 0,
        };
        assert!(!runnable(&indexed, 4));
        assert_eq!(
            [0, 1, 4, 5].map(kernel_topology),
            [Some(0), Some(1), Some(3), None]
        );
    }
}
