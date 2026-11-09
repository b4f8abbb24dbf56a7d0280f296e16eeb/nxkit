//! The interface of [`super`]: what a network tells the module about a run.

use super::*;

/// The run of dispatches that a replacement found: `len` dispatches from the dispatch at index `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    /// The index of the first dispatch of the run in the frame's dispatch list.
    pub(crate) start: usize,
    /// The number of dispatches.
    pub(crate) len: usize,
}

/// A byte range of the weights, found through a storage buffer binding of the run. The `dispatch` counts from the
/// start of the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlobSpec {
    /// The dispatch of the run whose binding names the bytes.
    pub(crate) dispatch: usize,
    /// The slot of the storage buffer in that dispatch.
    pub(crate) slot: u32,
    /// Where the bound address lies in the blob. The blob starts at the address minus this.
    pub(crate) offset: u64,
    /// The length of the blob in bytes.
    pub(crate) len: u64,
}

/// Where a game resource is bound in the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// A sampled texture of a dispatch.
    Texture {
        /// The dispatch of the run.
        dispatch: usize,
        /// The texture slot.
        slot: u32,
    },
    /// A storage image of a dispatch.
    Image {
        /// The dispatch of the run.
        dispatch: usize,
        /// The image slot.
        slot: u32,
    },
    /// The compute push constants of a dispatch.
    Push {
        /// The dispatch of the run.
        dispatch: usize,
    },
}

/// One game resource and where the run binds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResourceSpec {
    /// The resource the plan names.
    pub(crate) resource: GameResource,
    /// Where it is bound.
    pub(crate) place: Place,
}

/// A network that can replace a run of dispatches.
pub(crate) trait Replacement: Send + Sync {
    /// A short name for the diagnostics and the health report.
    fn name(&self) -> &'static str;

    /// Finds the run in the program keys of the frame's dispatches.
    fn find(&self, keys: &[&str]) -> Option<Run>;

    /// The weight ranges to read. `accepts` and `plan` get their bytes in this order.
    fn blobs(&self) -> &'static [BlobSpec];

    /// True when the bytes are the weights of the network that this replacement implements.
    fn accepts(&self, blobs: &[&[u8]]) -> bool;

    /// Builds the plan from the weights. It is called once for each distinct set of bytes.
    fn plan(&self, blobs: &[&[u8]]) -> Result<Plan, String>;

    /// The Metal source of the kernels that the plan's steps name.
    fn source(&self) -> &'static str;

    /// Where the run binds the game resources that the plan names.
    fn resources(&self) -> &'static [ResourceSpec];

    /// How many dispatches before the run the capture lists as well (a network that follows fixed passes lists them).
    fn lead(&self) -> usize {
        0
    }

    /// What this run needs beyond the program keys: the weight ranges (their length may follow the key), a tag for the
    /// cache key (a network whose plan follows the picture sizes puts them there) and a reason to refuse the run. The
    /// default is the static [`Replacement::blobs`] and an empty tag.
    fn shape(&self, _info: &RunInfo<'_>) -> Shape {
        Shape {
            blobs: self.blobs(),
            tag: Vec::new(),
            refusal: None,
        }
    }

    /// [`Replacement::accepts`] with the shape and the run's facts.
    fn accepts_in(&self, blobs: &[&[u8]], _shape: &Shape, _info: &RunInfo<'_>) -> bool {
        self.accepts(blobs)
    }

    /// [`Replacement::plan`] with the shape and the run's facts.
    fn plan_in(
        &self,
        blobs: &[&[u8]],
        _shape: &Shape,
        _info: &RunInfo<'_>,
    ) -> Result<Plan, String> {
        self.plan(blobs)
    }
}

/// A game texture or image of the run as the plan sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bound {
    /// The resource.
    pub(crate) resource: GameResource,
    /// The width of the backing texture.
    pub(crate) width: u32,
    /// The height of the backing texture.
    pub(crate) height: u32,
    /// The pixel format of the backing texture.
    pub(crate) format: nxkit_metal::Format,
}

/// The facts of a found run that a replacement may look at before it reads the weights.
pub(crate) struct RunInfo<'a> {
    /// The program keys of the run's dispatches.
    pub(crate) keys: Vec<&'a str>,
    /// The push constants of each dispatch of the run.
    pub(crate) pushes: Vec<Option<&'a [u8]>>,
    /// Each texture and image of [`Replacement::resources`] that resolves.
    pub(crate) bound: Vec<Bound>,
}

impl<'a> RunInfo<'a> {
    /// The facts of `run` in this frame.
    pub(super) fn new(net: &dyn Replacement, run: Run, cx: &PrepareCx<'a>) -> Self {
        let span = &cx.detail.dispatches[run.start..run.start + run.len];
        let mut bound = Vec::new();
        for spec in net.resources() {
            let (Place::Texture { dispatch, slot } | Place::Image { dispatch, slot }) = spec.place
            else {
                continue;
            };
            let Some(d) = span.get(dispatch) else {
                continue;
            };
            let list = if matches!(spec.place, Place::Image { .. }) {
                &d.images
            } else {
                &d.textures
            };
            if let Some((_, res, _)) = bind_of(list, slot).and_then(|b| cx.resolve(b.handle)) {
                bound.push(Bound {
                    resource: spec.resource,
                    width: res.desc.width,
                    height: res.desc.height,
                    format: res.desc.format,
                });
            }
        }
        Self {
            keys: span.iter().map(|d| d.program.as_str()).collect(),
            pushes: span.iter().map(|d| d.push.as_deref()).collect(),
            bound,
        }
    }

    /// The texture or image bound for `resource`.
    pub(crate) fn bound(&self, resource: GameResource) -> Option<&Bound> {
        self.bound.iter().find(|b| b.resource == resource)
    }
}

/// What [`Replacement::shape`] says about a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    /// The weight ranges of this run.
    pub(crate) blobs: &'static [BlobSpec],
    /// Joins the cache key: runs with different tags get their own plan.
    pub(crate) tag: Vec<u32>,
    /// A reason to keep the game's dispatches, counted as `fallback(shape)`. The capture is still printed.
    pub(crate) refusal: Option<String>,
}
