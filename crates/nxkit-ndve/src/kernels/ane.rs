//! The GPU's share of the Neural Engine path (`docs/plans/ndve-ane.md`): the epilogue that turns `final` and `delta`
//! into the game's RGBA8 image. The kernels are in `msl/ane.metal`. The tensors are bound from the slot that holds the
//! picture's result ([`plan::ANE_FINAL`], [`plan::ANE_DELTA`]); the game's own resources are those of the Metal plan.
//! The model's input is not the GPU's: the host writes it from the decoded picture, ahead of the game.

use crate::plan::{self, Binding, GameResource, Plan, Source, Step};

/// The name of the model's input tensor.
pub const INPUT: &str = "x";

/// The names of the model's outputs and the resources they are bound as.
pub const OUTPUTS: [(&str, GameResource); 2] =
    [("final", plan::ANE_FINAL), ("delta", plan::ANE_DELTA)];

fn tex(index: u32, resource: GameResource, writes: bool) -> Binding {
    Binding {
        index,
        source: Source::Game(resource),
        writes,
    }
}

fn step(
    kernel: &'static str,
    groups: [u32; 3],
    buffers: Vec<Binding>,
    textures: Vec<Binding>,
) -> Step {
    Step {
        kernel,
        groups,
        threads: [16, 8, 1],
        threadgroup_bytes: 0,
        buffers,
        textures,
    }
}

/// The epilogue: writes the picture of layer 12 (RGB10A2) from `final` and the game's output from the picture and
/// `delta`. It runs in place of the Metal plan when the Neural Engine's result of the picture is ready. It owns no
/// buffers.
#[must_use]
pub fn ane_epilogue() -> Plan {
    Plan {
        buffers: vec![],
        steps: vec![
            step(
                "ndve_ane_quant",
                [120, 135, 1],
                vec![],
                vec![
                    tex(0, plan::ANE_FINAL, false),
                    tex(1, plan::PICTURE_RGB10A2, true),
                ],
            ),
            step(
                "ndve_ane_epilogue",
                [160, 180, 1],
                vec![tex(0, plan::RESAMPLER_PUSH, false)],
                vec![
                    tex(0, plan::PICTURE_RGB10A2, false),
                    tex(1, plan::DITHER, false),
                    tex(2, plan::OUTPUT, true),
                    tex(3, plan::ANE_DELTA, false),
                ],
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grids_cover_the_images() {
        let p = ane_epilogue();
        let covers = |s: &Step, w: u32, h: u32| {
            s.groups[0] * s.threads[0] >= w && s.groups[1] * s.threads[1] >= h
        };
        assert!(covers(&p.steps[0], 1920, 1080));
        assert!(covers(&p.steps[1], 2560, 1440));
    }

    #[test]
    fn the_kernels_exist_with_the_bound_indices() {
        let src = crate::msl::SOURCE;
        for s in &ane_epilogue().steps {
            let at = src
                .find(&format!("kernel void {}(", s.kernel))
                .unwrap_or_else(|| panic!("no kernel {}", s.kernel));
            // The signature ends at the body's brace; an attribute such as `[[texture(0)]]` has its own parentheses.
            let sig = &src[at..at + src[at..].find('{').unwrap_or(0)];
            for b in &s.buffers {
                assert!(sig.contains(&format!("buffer({})", b.index)), "{sig}");
            }
            for b in &s.textures {
                assert!(sig.contains(&format!("texture({})", b.index)), "{sig}");
            }
        }
    }

    #[test]
    fn the_tensors_are_only_read_by_the_gpu() {
        let p = ane_epilogue();
        for b in p.steps.iter().flat_map(|s| &s.textures) {
            if OUTPUTS.iter().any(|(_, r)| b.source == Source::Game(*r)) {
                assert!(!b.writes);
            }
        }
        assert_eq!(OUTPUTS.len(), 2);
    }
}
