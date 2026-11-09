//! The movie network of `nn::ndve` as a [`Replacement`].
//!
//! The fingerprint, the plan and the kernels live in `nxkit-ndve`. This file says where the game binds the weights and
//! the pictures. The numbers come from `docs/plans/ndve-native.md`; the ones marked "to confirm" are guesses that the
//! first `NXKIT_DIAG=ndve` capture settles. A wrong guess makes the run fall back to the game's dispatches
//! (`ndve.fallback`), never a wrong picture.

use nxkit_ndve::plan::{self, Plan};

use crate::native::{BlobSpec, Place, Replacement, ResourceSpec, Run};

#[cfg(not(test))]
use nxkit_ndve::{fingerprint, kernels, msl};
#[cfg(test)]
use stub::{fingerprint, kernels, msl};

/// The network of the movie library.
pub(crate) struct Ndve;

/// The weights of layers 1 to 12 are one blob, bound as ssbo 2 of layer 1 at offset 0 (`ndve-native.md` section 4).
const LAYER_BLOB_LEN: u64 = 0x3c320;
/// The resampler's weights are 0x3000 bytes. The slot is to confirm.
const RESAMPLER_BLOB_LEN: u64 = 0x3000;

/// The dispatches of the run, counted from the colour conversion (pass 3).
const CONVERSION: usize = 0;
const LAYER_1: usize = 1;
const LAYER_12: usize = 12;
const RESAMPLER: usize = 13;

static BLOBS: [BlobSpec; 2] = [
    BlobSpec {
        dispatch: LAYER_1,
        slot: 2,
        offset: 0,
        len: LAYER_BLOB_LEN,
    },
    // To confirm: the slot and whether the bound address is the start of the 0x3000 bytes.
    BlobSpec {
        dispatch: RESAMPLER,
        slot: 2,
        offset: 0,
        len: RESAMPLER_BLOB_LEN,
    },
];

static RESOURCES: [ResourceSpec; 8] = [
    // Texture register 1 and 2 of the conversion (section 7).
    ResourceSpec {
        resource: plan::LUMA_PLANE,
        place: Place::Texture {
            dispatch: CONVERSION,
            slot: 1,
        },
    },
    ResourceSpec {
        resource: plan::CHROMA_PLANE,
        place: Place::Texture {
            dispatch: CONVERSION,
            slot: 2,
        },
    },
    ResourceSpec {
        resource: plan::CONVERSION_PUSH,
        place: Place::Push {
            dispatch: CONVERSION,
        },
    },
    // Image register 1 and texture register 3 of the resampler (section 8).
    ResourceSpec {
        resource: plan::OUTPUT,
        place: Place::Image {
            dispatch: RESAMPLER,
            slot: 1,
        },
    },
    ResourceSpec {
        resource: plan::DITHER,
        place: Place::Texture {
            dispatch: RESAMPLER,
            slot: 3,
        },
    },
    ResourceSpec {
        resource: plan::RESAMPLER_PUSH,
        place: Place::Push {
            dispatch: RESAMPLER,
        },
    },
    // To confirm: the image slots of the two pictures the game's chain writes.
    ResourceSpec {
        resource: plan::PICTURE_RGBA8,
        place: Place::Image {
            dispatch: CONVERSION,
            slot: 0,
        },
    },
    ResourceSpec {
        resource: plan::PICTURE_RGB10A2,
        place: Place::Image {
            dispatch: LAYER_12,
            slot: 0,
        },
    },
];

impl Replacement for Ndve {
    fn name(&self) -> &'static str {
        "ndve"
    }

    fn find(&self, keys: &[&str]) -> Option<Run> {
        find_run(keys)
    }

    fn blobs(&self) -> &'static [BlobSpec] {
        &BLOBS
    }

    fn accepts(&self, blobs: &[&[u8]]) -> bool {
        match blobs {
            [layers, resampler] => weights_ok(layers, resampler),
            _ => false,
        }
    }

    fn plan(&self, blobs: &[&[u8]]) -> Result<Plan, String> {
        match blobs {
            [layers, resampler] => build_plan(layers, resampler),
            _ => Err("the network needs two weight blobs".into()),
        }
    }

    fn source(&self) -> &'static str {
        msl_source()
    }

    fn resources(&self) -> &'static [ResourceSpec] {
        &RESOURCES
    }
}

// The four adapters. A rename in `nxkit-ndve` changes one of these and nothing else.

/// `fingerprint::matches`: the run in the keys. `Match` has `start` (the index of the first key of the run) and `len`.
fn find_run(keys: &[&str]) -> Option<Run> {
    fingerprint::matches(keys).map(|m| Run {
        start: m.start,
        len: m.len,
    })
}

/// `fingerprint::weights_ok`: the layer blob and the resampler's weights are those of the network.
fn weights_ok(layers: &[u8], resampler: &[u8]) -> bool {
    fingerprint::weights_ok(layers, resampler)
}

/// `kernels::plan`: the native passes for these weights.
fn build_plan(layers: &[u8], resampler: &[u8]) -> Result<Plan, String> {
    kernels::plan(layers, resampler)
}

/// `msl::SOURCE`: the kernels.
fn msl_source() -> &'static str {
    msl::SOURCE
}

/// The functions of `nxkit-ndve` that do not exist yet, as a test sees them.
#[cfg(test)]
pub(crate) mod stub {
    pub(crate) mod fingerprint {
        pub(crate) struct Match {
            pub(crate) start: usize,
            pub(crate) len: usize,
        }

        /// The stub finds the keys `a`, `b`, `c` in a row.
        pub(crate) fn matches(keys: &[&str]) -> Option<Match> {
            keys.windows(3)
                .position(|w| w == ["a", "b", "c"])
                .map(|start| Match { start, len: 3 })
        }

        pub(crate) fn weights_ok(blob: &[u8], resampler: &[u8]) -> bool {
            blob.len() == 4 && resampler.len() == 2
        }
    }

    pub(crate) mod kernels {
        use nxkit_ndve::plan::Plan;

        pub(crate) fn plan(_weights: &[u8], _resampler: &[u8]) -> Result<Plan, String> {
            Ok(Plan {
                buffers: vec![],
                steps: vec![],
            })
        }
    }

    pub(crate) mod msl {
        pub(crate) const SOURCE: &str = "";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapters_pass_the_calls_through() {
        let n = Ndve;
        assert_eq!(
            n.find(&["x", "a", "b", "c", "y"]),
            Some(Run { start: 1, len: 3 })
        );
        assert_eq!(n.find(&["a", "b"]), None);
        assert!(n.accepts(&[&[0; 4], &[0; 2]]));
        assert!(!n.accepts(&[&[0; 4]]));
        assert!(n.plan(&[&[0; 4], &[0; 2]]).is_ok());
        assert!(n.plan(&[&[0; 4]]).is_err());
        assert_eq!(n.source(), "");
    }

    #[test]
    fn the_blobs_and_resources_stay_inside_the_run() {
        let n = Ndve;
        for b in n.blobs() {
            assert!(b.dispatch <= RESAMPLER);
        }
        for r in n.resources() {
            let d = match r.place {
                Place::Texture { dispatch, .. }
                | Place::Image { dispatch, .. }
                | Place::Push { dispatch } => dispatch,
            };
            assert!(d <= RESAMPLER, "{r:?}");
        }
    }
}
