//! The plans of the GPU's share of the Neural Engine path (`docs/plans/ndve-ane.md`): the input tensor from the
//! colour conversion, and the epilogue that turns `final` and `delta` into the game's RGBA8 image. The kernels are in
//! `msl/ane.metal`. The tensors are bound from the slot of the ring ([`plan::ANE_X`], [`plan::ANE_FINAL`],
//! [`plan::ANE_DELTA`]); the game's own resources are those of the Metal plan.

use crate::plan::{self, Binding, GameResource, Plan, Source, Step};

/// The two plans of the Neural Engine path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnePlans {
    /// Writes the input tensor from the game's NV12 planes. It runs for every picture.
    pub input: Plan,
    /// Writes the picture of layer 12 (RGB10A2) from `final` and the game's output from the picture and `delta`. It
    /// runs in place of the Metal plan when the Neural Engine's result of the previous picture is ready.
    pub epilogue: Plan,
}

/// The names of the model's tensors and the resources they are bound as: `x` is the input, the others the outputs.
pub const TENSORS: [(&str, GameResource); 3] = [
    ("x", plan::ANE_X),
    ("final", plan::ANE_FINAL),
    ("delta", plan::ANE_DELTA),
];

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

/// The plans. They own no buffers.
#[must_use]
pub fn ane_plans() -> AnePlans {
    let input = Plan {
        buffers: vec![],
        steps: vec![step(
            "ndve_ane_input",
            [120, 135, 1],
            vec![tex(0, plan::CONVERSION_PUSH, false)],
            vec![
                tex(0, plan::LUMA_PLANE, false),
                tex(1, plan::CHROMA_PLANE, false),
                tex(2, plan::ANE_X, true),
            ],
        )],
    };
    let epilogue = Plan {
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
    };
    AnePlans { input, epilogue }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grids_cover_the_images() {
        let p = ane_plans();
        let covers = |s: &Step, w: u32, h: u32| {
            s.groups[0] * s.threads[0] >= w && s.groups[1] * s.threads[1] >= h
        };
        assert!(covers(&p.input.steps[0], 1920, 1080));
        assert!(covers(&p.epilogue.steps[0], 1920, 1080));
        assert!(covers(&p.epilogue.steps[1], 2560, 1440));
    }

    #[test]
    fn the_kernels_exist_with_the_bound_indices() {
        let src = crate::msl::SOURCE;
        for s in p_steps() {
            let at = src
                .find(&format!("kernel void {}(", s.kernel))
                .unwrap_or_else(|| panic!("no kernel {}", s.kernel));
            let sig = &src[at..at + src[at..].find(')').unwrap_or(0)];
            for b in &s.buffers {
                assert!(sig.contains(&format!("buffer({})", b.index)), "{sig}");
            }
            for b in &s.textures {
                assert!(sig.contains(&format!("texture({})", b.index)), "{sig}");
            }
        }
    }

    fn p_steps() -> Vec<Step> {
        let p = ane_plans();
        p.input.steps.into_iter().chain(p.epilogue.steps).collect()
    }

    #[test]
    fn the_tensors_are_written_only_by_the_input_step() {
        let p = ane_plans();
        for b in &p.epilogue.steps[1].textures {
            if b.source == Source::Game(plan::ANE_DELTA) {
                assert!(!b.writes);
            }
        }
        assert!(p.input.steps[0].textures[2].writes);
        assert_eq!(TENSORS.len(), 3);
    }
}
