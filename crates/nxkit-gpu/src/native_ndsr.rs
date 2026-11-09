//! agl's NDSR upscaler as a [`Replacement`].
//!
//! The network table, the fingerprint, the plan and the kernels live in `nxkit-ndsr`. This file says where the game
//! binds the weights and the pictures, and what makes one plan different from another: the network (its key), the input
//! and output sizes and the push words the kernel reads. The dynamic resolution changes the sizes every few frames, so
//! each size gets its own plan in the cache of [`crate::native`] and a size that comes back costs nothing.
//!
//! The run is one dispatch, the network. The three SMAA dispatches before it stay the game's own; the capture
//! (`NXKIT_DIAG=ndsr`) lists them so that `docs/plans/ndsr-native.md` section 10 can be filled from one log. The numbers
//! come from `docs/plans/ndsr-native.md` sections 4, 6 and 7.9; a wrong guess makes the run fall back to the game's
//! dispatch (`ndsr.fallback`), never a wrong picture. `NXKIT_NDSR=0` turns the network off, `NXKIT_NDSR_COMPARE=1` runs
//! both on a sampled frame and prints the largest difference per channel.

use std::hash::{BuildHasher, Hasher};

use nxkit_metal::Format;
use nxkit_ndsr::plan::{GameResource, Plan};

use crate::native::{BlobSpec, Bound, Place, Replacement, ResourceSpec, Run, RunInfo, Shape};

/// The upscaler of the SDK's NDSR library.
pub(crate) struct Ndsr;

/// The network's colour input: `tex0`, sampled with the game's sampler.
const INPUT: GameResource = GameResource::Texture(0);
/// The push constants of the network (bank 24).
const PUSH: GameResource = GameResource::Push(0);
/// The upscaled picture: `img1`.
const OUTPUT: GameResource = GameResource::Output;

/// The SMAA dispatches in front of the network that the capture lists: edges, blending weights, neighbourhood blending.
const SMAA_LEAD: usize = 3;

/// The weights are ssbo 2 of the network dispatch. Five layers' worth of blocks is 0x3000 bytes, the 4/3 networks have
/// three blocks (0x1800).
const FULL_BLOB: u64 = 0x3000;
const SMALL_BLOB: u64 = 0x1800;

static BLOBS_FULL: [BlobSpec; 1] = [BlobSpec {
    dispatch: 0,
    slot: 2,
    offset: 0,
    len: FULL_BLOB,
}];
static BLOBS_SMALL: [BlobSpec; 1] = [BlobSpec {
    dispatch: 0,
    slot: 2,
    offset: 0,
    len: SMALL_BLOB,
}];

static RESOURCES: [ResourceSpec; 3] = [
    ResourceSpec {
        resource: INPUT,
        place: Place::Texture {
            dispatch: 0,
            slot: 0,
        },
    },
    ResourceSpec {
        resource: OUTPUT,
        place: Place::Image {
            dispatch: 0,
            slot: 1,
        },
    },
    ResourceSpec {
        resource: PUSH,
        place: Place::Push { dispatch: 0 },
    },
];

/// The output encodings of `ndsr-native.md` section 7.7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Variant {
    /// sRGB-encoded 8-bit.
    A,
    /// Linear 10-bit.
    B,
    /// Linear R11G11B10F.
    C,
}

impl Variant {
    /// The formats of `img1` that this variant writes. The backing may be the sRGB twin of the plain format. The
    /// vr_light programs B and C write linear values without a dither (section 15.4), so any float or 10-bit image.
    fn output_formats(self, light: bool) -> &'static [Format] {
        match self {
            Self::B | Self::C if light => &[
                Format::Rg11B10Float,
                Format::Rgb10A2Unorm,
                Format::Rgba16Float,
            ],
            Self::A => &[
                Format::Rgba8Unorm,
                Format::Rgba8UnormSrgb,
                Format::Bgra8Unorm,
                Format::Bgra8UnormSrgb,
            ],
            Self::B => &[Format::Rgb10A2Unorm],
            Self::C => &[Format::Rg11B10Float],
        }
    }
}

/// The formats of `tex0`: the float4 that neighbourhood blending `92918e9b` writes.
const INPUT_FORMATS: [Format; 2] = [Format::Rgba16Float, Format::Rgba32Float];
/// The formats of `tex0` for the vr_light networks: the sqrt RGB of `a335c1db` (RGB9E5, as Ocarina of Time binds it) or
/// of `1fdc8517` (a float4).
const LIGHT_INPUT_FORMATS: [Format; 4] = [
    Format::Rgb9E5Float,
    Format::Rg11B10Float,
    Format::Rgba16Float,
    Format::Rgba32Float,
];

impl Replacement for Ndsr {
    fn name(&self) -> &'static str {
        "ndsr"
    }

    fn find(&self, keys: &[&str]) -> Option<Run> {
        api::find(keys).map(|(start, _)| Run { start, len: 1 })
    }

    fn blobs(&self) -> &'static [BlobSpec] {
        &BLOBS_FULL
    }

    fn accepts(&self, _blobs: &[&[u8]]) -> bool {
        false
    }

    fn plan(&self, _blobs: &[&[u8]]) -> Result<Plan, String> {
        Err("the plan follows the picture sizes: plan_in".into())
    }

    fn source(&self) -> &'static str {
        api::source()
    }

    fn resources(&self) -> &'static [ResourceSpec] {
        &RESOURCES
    }

    fn lead(&self) -> usize {
        SMAA_LEAD
    }

    fn shape(&self, info: &RunInfo<'_>) -> Shape {
        shape_of(info)
    }

    fn accepts_in(&self, blobs: &[&[u8]], _shape: &Shape, info: &RunInfo<'_>) -> bool {
        match (api::find(&info.keys), blobs) {
            (Some((_, net)), [blob]) => api::weights_ok(net, blob),
            _ => false,
        }
    }

    fn plan_in(&self, blobs: &[&[u8]], _shape: &Shape, info: &RunInfo<'_>) -> Result<Plan, String> {
        let (_, net) = api::find(&info.keys).ok_or("the key is no network of the table")?;
        let [blob] = blobs else {
            return Err("the network needs one weight blob".into());
        };
        let (input, output) = pictures(info).ok_or("tex0 or img1 is not bound")?;
        let push = info
            .pushes
            .first()
            .copied()
            .flatten()
            .ok_or("the network dispatch pushed no constants")?;
        let (size_in, size_out) = plan_sizes(push, input, output);
        api::plan(net, blob, size_in, size_out, push)
    }
}

/// `tex0` and `img1` of the run.
fn pictures<'a>(info: &'a RunInfo<'_>) -> Option<(&'a Bound, &'a Bound)> {
    Some((info.bound(INPUT)?, info.bound(OUTPUT)?))
}

/// The word `i` of the push constants.
fn word(push: &[u8], i: usize) -> Option<u32> {
    let b: [u8; 4] = push.get(i * 4..i * 4 + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// The input and output sizes the plan is made for. The push words say them (`ndsr-native.md` section 7.9: words 2 and 3
/// are `W_in - 1` and `H_in - 1`, words 6 and 7 are `W_out - 1` and `H_out - 1`), because the game may render into a
/// part of a larger target. When the words are missing or do not fit inside the pictures, the pictures' sizes stand.
fn plan_sizes(push: &[u8], input: &Bound, output: &Bound) -> ([u32; 2], [u32; 2]) {
    let from_push = |a: usize, b: usize, max: [u32; 2]| {
        let (w, h) = (
            word(push, a)?.checked_add(1)?,
            word(push, b)?.checked_add(1)?,
        );
        (w <= max[0] && h <= max[1]).then_some([w, h])
    };
    let (pi, po) = ([input.width, input.height], [output.width, output.height]);
    (
        from_push(2, 3, pi).unwrap_or(pi),
        from_push(6, 7, po).unwrap_or(po),
    )
}

/// The push words that change the kernel's result: the sizes, offsets and reciprocals (2 to 9), the unsharp strength
/// and the dither switch (18, 19); for vr_light the tile, its mapping, the output size and the strength (18 to 28,
/// section 15.4). The others are not read (section 7.9); a word that moves every frame would make a plan every frame.
fn push_tag(push: &[u8], light: bool) -> u64 {
    let mut h = foldhash::fast::FixedState::with_seed(0).build_hasher();
    for i in (2..10).chain(18..if light { 29 } else { 20 }) {
        h.write_u32(word(push, i).unwrap_or(u32::MAX));
    }
    h.finish()
}

fn hash_str(s: &str) -> u64 {
    let mut h = foldhash::fast::FixedState::with_seed(0).build_hasher();
    h.write(s.as_bytes());
    h.finish()
}

fn halves(v: u64) -> [u32; 2] {
    [v as u32, (v >> 32) as u32]
}

/// What the run is: its weights, its tag and a reason to refuse it.
fn shape_of(info: &RunInfo<'_>) -> Shape {
    let refused = |blobs, tag, why: String| Shape {
        blobs,
        tag,
        refusal: Some(why),
    };
    let Some((_, net)) = api::find(&info.keys) else {
        return refused(
            &BLOBS_FULL,
            Vec::new(),
            format!("the key {:?} is no network of the table", info.keys),
        );
    };
    let blobs: &'static [BlobSpec] = match api::blob_len(net) as u64 {
        FULL_BLOB => &BLOBS_FULL,
        SMALL_BLOB => &BLOBS_SMALL,
        n => {
            return refused(
                &BLOBS_FULL,
                Vec::new(),
                format!("{}: weights of {n:#x} bytes", api::key(net)),
            );
        }
    };
    let mut tag = halves(hash_str(api::key(net))).to_vec();
    let Some((input, output)) = pictures(info) else {
        return refused(blobs, tag, "tex0 or img1 is not bound".into());
    };
    let Some(push) = info.pushes.first().copied().flatten() else {
        return refused(
            blobs,
            tag,
            "the network dispatch pushed no constants".into(),
        );
    };
    let (size_in, size_out) = plan_sizes(push, input, output);
    tag.extend([size_in[0], size_in[1], size_out[0], size_out[1]]);
    let light = api::is_light(net);
    tag.extend(halves(push_tag(push, light)));
    let variant = api::variant(net);
    let inputs: &[Format] = if light {
        &LIGHT_INPUT_FORMATS
    } else {
        &INPUT_FORMATS
    };
    let refusal = if let Some(why) = api::unsupported(net) {
        Some(why)
    } else if !inputs.contains(&input.format) {
        Some(format!(
            "tex0 has format {:?}, not one of {inputs:?}",
            input.format
        ))
    } else if !variant.output_formats(light).contains(&output.format) {
        Some(format!(
            "{}: img1 has format {:?}, variant {variant:?} writes {:?}",
            api::key(net),
            output.format,
            variant.output_formats(light)
        ))
    } else {
        None
    };
    Shape {
        blobs,
        tag,
        refusal,
    }
}

/// The surface of `nxkit-ndsr` this file codes against. A rename there changes this module and nothing else.
mod api {
    use super::Variant;
    use nxkit_ndsr::net::{Net, Pack};
    use nxkit_ndsr::plan::Plan;
    use nxkit_ndsr::{fingerprint, kernels, msl, net};

    pub(super) fn find(keys: &[&str]) -> Option<(usize, &'static Net)> {
        fingerprint::find(keys)
    }

    pub(super) fn weights_ok(net: &Net, blob: &[u8]) -> bool {
        fingerprint::weights_ok(net, blob)
    }

    pub(super) fn plan(
        net: &Net,
        blob: &[u8],
        size_in: [u32; 2],
        size_out: [u32; 2],
        push: &[u8],
    ) -> Result<Plan, String> {
        kernels::plan(net, blob, size_in, size_out, push)
    }

    pub(super) fn source() -> &'static str {
        msl::SOURCE
    }

    pub(super) fn key(net: &Net) -> &'static str {
        net.key
    }

    pub(super) fn blob_len(net: &Net) -> usize {
        net.blob().len as usize
    }

    /// Why the network is not run natively, if it is not: the vr programs and the vr_light programs that have a
    /// kernel run, agl_ndsr does not.
    pub(super) fn unsupported(net: &Net) -> Option<String> {
        let runs = match net.pack {
            Pack::Vr => true,
            Pack::VrLight => kernels::light::find(net).is_some(),
            Pack::Agl => false,
        };
        (!runs).then(|| {
            format!(
                "{}: the {} pack is not run natively yet",
                net.key,
                net.pack.name()
            )
        })
    }

    /// The program is of the vr_light pack.
    pub(super) fn is_light(net: &Net) -> bool {
        net.pack == Pack::VrLight
    }

    pub(super) fn variant(net: &Net) -> Variant {
        match net.variant {
            net::Variant::A => Variant::A,
            net::Variant::B => Variant::B,
            net::Variant::C => Variant::C,
        }
    }
}

#[cfg(test)]
mod tests;
