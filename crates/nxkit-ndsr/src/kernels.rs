//! The plan of the network: the packed weights and the one dispatch of the kernel in [`crate::msl::SOURCE`], for the
//! network program of `docs/plans/ndsr-native.md` section 7 (stage 1 of `docs/plans/ndsr-impl.md`: the SMAA passes stay
//! the game's).
//!
//! There is one kernel per network program of the vr pack: five input scales (`in` = 10, 8, 7, 6, 4 of the 12-pixel
//! grid, ratios 1.2, 1.5, 12/7, 2, 3) times three output variants (A: sRGB 8 bit, B: linear 10 bit, C: R11G11B10F). The
//! specialisation is by C++ template in the MSL (the plan names a function, it cannot carry function constants), and
//! [`KERNELS`] repeats the instantiations' parameters; a test compares the two.
//!
//! The kernel reads the sizes, the output offset, the unsharp strength and the dither switch from the game's push block
//! (bank 24), so one plan serves every input and output size of its tile shape; only the threadgroup count depends on
//! the input size.
//!
//! The vr_light programs have kernels of their own ([`light`], section 15 of the notes). The agl_ndsr input (f32 luma,
//! graphics SMAA) is open, see the end of `docs/plans/ndsr-native.md` section 13.

use crate::net::{Net, Pack};
use crate::pack;
use crate::plan::{Binding, Buffer, GameResource, Plan, Source, Step};

/// The texture the network reads: the neighbourhood blending's output (the game's tex0, with its sampler).
pub const TEX0: GameResource = GameResource::Texture(0);
/// The compute push block (bank 24) of the network dispatch.
pub const PUSH: GameResource = GameResource::Push(0);
/// The image the network writes (the game's img1): the result of the run.
pub const OUT: GameResource = GameResource::Output;

pub use crate::net::Variant;

pub mod light;

/// One network program of the vr pack: the parameters of its instantiation in `ndsr.metal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelSpec {
    /// The kernel's function name.
    pub name: &'static str,
    /// Input pixels per patch step (`in`; the grid is 12 cells per patch, so the ratio is `12 / in`).
    pub in_px: u32,
    /// The output encoding.
    pub variant: Variant,
    /// Input pixels per threadgroup in x and y (config bytes `b0`, `b1`).
    pub step: [u32; 2],
    /// The ratio immediate as f32 bits.
    pub ratio_bits: u32,
    /// The cell offset immediates `s`, `h`, `h1` of x and `h1` of y, as f32 bits (section 7.4).
    pub cell: [u32; 4],
    /// Whether the rows use the odd-row form of `ty` (all but the 1.2 programs).
    pub ty_odd: bool,
    /// Simdgroups of a threadgroup (`NSG` of the instantiation); the threadgroup has 32 times as many threads. Tuned
    /// per kernel on the full-size picture: more simdgroups hide the latency of the window, the cell samples and the
    /// barriers, until the registers per thread suffer.
    pub simdgroups: u32,
    /// Patches a simdgroup takes at once (`NB`): a weight tile loaded serves that many patches.
    pub patch_group: u32,
}

impl KernelSpec {
    /// Patches of a threadgroup along x and y.
    #[must_use]
    pub const fn patches(&self) -> [u32; 2] {
        [
            (self.step[0] + 1) / self.in_px,
            (self.step[1] + 1) / self.in_px,
        ]
    }

    /// The layers of the network (of 6) staged in threadgroup memory: as many as fit in the 32 KB budget next to the
    /// grid, the input window and the per-simdgroup result scratch (a mirror of `NST` in `ndsr_main`).
    #[must_use]
    pub const fn staged_layers(&self) -> u32 {
        let base = self.threadgroup_base();
        if base + 2048 > 32768 - 256 {
            0
        } else {
            let n = (32768 - 256 - base) / 2048;
            if n > 6 { 6 } else { n }
        }
    }

    /// The threads of a threadgroup.
    #[must_use]
    pub const fn threads(&self) -> u32 {
        32 * self.simdgroups
    }

    /// The threadgroup memory without the staged weights: grid, window, result scratch.
    const fn threadgroup_base(&self) -> u32 {
        let [npx, npy] = self.patches();
        let grid = 144 * npx * npy;
        let win = (self.in_px * (npx - 1) + 16) * (self.in_px * (npy - 1) + 16);
        (grid + win + self.simdgroups * 256) * 2
    }

    /// The threadgroup memory the kernel declares, in bytes: the grid, the input window, the per-simdgroup result
    /// scratch and the staged weights (a mirror of `ndsr_main`).
    #[must_use]
    pub const fn threadgroup_bytes(&self) -> u32 {
        let staged = self.staged_layers();
        // with nothing staged the kernel still declares 8 halves
        self.threadgroup_base() + if staged == 0 { 16 } else { 2048 * staged }
    }

    /// The threadgroups for an input of `in_size` pixels: the tile rule of section 7.6 needs exactly these.
    #[must_use]
    pub const fn groups(&self, in_size: [u32; 2]) -> [u32; 3] {
        [
            in_size[0].div_ceil(self.step[0]),
            in_size[1].div_ceil(self.step[1]),
            1,
        ]
    }
}

const fn spec(
    name: &'static str,
    in_px: u32,
    variant: Variant,
    step: [u32; 2],
    (ratio_bits, cell): (u32, [u32; 4]),
    ty_odd: bool,
    shape: [u32; 2],
) -> KernelSpec {
    KernelSpec {
        name,
        in_px,
        variant,
        step,
        ratio_bits,
        cell,
        ty_odd,
        simdgroups: shape[0],
        patch_group: shape[1],
    }
}

const R10: u32 = 0x3f99_999a;
const R8: u32 = 0x3fc0_0000;
const R7: u32 = 0x3fdb_6db8;
const R6: u32 = 0x4000_0000;
const R4: u32 = 0x4040_0000;
const C10: [u32; 4] = [0x3f55_5556, 0x3ed5_5556, 0x3fa0_0000, 0];
const C8: [u32; 4] = [0x3f2a_aaab, 0x3eaa_aaab, 0x3f80_0000, 0x3f80_0000];
const C7: [u32; 4] = [0x3f15_5556, 0x3e95_5556, 0x3f60_0000, 0x3f60_0001];
const C6: [u32; 4] = [0x3f00_0000, 0x3e80_0000, 0x3f40_0000, 0x3f40_0000];
const C4: [u32; 4] = [0x3eaa_aaab, 0x3e2a_aaab, 0x3f00_0000, 0x3f00_0000];

use Variant::{A, B, C};

/// The 15 network programs of the vr pack (ndsr-native.md section 4; the tile is the config bytes `b0 b1`).
pub const KERNELS: [KernelSpec; 15] = [
    spec("ndsr_in10_a", 10, A, [39, 29], (R10, C10), false, [12, 1]),
    spec("ndsr_in10_b", 10, B, [39, 29], (R10, C10), false, [12, 1]),
    spec("ndsr_in10_c", 10, C, [39, 39], (R10, C10), false, [16, 1]),
    spec("ndsr_in8_a", 8, A, [31, 31], (R8, C8), true, [16, 1]),
    spec("ndsr_in8_b", 8, B, [31, 31], (R8, C8), true, [16, 1]),
    spec("ndsr_in8_c", 8, C, [31, 31], (R8, C8), true, [16, 1]),
    spec("ndsr_in7_a", 7, A, [27, 27], (R7, C7), true, [16, 1]),
    spec("ndsr_in7_b", 7, B, [55, 41], (R7, C7), true, [8, 1]),
    spec("ndsr_in7_c", 7, C, [55, 20], (R7, C7), true, [12, 2]),
    spec("ndsr_in6_a", 6, A, [23, 23], (R6, C6), true, [16, 1]),
    spec("ndsr_in6_b", 6, B, [47, 23], (R6, C6), true, [12, 1]),
    spec("ndsr_in6_c", 6, C, [47, 23], (R6, C6), true, [12, 1]),
    spec("ndsr_in4_a", 4, A, [31, 23], (R4, C4), true, [8, 3]),
    spec("ndsr_in4_b", 4, B, [31, 23], (R4, C4), true, [8, 3]),
    spec("ndsr_in4_c", 4, C, [31, 23], (R4, C4), true, [8, 3]),
];

/// The kernel of an input scale (`in` = 10, 8, 7, 6, 4) and a variant.
#[must_use]
pub fn find(in_px: u32, variant: Variant) -> Option<&'static KernelSpec> {
    KERNELS
        .iter()
        .find(|k| k.in_px == in_px && k.variant == variant)
}

/// The plan of a network of the table ([`crate::fingerprint::find`]) for an input of `in_size` pixels and an output of
/// `out_size`. The kernel is the one of the network's input scale and variant ([`find`]); `blob` is the game's weight
/// blob (ssbo 2 of the dispatch) and `push` the dispatch's push block, which the kernel reads from the game's bank at
/// run time, so it only has to be there.
///
/// # Errors
///
/// When the network is of the agl pack (its input is open), the push block is missing, or [`plan_kernel`] or
/// [`light::plan`] refuses.
pub fn plan(
    net: &Net,
    blob: &[u8],
    in_size: [u32; 2],
    out_size: [u32; 2],
    push: &[u8],
) -> Result<Plan, String> {
    if push.is_empty() {
        return Err(format!("{}: no push block", net.key));
    }
    match net.pack {
        Pack::Vr => {}
        Pack::VrLight => return light::plan(net, blob, in_size, push),
        Pack::Agl => {
            return Err(format!(
                "{}: the {} pack is not run natively yet (vr and vr_light only)",
                net.key,
                net.pack.name()
            ));
        }
    }
    let kernel = find(u32::from(net.in_px), net.variant).ok_or_else(|| {
        format!(
            "{}: no kernel for in {} {:?}",
            net.key, net.in_px, net.variant
        )
    })?;
    plan_kernel(kernel, blob, in_size, out_size)
}

/// The plan of one network program for an input of `in_size` pixels and an output of `out_size`: the packed weights as
/// the only buffer and one dispatch. `blob` is the game's weight blob (ssbo 2 of the network dispatch): the vr pack's, 0x3000 bytes.
///
/// # Errors
///
/// When a size is zero, or `blob` is not the six layers of the vr pack.
pub fn plan_kernel(
    kernel: &KernelSpec,
    blob: &[u8],
    in_size: [u32; 2],
    out_size: [u32; 2],
) -> Result<Plan, String> {
    if in_size.contains(&0) || out_size.contains(&0) {
        return Err(format!(
            "sizes {in_size:?} -> {out_size:?} have a zero side"
        ));
    }
    if blob.len() != pack::BLOB_BYTES {
        return Err(format!(
            "a vr blob is {:#x} bytes, expected {:#x}",
            blob.len(),
            pack::BLOB_BYTES
        ));
    }
    let packed = pack::from_vr(blob)?;
    let bind = |index, source, writes| Binding {
        index,
        source,
        writes,
    };
    Ok(Plan {
        buffers: vec![Buffer {
            name: "ndsr.weights",
            bytes: packed.len() as u64,
            init: Some(packed),
        }],
        steps: vec![Step {
            kernel: kernel.name,
            groups: kernel.groups(in_size),
            threads: [kernel.threads(), 1, 1],
            threadgroup_bytes: 0,
            buffers: vec![
                bind(0, Source::Buffer(0), false),
                bind(1, Source::Game(PUSH), false),
            ],
            textures: vec![
                bind(0, Source::Game(TEX0), false),
                bind(1, Source::Game(OUT), true),
            ],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msl::SOURCE;
    use crate::net::NETS;

    fn blob() -> Vec<u8> {
        (0..pack::BLOB_BYTES / 2)
            .flat_map(|i| (0x3000 + (i % 0x400) as u16).to_le_bytes())
            .collect()
    }

    /// The numbers of a `NDSR_KERNEL(...)` line of the MSL, name first.
    fn instantiations() -> Vec<(String, Vec<u64>)> {
        SOURCE
            .lines()
            .filter_map(|l| l.strip_prefix("NDSR_KERNEL(\"")?.strip_suffix(')'))
            .map(|l| {
                let (name, rest) = l.split_once("\", ").expect("name");
                let nums = rest
                    .split(", ")
                    .map(|v| match v {
                        "true" => 1,
                        "false" => 0,
                        _ => {
                            let v = v.trim_end_matches('u');
                            v.strip_prefix("0x").map_or_else(
                                || v.parse().expect("number"),
                                |h| u64::from_str_radix(h, 16).expect("hex"),
                            )
                        }
                    })
                    .collect();
                (name.to_string(), nums)
            })
            .collect()
    }

    /// The Rust table and the MSL instantiations are the same 15 programs.
    #[test]
    fn table_matches_the_msl_instantiations() {
        let inst = instantiations();
        assert_eq!(inst.len(), KERNELS.len());
        for (k, (name, n)) in KERNELS.iter().zip(&inst) {
            assert_eq!(&k.name, name);
            let variant = match k.variant {
                Variant::A => 0,
                Variant::B => 1,
                Variant::C => 2,
            };
            let want = [
                u64::from(k.in_px),
                u64::from(k.step[0]),
                u64::from(k.step[1]),
                variant,
                u64::from(k.ratio_bits),
                u64::from(k.cell[0]),
                u64::from(k.cell[1]),
                u64::from(k.cell[2]),
                u64::from(k.cell[3]),
                u64::from(k.ty_odd),
                u64::from(k.simdgroups),
                u64::from(k.patch_group),
            ];
            assert_eq!(n, &want, "{name}");
        }
    }

    /// Names are unique and follow the scale and variant; the tile covers whole patches.
    #[test]
    fn table_is_consistent() {
        for (i, k) in KERNELS.iter().enumerate() {
            assert!(KERNELS[..i].iter().all(|o| o.name != k.name), "{}", k.name);
            assert_eq!(find(k.in_px, k.variant), Some(k));
            let [npx, npy] = k.patches();
            assert!(npx >= 1 && npy >= 1, "{}", k.name);
            assert_eq!(npx * k.in_px, k.step[0] + 1, "{}", k.name);
            assert_eq!(npy * k.in_px, k.step[1] + 1, "{}", k.name);
            // the network maps in to 12 cells: ratio 12 / in
            let ratio = f32::from_bits(k.ratio_bits);
            assert!((ratio - 12.0 / k.in_px as f32).abs() < 1e-6, "{}", k.name);
            assert_eq!(
                12 * npx % 6,
                0,
                "the unsharp pass takes blocks of 6 columns"
            );
            assert!(k.threadgroup_bytes() <= 32768, "{}", k.name);
            assert!((1..=16).contains(&k.simdgroups), "{}", k.name);
            assert!((1..=4).contains(&k.patch_group), "{}", k.name);
        }
        assert_eq!(find(9, Variant::A), None);
    }

    #[test]
    fn plan_shape() {
        let k = find(10, Variant::A).expect("kernel");
        let p = plan_kernel(k, &blob(), [1536, 864], [1920, 1080]).expect("plan");
        assert_eq!(p.buffers.len(), 1);
        assert_eq!(p.buffers[0].bytes, 0x3000);
        assert_eq!(p.buffers[0].init.as_ref().map(Vec::len), Some(0x3000));
        let [s] = &p.steps[..] else {
            panic!("one step")
        };
        assert_eq!(s.kernel, "ndsr_in10_a");
        assert_eq!(s.groups, [1536u32.div_ceil(39), 864u32.div_ceil(29), 1]);
        assert_eq!(s.threads, [k.threads(), 1, 1]);
        for b in s.buffers.iter().chain(&s.textures) {
            if let Source::Buffer(i) = b.source {
                assert!(i < p.buffers.len());
            }
        }
        assert_eq!(s.textures[1].source, Source::Game(GameResource::Output));
        assert!(s.textures[1].writes && !s.textures[0].writes);
    }

    /// Every vr network of the table has a kernel of its scale and variant; agl refuses with a reason.
    #[test]
    fn plan_of_a_net() {
        let push = [0u8; 80];
        for n in &NETS {
            let r = plan(n, &blob(), [64, 64], [128, 128], &push);
            if n.pack == Pack::VrLight {
                // the light plan wants its blob length and 29 push words with the game's tile (light.rs)
                assert!(r.is_err(), "{}", n.key);
            } else if n.pack == Pack::Vr {
                let p = r.expect(n.key);
                let k = find(u32::from(n.in_px), n.variant).expect("kernel");
                assert_eq!(p.steps[0].kernel, k.name);
                assert_eq!(
                    k.step,
                    [u32::from(n.step_x), u32::from(n.step_y)],
                    "{}",
                    n.key
                );
            } else {
                assert!(r.is_err_and(|e| e.contains("vr_light only")), "{}", n.key);
            }
        }
        let n = NETS.iter().find(|n| n.pack == Pack::Vr).expect("vr");
        assert!(plan(n, &blob(), [64, 64], [128, 128], &[]).is_err());
    }

    #[test]
    fn plan_errors() {
        let k = find(8, Variant::B).expect("kernel");
        assert!(plan_kernel(k, &blob()[..100], [64, 64], [128, 128]).is_err());
        assert!(plan_kernel(k, &blob(), [0, 64], [128, 128]).is_err());
        let short = &blob()[..pack::SHORT_BLOB_BYTES];
        assert!(plan_kernel(k, short, [64, 64], [128, 128]).is_err());
        // the vr repack differs from the identity
        let v = plan_kernel(k, &blob(), [64, 64], [128, 128]).expect("plan");
        assert_ne!(v.buffers[0].init.as_deref(), Some(&blob()[..]));
    }
}
