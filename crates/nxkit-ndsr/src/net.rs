//! The 52 network programs of the NDSR packs as a table.
//!
//! Everything here is read from the packs and the shader map (V in `docs/plans/ndsr-native.md`, sections 3 to 5 and
//! 7.4): the key of the program, the pack it sits in, the ratio and the tile of its config, the weight blob it reads and
//! the blob's SHA-256. For the vr programs it also holds the immediates the bit-exact path needs (the ratio bits and the
//! cell-offset constants of section 7.4). The order of [`NETS`] is the order of the programs in their pack, so a test
//! can walk a pack and the table side by side.

/// Bytes of one network layer: four 16x16 f16 matrices.
pub const LAYER_BYTES: usize = 0x800;
/// Edge of a layer's matrices.
pub const N: usize = 16;

/// The three NDSR packs of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pack {
    /// `agl_ndsr.pck`: graphics SMAA and fixed-ratio networks (22 programs).
    Agl,
    /// `agl_ndsr_vr.pck`: compute SMAA and variable-ratio networks (15 programs).
    Vr,
    /// `agl_ndsr_vr_light.pck`: the same with cheaper networks (15 programs).
    VrLight,
}

impl Pack {
    /// All packs, in the order of the tables.
    pub const ALL: [Pack; 3] = [Pack::Agl, Pack::Vr, Pack::VrLight];

    /// The archive member name.
    pub const fn name(self) -> &'static str {
        match self {
            Pack::Agl => "agl_ndsr.pck",
            Pack::Vr => "agl_ndsr_vr.pck",
            Pack::VrLight => "agl_ndsr_vr_light.pck",
        }
    }

    /// Length of the pack in bytes.
    pub const fn byte_len(self) -> usize {
        match self {
            Pack::Agl => 0xf8000,
            Pack::Vr => 0x99000,
            Pack::VrLight => 0x86000,
        }
    }

    /// SHA-256 of the pack (hex, lower case).
    pub const fn sha256(self) -> &'static str {
        match self {
            Pack::Agl => "47f6f511247fdee2ed654b591cf2e32f484dfb13fcf882977dcfcd4f8d1ea9f4",
            Pack::Vr => "191ea1989c1b6d5b85de4d1a549c2c63ec3fa87d0a39435d4f0c401e3ac3d029",
            Pack::VrLight => "7780c292095d92aef17a8e03434bdcbb8459469f635988e025e250429ffbdc18",
        }
    }

    /// The number of 8-byte entries in the pack's table.
    pub const fn entry_count(self) -> u32 {
        match self {
            Pack::Agl => 0x9c,
            Pack::Vr => 0x4e,
            Pack::VrLight => 0x66,
        }
    }

    /// The fourth byte of every network config (`b3`, section 5): its meaning is unknown.
    pub const fn config_b3(self) -> u8 {
        match self {
            Pack::Agl => 0x01,
            Pack::Vr => 0x22,
            Pack::VrLight => 0x12,
        }
    }

    /// The header words at `+4` to `+0x10`: version, the second word, the entry count and the word at `+0x10`. All
    /// three packs have version `0x00010100` and second word `0x00020302`.
    pub const fn header_words(self) -> [u32; 4] {
        let w4 = match self {
            Pack::Agl => 0x0002_0c00,
            Pack::Vr => 0x0602_0c00,
            Pack::VrLight => 0x0502_0c00,
        };
        [0x0001_0100, 0x0002_0302, self.entry_count(), w4]
    }
}

/// The output encoding a program writes, and the format of the image it expects (section 4).
///
/// The vr programs: `A` writes sRGB 8-bit (dither 1/255), `B` linear 10-bit (dither 1/1023), `C` linear R11G11B10F.
/// The agl programs: `A` reads linear colour, `B` adds an sRGB decode of the input. For the vr_light pack the variant
/// follows the entry order of the pack (I): where two entries share one program, the program takes the letter of its
/// first entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// See the type's documentation.
    A,
    /// See the type's documentation.
    B,
    /// See the type's documentation.
    C,
}

/// The immediates of a vr program that the bit-exact path needs (sections 4 and 7.4). They are shared by the three
/// variants of one ratio, except [`VrConsts::y_every_row`], which is true for the three 1.2 programs only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VrConsts {
    /// The ratio `R` the shader multiplies by, as f32 bits. For 12/7 it is one ulp above `f32(12/7)`.
    pub ratio_bits: u32,
    /// `s`, f32 bits: the cell spacing in input pixels.
    pub step_bits: u32,
    /// `h`, f32 bits: the offset of the first cell centre.
    pub half_bits: u32,
    /// `h1` of the columns, f32 bits: the offset of odd columns.
    pub odd_x_bits: u32,
    /// `h1` of the rows, f32 bits: the offset of odd rows (unused when [`VrConsts::y_every_row`]).
    pub odd_y_bits: u32,
    /// The rows use `fma(i, s, h)` for every row, not the even/odd form of the columns.
    pub y_every_row: bool,
}

const fn vr_consts(in_px: u8) -> Option<VrConsts> {
    Some(match in_px {
        10 => VrConsts {
            ratio_bits: 0x3f99_999a,
            step_bits: 0x3f55_5556,
            half_bits: 0x3ed5_5556,
            odd_x_bits: 0x3fa0_0000,
            odd_y_bits: 0x3fa0_0000,
            y_every_row: true,
        },
        8 => VrConsts {
            ratio_bits: 0x3fc0_0000,
            step_bits: 0x3f2a_aaab,
            half_bits: 0x3eaa_aaab,
            odd_x_bits: 0x3f80_0000,
            odd_y_bits: 0x3f80_0000,
            y_every_row: false,
        },
        7 => VrConsts {
            ratio_bits: 0x3fdb_6db8,
            step_bits: 0x3f15_5556,
            half_bits: 0x3e95_5556,
            odd_x_bits: 0x3f60_0000,
            odd_y_bits: 0x3f60_0001,
            y_every_row: false,
        },
        6 => VrConsts {
            ratio_bits: 0x4000_0000,
            step_bits: 0x3f00_0000,
            half_bits: 0x3e80_0000,
            odd_x_bits: 0x3f40_0000,
            odd_y_bits: 0x3f40_0000,
            y_every_row: false,
        },
        4 => VrConsts {
            ratio_bits: 0x4040_0000,
            step_bits: 0x3eaa_aaab,
            half_bits: 0x3e2a_aaab,
            odd_x_bits: 0x3f00_0000,
            odd_y_bits: 0x3f00_0000,
            y_every_row: false,
        },
        _ => return None,
    })
}

/// One weight blob of a pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    /// The pack that holds it.
    pub pack: Pack,
    /// Offset of the weights in the pack.
    pub offset: u32,
    /// Length in bytes: 0x3000 (6 layers) or 0x1800 (3 layers).
    pub len: u32,
    /// SHA-256 (hex, lower case).
    pub sha256: &'static str,
}

impl Blob {
    const fn new(pack: Pack, offset: u32, len: u32, sha256: &'static str) -> Blob {
        Blob {
            pack,
            offset,
            len,
            sha256,
        }
    }

    /// The number of layers: the length over [`LAYER_BYTES`].
    pub const fn layers(&self) -> usize {
        self.len as usize / LAYER_BYTES
    }
}

/// One network program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Net {
    /// The game's shader key (16 hex characters, lower case).
    pub key: &'static str,
    /// The pack it sits in.
    pub pack: Pack,
    /// Which of its pack's output variants it is.
    pub variant: Variant,
    /// Input pixels per patch edge: the network maps the centre `in x in` pixels of a 16x16 patch ...
    pub in_px: u8,
    /// ... to an `out x out` block of the grid. The ratio is `out / in`.
    pub out_px: u8,
    /// Input pixels per threadgroup in x (`b0`).
    pub step_x: u8,
    /// Input pixels per threadgroup in y (`b1`).
    pub step_y: u8,
    /// Index into [`BLOBS`].
    pub blob: u8,
    /// The immediates of a vr program, `None` for the other packs.
    pub vr: Option<VrConsts>,
}

impl Net {
    #[allow(clippy::too_many_arguments)]
    const fn new(
        key: &'static str,
        pack: Pack,
        variant: Variant,
        in_px: u8,
        out_px: u8,
        step_x: u8,
        step_y: u8,
        blob: u8,
    ) -> Net {
        let vr = match pack {
            Pack::Vr => vr_consts(in_px),
            _ => None,
        };
        Net {
            key,
            pack,
            variant,
            in_px,
            out_px,
            step_x,
            step_y,
            blob,
            vr,
        }
    }

    /// The look-up by key (any case). `None` for a key that is not a network of the table.
    pub fn by_key(key: &str) -> Option<(usize, &'static Net)> {
        NETS.iter()
            .enumerate()
            .find(|(_, n)| n.key.eq_ignore_ascii_case(key))
    }

    /// The weight blob of the program.
    pub fn blob(&self) -> &'static Blob {
        &BLOBS[usize::from(self.blob)]
    }

    /// The ratio `out / in`.
    pub fn ratio(&self) -> f64 {
        f64::from(self.out_px) / f64::from(self.in_px)
    }

    /// The config of the pack entry: `b0 b1 b2 b3`, with `b2 = (out - 1) << 4 | (in - 1)` (section 5).
    pub fn config(&self) -> [u8; 4] {
        let b2 = ((self.out_px - 1) << 4) | (self.in_px - 1);
        [self.step_x, self.step_y, b2, self.pack.config_b3()]
    }

    /// Patches per threadgroup in x: `(b0 + 1) / in` (section 5).
    pub fn patches_x(&self) -> usize {
        (usize::from(self.step_x) + 1) / usize::from(self.in_px)
    }

    /// Patches per threadgroup in y.
    pub fn patches_y(&self) -> usize {
        (usize::from(self.step_y) + 1) / usize::from(self.in_px)
    }

    /// The grid of a threadgroup in cells: `out * npx` by `out * npy`.
    pub fn grid_size(&self) -> (usize, usize) {
        let out = usize::from(self.out_px);
        (out * self.patches_x(), out * self.patches_y())
    }

    /// The margin of the window left of and above a tile: `(16 - in) / 2` (section 7).
    pub fn margin(&self) -> usize {
        (N - usize::from(self.in_px)) / 2
    }

    /// The input window of a threadgroup in texels: `in (np - 1) + 16` in each direction (section 7.1).
    pub fn window_size(&self) -> (usize, usize) {
        let i = usize::from(self.in_px);
        (
            i * (self.patches_x() - 1) + N,
            i * (self.patches_y() - 1) + N,
        )
    }
}

use Pack::{Agl, Vr, VrLight};
use Variant::{A, B, C};

#[cfg(test)]
mod tests;

/// The weight blobs of the three packs, in pack order. The vr_light pack holds its own copies of the vr blobs.
pub static BLOBS: [Blob; 23] = [
    Blob::new(
        Agl,
        0xd5f00,
        0x3000,
        "1b394dd97b2dd4512354c4a33d631a86e88a0b63c37cb17188c5f6370bcfdc63",
    ),
    Blob::new(
        Agl,
        0xd8f00,
        0x3000,
        "314cedcfe9705b9457796f822d3e1813bba26755141932156a6798ec8ddaaa45",
    ),
    Blob::new(
        Agl,
        0xdbf00,
        0x1800,
        "f424fa9292db8272384299b7e0e4bc265113a1a54ab2eb043595a7cd08466eed",
    ),
    Blob::new(
        Agl,
        0xdd700,
        0x3000,
        "d43c42a8f1122158ca309c39d170a3469c628d86545dc084d3e2ac8edb212b3b",
    ),
    Blob::new(
        Agl,
        0xe0700,
        0x3000,
        "7e7d52b445e8366c04f2a700f4cc10ac54bf23066aa07e1f6c452fec8e5cc199",
    ),
    Blob::new(
        Agl,
        0xe3700,
        0x3000,
        "84d5455e137d816eaa08dad76f51b5a95e209ec02f75c5231f7f515b62efb75e",
    ),
    Blob::new(
        Agl,
        0xe6700,
        0x3000,
        "7e5ceef2b1b4f518ca087abf9ff0d7b5c19e9d53840d27ddbdd78ccad1ac71cf",
    ),
    Blob::new(
        Agl,
        0xe9700,
        0x3000,
        "d7960412cf3861849e185fb99898b5b02aa6f77064f653a63ef3be9deaf59a7b",
    ),
    Blob::new(
        Agl,
        0xec700,
        0x3000,
        "89f9eca53909586c4354f36807c7425ed14e72717e2ce05a2ea9710b775113e5",
    ),
    Blob::new(
        Agl,
        0xef700,
        0x3000,
        "de4ea9bce0cf766756fe26b4280c99a2c6c9456b4a462f3bc507d7d1c22d6b43",
    ),
    Blob::new(
        Agl,
        0xf2700,
        0x3000,
        "2402e74a22ff78ada5cb261dddaccd3d0c2db6adfcfdbfe47f3ac5a5a1b04cb8",
    ),
    Blob::new(
        Vr,
        0x88710,
        0x3000,
        "b82b7ec1326f05d6d0ce143b815de0779849ebbb4d40f77c0fba7821979c39b0",
    ),
    Blob::new(
        Vr,
        0x8b710,
        0x3000,
        "94adcd47f0648e3c7e53cb88bfdd4366426145188a2301fa8acf9b68a05016fe",
    ),
    Blob::new(
        Vr,
        0x8e710,
        0x3000,
        "f88305d44dfb27d6e8a64932c3188f62972bb4375e029f65020ecdbaee83d3b6",
    ),
    Blob::new(
        Vr,
        0x91710,
        0x3000,
        "e960abf906280cd5a3942987f741a8b5bb30dc26da9bf2c22fc029f2cdaf93fe",
    ),
    Blob::new(
        Vr,
        0x94710,
        0x3000,
        "5de06ec356888082e6f04e266767894b30dce661872146c18c28efde6fbda30a",
    ),
    Blob::new(
        VrLight,
        0x70a10,
        0x3000,
        "b82b7ec1326f05d6d0ce143b815de0779849ebbb4d40f77c0fba7821979c39b0",
    ),
    Blob::new(
        VrLight,
        0x73a10,
        0x1800,
        "45209a676f28e76a8709f9c4d72d8ecb003cd11fb0a893c4e57c26276eb091b4",
    ),
    Blob::new(
        VrLight,
        0x75210,
        0x3000,
        "94adcd47f0648e3c7e53cb88bfdd4366426145188a2301fa8acf9b68a05016fe",
    ),
    Blob::new(
        VrLight,
        0x78210,
        0x3000,
        "f88305d44dfb27d6e8a64932c3188f62972bb4375e029f65020ecdbaee83d3b6",
    ),
    Blob::new(
        VrLight,
        0x7b210,
        0x3000,
        "e960abf906280cd5a3942987f741a8b5bb30dc26da9bf2c22fc029f2cdaf93fe",
    ),
    Blob::new(
        VrLight,
        0x7e210,
        0x3000,
        "8678cc5717d22839dbcd5f9c094ebe52137a83432a7ed888ebb5eeff47be9395",
    ),
    Blob::new(
        VrLight,
        0x81210,
        0x3000,
        "5de06ec356888082e6f04e266767894b30dce661872146c18c28efde6fbda30a",
    ),
];

/// The 52 network programs: 22 of `agl_ndsr.pck`, 15 of `agl_ndsr_vr.pck` and 15 of `agl_ndsr_vr_light.pck`, each
/// pack in the order of its programs.
pub static NETS: [Net; 52] = [
    Net::new("abef20ad0d9a9b12", Agl, A, 10, 12, 50, 40, 0),
    Net::new("725bcf5c3f4bb3f3", Agl, B, 10, 12, 40, 60, 0),
    Net::new("e9213c7fba8ccf93", Agl, A, 8, 10, 16, 64, 1),
    Net::new("87bdc29ac7ce9adb", Agl, B, 8, 10, 24, 64, 1),
    Net::new("aa1243f635baa8e8", Agl, A, 6, 8, 48, 12, 2),
    Net::new("54c6b94ddf539d9f", Agl, B, 6, 8, 48, 30, 2),
    Net::new("1de2a54f4b81e4e0", Agl, A, 8, 11, 24, 64, 3),
    Net::new("18f53257e4a46245", Agl, B, 8, 11, 24, 64, 3),
    Net::new("3fe6b091bb8c40ce", Agl, A, 8, 12, 64, 32, 4),
    Net::new("e16733ed9a14fc7a", Agl, B, 8, 12, 64, 32, 4),
    Net::new("fdeb898427db8562", Agl, A, 6, 10, 18, 48, 5),
    Net::new("a326e7353ec81e2c", Agl, B, 6, 10, 18, 48, 5),
    Net::new("4acef5afefba098b", Agl, A, 7, 12, 21, 28, 6),
    Net::new("c78798281d3519c1", Agl, B, 7, 12, 21, 28, 6),
    Net::new("37121447f861a7d3", Agl, A, 6, 11, 48, 18, 7),
    Net::new("add52a00627e7e84", Agl, B, 6, 11, 48, 18, 7),
    Net::new("fb5038f1befcd03b", Agl, A, 6, 12, 48, 24, 8),
    Net::new("fbe679b956a85e1f", Agl, B, 6, 12, 48, 24, 8),
    Net::new("196dfe02185a7518", Agl, A, 5, 11, 20, 15, 9),
    Net::new("0253bd17deceee79", Agl, B, 5, 11, 20, 30, 9),
    Net::new("028bd0b187f7342c", Agl, A, 4, 12, 32, 16, 10),
    Net::new("f54f031dbc6f65f5", Agl, B, 4, 12, 32, 16, 10),
    Net::new("14c10aa70b81f04e", Vr, A, 10, 12, 39, 29, 11),
    Net::new("f407ab4429b8eae6", Vr, B, 10, 12, 39, 29, 11),
    Net::new("caf3a6e1b2f70605", Vr, C, 10, 12, 39, 39, 11),
    Net::new("49e67caae643e42a", Vr, A, 8, 12, 31, 31, 12),
    Net::new("3bed6a425f72e4d6", Vr, B, 8, 12, 31, 31, 12),
    Net::new("7a5a51af95b27ae5", Vr, C, 8, 12, 31, 31, 12),
    Net::new("c16ce0c879c981a3", Vr, A, 7, 12, 27, 27, 13),
    Net::new("e30678db92cf66ff", Vr, B, 7, 12, 55, 41, 13),
    Net::new("4aa7eec58d2fa656", Vr, C, 7, 12, 55, 20, 13),
    Net::new("45f7364945aff920", Vr, A, 6, 12, 23, 23, 14),
    Net::new("17c6bd35f159a957", Vr, B, 6, 12, 47, 23, 14),
    Net::new("57cdcd5913bd385f", Vr, C, 6, 12, 47, 23, 14),
    Net::new("e4b49569547e3d47", Vr, A, 4, 12, 31, 23, 15),
    Net::new("f0c1f33f84b619dc", Vr, B, 4, 12, 31, 23, 15),
    Net::new("aec4dfce7ab3cefa", Vr, C, 4, 12, 31, 23, 15),
    Net::new("9327ea56aafa15b9", VrLight, A, 10, 12, 40, 40, 16),
    Net::new("8df148d58b7cbe8e", VrLight, B, 10, 12, 40, 40, 16),
    Net::new("927ceaf3cf1be240", VrLight, A, 6, 8, 48, 48, 17),
    Net::new("fa3033d05c0a41d6", VrLight, B, 6, 8, 48, 24, 17),
    Net::new("48d00da84b3484df", VrLight, A, 8, 12, 32, 32, 18),
    Net::new("aad7ea7e5c02f41d", VrLight, B, 8, 12, 32, 32, 18),
    Net::new("6160882513197af3", VrLight, A, 7, 12, 28, 28, 19),
    Net::new("b106afba1a18faa9", VrLight, B, 7, 12, 28, 28, 19),
    Net::new("2f9bca45ef842a3b", VrLight, A, 6, 12, 24, 24, 20),
    Net::new("04135bb812a0d66e", VrLight, B, 6, 12, 24, 24, 20),
    Net::new("d8fffc05afaaa981", VrLight, A, 5, 12, 20, 20, 21),
    Net::new("7c511cc23ff85d14", VrLight, B, 5, 12, 20, 20, 21),
    Net::new("bdb298c6aa71a298", VrLight, A, 4, 12, 16, 16, 22),
    Net::new("c8b20adc9d4978e0", VrLight, B, 4, 12, 16, 16, 22),
    Net::new("d99fbeeb53d38847", VrLight, C, 4, 12, 16, 16, 22),
];
