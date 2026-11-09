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
    /// The formats of `img1` that this variant writes. The backing may be the sRGB twin of the plain format.
    fn output_formats(self) -> &'static [Format] {
        match self {
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
/// and the dither switch (18, 19). The others are not read (section 7.9); a word that moves every frame would make a
/// plan every frame.
fn push_tag(push: &[u8]) -> u64 {
    let mut h = foldhash::fast::FixedState::with_seed(0).build_hasher();
    for i in (2..10).chain(18..20) {
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
    tag.extend(halves(push_tag(push)));
    let variant = api::variant(net);
    let refusal = if !INPUT_FORMATS.contains(&input.format) {
        Some(format!("tex0 has format {:?}, not a float4", input.format))
    } else if !variant.output_formats().contains(&output.format) {
        Some(format!(
            "{}: img1 has format {:?}, variant {variant:?} writes {:?}",
            api::key(net),
            output.format,
            variant.output_formats()
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
///
/// Until packages A and B are merged the crate has no table and no kernels, so the module below stands in for them: it
/// knows no network (the hook never finds a run) and the tests give it two. On the merge, delete the `lib` module, point
/// `api` at `nxkit_ndsr::{fingerprint, kernels, msl, net}` and drop the `#[cfg(test)]` table. The signatures assumed:
///
/// - `fingerprint::find(keys: &[&str]) -> Option<(usize, &'static Net)>`: the index in `keys` of the first dispatch
///   whose key is a network of the table, and the network.
/// - `fingerprint::weights_ok(net: &Net, blob: &[u8]) -> bool`.
/// - `kernels::plan(net: &Net, blob: &[u8], in_size: [u32; 2], out_size: [u32; 2], push: &[u8]) -> Result<Plan, String>`.
/// - `msl::SOURCE: &str`.
/// - `net::Net` with `key: &'static str`, `blob_len: usize` and `variant: net::Variant { A, B, C }`.
mod api {
    use super::Variant;
    use super::lib::{self, Net};
    use nxkit_ndsr::plan::Plan;

    pub(super) fn find(keys: &[&str]) -> Option<(usize, &'static Net)> {
        lib::fingerprint::find(keys)
    }

    pub(super) fn weights_ok(net: &Net, blob: &[u8]) -> bool {
        lib::fingerprint::weights_ok(net, blob)
    }

    pub(super) fn plan(
        net: &Net,
        blob: &[u8],
        size_in: [u32; 2],
        size_out: [u32; 2],
        push: &[u8],
    ) -> Result<Plan, String> {
        lib::kernels::plan(net, blob, size_in, size_out, push)
    }

    pub(super) fn source() -> &'static str {
        lib::msl::SOURCE
    }

    pub(super) fn key(net: &Net) -> &'static str {
        net.key
    }

    pub(super) fn blob_len(net: &Net) -> usize {
        net.blob_len
    }

    pub(super) fn variant(net: &Net) -> Variant {
        match net.variant {
            lib::Variant::A => Variant::A,
            lib::Variant::B => Variant::B,
            lib::Variant::C => Variant::C,
        }
    }
}

/// Stand-in for the parts of `nxkit-ndsr` that packages A and B write. See [`api`].
#[allow(dead_code)]
mod lib {
    use nxkit_ndsr::plan::Plan;

    pub(super) enum Variant {
        A,
        B,
        C,
    }

    pub(super) struct Net {
        pub(super) key: &'static str,
        pub(super) blob_len: usize,
        pub(super) variant: Variant,
    }

    #[cfg(test)]
    static NETS: [Net; 2] = [
        Net {
            key: "aaaa",
            blob_len: 0x3000,
            variant: Variant::A,
        },
        Net {
            key: "bbbb",
            blob_len: 0x1800,
            variant: Variant::C,
        },
    ];
    #[cfg(not(test))]
    static NETS: [Net; 0] = [];

    pub(super) mod fingerprint {
        use super::{NETS, Net};

        pub(in super::super) fn find(keys: &[&str]) -> Option<(usize, &'static Net)> {
            keys.iter()
                .enumerate()
                .find_map(|(i, k)| NETS.iter().find(|n| n.key == *k).map(|n| (i, n)))
        }

        pub(in super::super) fn weights_ok(net: &Net, blob: &[u8]) -> bool {
            blob.len() == net.blob_len
        }
    }

    pub(super) mod kernels {
        use super::{Net, Plan};

        /// The test plan carries the sizes it was made for in its one buffer.
        pub(in super::super) fn plan(
            _net: &Net,
            _blob: &[u8],
            size_in: [u32; 2],
            size_out: [u32; 2],
            _push: &[u8],
        ) -> Result<Plan, String> {
            if cfg!(not(test)) {
                return Err("nxkit-ndsr has no kernels yet".into());
            }
            let init = [size_in, size_out]
                .concat()
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            Ok(Plan {
                buffers: vec![nxkit_ndsr::plan::Buffer {
                    name: "sizes",
                    bytes: 16,
                    init: Some(init),
                }],
                steps: vec![],
            })
        }
    }

    pub(super) mod msl {
        pub(in super::super) const SOURCE: &str = "";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(resource: GameResource, w: u32, h: u32, format: Format) -> Bound {
        Bound {
            resource,
            width: w,
            height: h,
            format,
        }
    }

    /// A push block of 20 words with the vr layout of section 7.9.
    fn push(w_in: u32, h_in: u32, w_out: u32, h_out: u32) -> Vec<u8> {
        let mut words = [0u32; 20];
        words[2] = w_in - 1;
        words[3] = h_in - 1;
        words[6] = w_out - 1;
        words[7] = h_out - 1;
        words[8] = (1.0f32 / w_in as f32).to_bits();
        words[9] = (1.0f32 / h_in as f32).to_bits();
        words[18] = 1.5f32.to_bits();
        words[19] = 1;
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn info<'a>(key: &'a str, push: &'a [u8], out: Format, size_out: [u32; 2]) -> RunInfo<'a> {
        RunInfo {
            keys: vec![key],
            pushes: vec![Some(push)],
            bound: vec![
                bound(INPUT, 1536, 864, Format::Rgba16Float),
                bound(OUTPUT, size_out[0], size_out[1], out),
            ],
        }
    }

    #[test]
    fn the_run_is_the_network_dispatch_among_the_smaa_ones() {
        let n = Ndsr;
        let keys = ["f8965bff", "d17eefaf", "92918e9b", "aaaa", "x"];
        assert_eq!(n.find(&keys), Some(Run { start: 3, len: 1 }));
        assert_eq!(n.find(&["f8965bff", "92918e9b"]), None);
        assert_eq!(n.name(), "ndsr");
        assert_eq!(n.lead(), 3);
    }

    #[test]
    fn the_resources_stay_inside_the_one_dispatch_run() {
        for r in Ndsr.resources() {
            let d = match r.place {
                Place::Texture { dispatch, .. }
                | Place::Image { dispatch, .. }
                | Place::Push { dispatch } => dispatch,
            };
            assert_eq!(d, 0, "{r:?}");
        }
        for b in Ndsr.blobs() {
            assert_eq!(b.dispatch, 0);
        }
    }

    #[test]
    fn the_blob_length_follows_the_network() {
        let p = push(1536, 864, 1920, 1080);
        let s = shape_of(&info("aaaa", &p, Format::Rgba8Unorm, [1920, 1080]));
        assert_eq!(s.blobs[0].len, 0x3000);
        assert_eq!(s.refusal, None);
        let s = shape_of(&info("bbbb", &p, Format::Rg11B10Float, [1920, 1080]));
        assert_eq!(s.blobs[0].len, 0x1800);
        assert_eq!(s.refusal, None);
    }

    #[test]
    fn each_size_and_network_gets_its_own_tag() {
        let p1 = push(1536, 864, 1920, 1080);
        let p2 = push(1280, 720, 1920, 1080);
        let tag = |key, p: &[u8], out| shape_of(&info(key, p, out, [1920, 1080])).tag;
        let a = tag("aaaa", &p1, Format::Rgba8Unorm);
        assert_eq!(a, tag("aaaa", &p1, Format::Rgba8Unorm), "stable");
        assert_ne!(a, tag("aaaa", &p2, Format::Rgba8Unorm), "input size");
        assert_ne!(a, tag("bbbb", &p1, Format::Rg11B10Float), "network");
        assert_eq!(&a[2..6], &[1536, 864, 1920, 1080]);
        // The words the kernel does not read do not make a new plan; the strength does.
        let mut q = p1.clone();
        q[0] ^= 0xff;
        q[12 * 4] ^= 0xff;
        assert_eq!(a, tag("aaaa", &q, Format::Rgba8Unorm));
        q[18 * 4] ^= 0x01;
        assert_ne!(a, tag("aaaa", &q, Format::Rgba8Unorm));
    }

    #[test]
    fn the_formats_must_be_the_variants() {
        let p = push(1536, 864, 1920, 1080);
        // Variant A writes 8 bit; a 10 bit image is variant B's.
        let s = shape_of(&info("aaaa", &p, Format::Rgb10A2Unorm, [1920, 1080]));
        assert!(s.refusal.is_some_and(|r| r.contains("img1")));
        // The capture still gets the tag.
        assert_eq!(s.tag.len(), 8);
        let mut i = info("aaaa", &p, Format::Rgba8Unorm, [1920, 1080]);
        i.bound[0].format = Format::Rgba8Unorm;
        assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("tex0")));
    }

    #[test]
    fn a_run_without_pictures_or_push_is_refused() {
        let p = push(1536, 864, 1920, 1080);
        let mut i = info("aaaa", &p, Format::Rgba8Unorm, [1920, 1080]);
        i.bound.pop();
        assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("img1")));
        let mut i = info("aaaa", &p, Format::Rgba8Unorm, [1920, 1080]);
        i.pushes = vec![None];
        assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("push")));
        let i = info("zzzz", &p, Format::Rgba8Unorm, [1920, 1080]);
        assert!(shape_of(&i).refusal.is_some_and(|r| r.contains("zzzz")));
    }

    #[test]
    fn the_sizes_come_from_the_push_words_when_they_fit() {
        let i = bound(INPUT, 1536, 864, Format::Rgba16Float);
        let o = bound(OUTPUT, 1920, 1080, Format::Rgba8Unorm);
        let p = push(1280, 720, 1920, 1080);
        assert_eq!(plan_sizes(&p, &i, &o), ([1280, 720], [1920, 1080]));
        // Larger than the picture, or no words: the pictures' sizes.
        let big = push(2000, 720, 1920, 1080);
        assert_eq!(plan_sizes(&big, &i, &o).0, [1536, 864]);
        assert_eq!(plan_sizes(&[], &i, &o), ([1536, 864], [1920, 1080]));
    }

    #[test]
    fn the_weights_and_the_plan_follow_the_network_and_the_sizes() {
        let n = Ndsr;
        let p = push(1280, 720, 1920, 1080);
        let i = info("aaaa", &p, Format::Rgba8Unorm, [1920, 1080]);
        let s = n.shape(&i);
        assert!(n.accepts_in(&[&[0; 0x3000]], &s, &i));
        assert!(!n.accepts_in(&[&[0; 0x1800]], &s, &i));
        assert!(!n.accepts_in(&[], &s, &i));
        let plan = n
            .plan_in(&[&[0; 0x3000]], &s, &i)
            .map_err(|e| e.to_string());
        let init = plan.ok().and_then(|p| p.buffers[0].init.clone());
        let sizes: Vec<u32> = init
            .iter()
            .flat_map(|b| b.chunks(4))
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        assert_eq!(sizes, [1280, 720, 1920, 1080]);
        assert!(n.plan_in(&[], &s, &i).is_err());
        // The static interface is not usable: the plan needs the sizes.
        assert!(!n.accepts(&[&[0; 0x3000]]));
        assert!(n.plan(&[&[0; 0x3000]]).is_err());
    }
}
