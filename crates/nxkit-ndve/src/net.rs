//! The layer table of the network: shapes, offsets and kinds, as constants.
//!
//! All numbers are from section 4 of `docs/plans/ndve-native.md` and are marked V there (read from the shader code and
//! the file bytes), except which tensor is the skip of an up layer, which is I (inferred, strongly supported).
//! Sizes are those of a 1920x1080 movie. The network works at any picture size that is a multiple of 8 in both
//! directions; [`Layer::in_size`] and [`Layer::out_size`] scale the table.

/// Length of the weight blob in bytes.
pub const BLOB_LEN: usize = 0x3c320;
/// Length of the resampler's weights in bytes: six blocks of [`RESAMPLER_BLOCK`] bytes.
pub const RESAMPLER_LEN: usize = 0x3000;
/// Bytes of one resampler block.
pub const RESAMPLER_BLOCK: usize = 0x800;
/// The movie picture width the table is written for.
pub const PICTURE_WIDTH: usize = 1920;
/// The movie picture height the table is written for.
pub const PICTURE_HEIGHT: usize = 1080;

/// What a layer computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 3x3 convolution, stride 1, zero padding. 9 taps.
    Conv3x3,
    /// 2x2 convolution, stride 2, no padding. 4 taps. Halves the size.
    Down2x2,
    /// 2x2 transposed convolution, stride 2. 4 taps. Doubles the size. The skip tensor is the start of every chain.
    Up2x2,
    /// The last layer: a 3x3 convolution without ReLU whose output is added to the picture (depth to space).
    Final,
}

impl Kind {
    /// The number of taps (kernel positions) of the kind.
    pub const fn taps(self) -> usize {
        match self {
            Kind::Conv3x3 | Kind::Final => 9,
            Kind::Down2x2 | Kind::Up2x2 => 4,
        }
    }
}

/// One layer of the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layer {
    /// 1 to 12. The dispatch of the layer sits at this index in the matched run (the colour conversion is index 0).
    pub number: usize,
    /// The first 8 hex characters of the game's shader key.
    pub key: &'static str,
    /// What the layer computes.
    pub kind: Kind,
    /// Input width in pixels (at 1920x1080). For layer 1 this is the packed size, 960.
    pub in_width: usize,
    /// Input height in pixels.
    pub in_height: usize,
    /// Input channels.
    pub in_channels: usize,
    /// Output width in pixels of the network tensor.
    pub out_width: usize,
    /// Output height in pixels of the network tensor.
    pub out_height: usize,
    /// Output channels of the network tensor.
    pub out_channels: usize,
    /// Offset of the weights `[tap][cin][cout]` (f16) in the blob.
    pub weight_offset: usize,
    /// Offset of the bias (f16, `out_channels` values) in the blob.
    pub bias_offset: usize,
    /// ReLU after the bias.
    pub relu: bool,
    /// The number of the layer whose output is the skip tensor (up layers only).
    pub skip: Option<usize>,
}

impl Layer {
    /// Bytes of the weights.
    pub const fn weight_bytes(&self) -> usize {
        self.kind.taps() * self.in_channels * self.out_channels * 2
    }

    /// Bytes of the bias values (without the padding).
    pub const fn bias_bytes(&self) -> usize {
        self.out_channels * 2
    }

    /// Input size for a picture of `width` x `height` (the table's size scaled).
    pub const fn in_size(&self, width: usize, height: usize) -> (usize, usize) {
        (
            self.in_width * width / PICTURE_WIDTH,
            self.in_height * height / PICTURE_HEIGHT,
        )
    }

    /// Output size for a picture of `width` x `height`.
    pub const fn out_size(&self, width: usize, height: usize) -> (usize, usize) {
        (
            self.out_width * width / PICTURE_WIDTH,
            self.out_height * height / PICTURE_HEIGHT,
        )
    }

    /// The weights of the layer in `blob`, or `None` when the blob is too short.
    pub fn weights<'a>(&self, blob: &'a [u8]) -> Option<&'a [u8]> {
        blob.get(self.weight_offset..self.weight_offset + self.weight_bytes())
    }

    /// The bias of the layer in `blob`, or `None` when the blob is too short.
    pub fn bias<'a>(&self, blob: &'a [u8]) -> Option<&'a [u8]> {
        blob.get(self.bias_offset..self.bias_offset + self.bias_bytes())
    }
}

const fn layer(
    number: usize,
    key: &'static str,
    kind: Kind,
    input: (usize, usize, usize),
    output: (usize, usize, usize),
    offsets: (usize, usize),
    skip: Option<usize>,
) -> Layer {
    Layer {
        number,
        key,
        kind,
        in_width: input.0,
        in_height: input.1,
        in_channels: input.2,
        out_width: output.0,
        out_height: output.1,
        out_channels: output.2,
        weight_offset: offsets.0,
        bias_offset: offsets.1,
        relu: !matches!(kind, Kind::Final),
        skip,
    }
}

/// The twelve layers, in the order the game runs them.
pub const LAYERS: [Layer; 12] = [
    layer(
        1,
        "ba079f68",
        Kind::Conv3x3,
        (960, 540, 16),
        (960, 540, 16),
        (0x0, 0x1200),
        None,
    ),
    layer(
        2,
        "a8b4fb48",
        Kind::Conv3x3,
        (960, 540, 16),
        (960, 540, 16),
        (0x1300, 0x2500),
        None,
    ),
    layer(
        3,
        "e2eebd7b",
        Kind::Down2x2,
        (960, 540, 16),
        (480, 270, 32),
        (0x2600, 0x3600),
        None,
    ),
    layer(
        4,
        "e6be82d8",
        Kind::Conv3x3,
        (480, 270, 32),
        (480, 270, 32),
        (0x3700, 0x7f00),
        None,
    ),
    layer(
        5,
        "9442573e",
        Kind::Down2x2,
        (480, 270, 32),
        (240, 135, 64),
        (0x8000, 0xc000),
        None,
    ),
    layer(
        6,
        "0a56002c",
        Kind::Conv3x3,
        (240, 135, 64),
        (240, 135, 64),
        (0xc100, 0x1e100),
        None,
    ),
    layer(
        7,
        "0a56002c",
        Kind::Conv3x3,
        (240, 135, 64),
        (240, 135, 64),
        (0x1e200, 0x30200),
        None,
    ),
    layer(
        8,
        "d15370a3",
        Kind::Up2x2,
        (240, 135, 64),
        (480, 270, 32),
        (0x30300, 0x34300),
        Some(4),
    ),
    layer(
        9,
        "e6be82d8",
        Kind::Conv3x3,
        (480, 270, 32),
        (480, 270, 32),
        (0x34400, 0x38c00),
        None,
    ),
    layer(
        10,
        "7f7ff0f3",
        Kind::Up2x2,
        (480, 270, 32),
        (960, 540, 16),
        (0x38d00, 0x39d00),
        Some(2),
    ),
    layer(
        11,
        "a8b4fb48",
        Kind::Conv3x3,
        (960, 540, 16),
        (960, 540, 16),
        (0x39e00, 0x3b000),
        None,
    ),
    layer(
        12,
        "5a7d69b0",
        Kind::Final,
        (960, 540, 16),
        (960, 540, 16),
        (0x3b100, 0x3c300),
        None,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_follow_each_other_padded_to_0x100() {
        let mut at = 0;
        for l in &LAYERS {
            assert_eq!(l.weight_offset, at, "layer {}", l.number);
            at += l.weight_bytes().next_multiple_of(0x100);
            assert_eq!(l.bias_offset, at, "layer {}", l.number);
            at += l.bias_bytes().next_multiple_of(0x100);
        }
        // The last bias is 0x20 bytes and not padded.
        assert_eq!(at - 0xe0, BLOB_LEN);
        let last = &LAYERS[11];
        assert_eq!(last.bias_offset + last.bias_bytes(), BLOB_LEN);
    }

    #[test]
    fn shapes_chain() {
        for l in &LAYERS {
            match l.kind {
                Kind::Conv3x3 | Kind::Final => {
                    assert_eq!((l.in_width, l.in_height), (l.out_width, l.out_height))
                }
                Kind::Down2x2 => assert_eq!(
                    (l.in_width / 2, l.in_height.div_ceil(2)),
                    (l.out_width, l.out_height)
                ),
                Kind::Up2x2 => assert_eq!(
                    (l.in_width * 2, l.in_height * 2),
                    (l.out_width, l.out_height)
                ),
            }
        }
        for pair in LAYERS.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert_eq!(
                (a.out_width, a.out_height, a.out_channels),
                (b.in_width, b.in_height, b.in_channels)
            );
        }
    }

    #[test]
    fn skips_have_the_size_of_the_output() {
        for l in LAYERS.iter().filter(|l| l.skip.is_some()) {
            let s = &LAYERS[l.skip.unwrap_or(1) - 1];
            assert_eq!(
                (s.out_width, s.out_height, s.out_channels),
                (l.out_width, l.out_height, l.out_channels)
            );
        }
    }

    #[test]
    fn slices_of_a_blob() {
        let blob = vec![0u8; BLOB_LEN];
        for l in &LAYERS {
            assert_eq!(l.weights(&blob).map(<[u8]>::len), Some(l.weight_bytes()));
            assert_eq!(l.bias(&blob).map(<[u8]>::len), Some(l.bias_bytes()));
        }
        assert!(LAYERS[11].bias(&blob[..BLOB_LEN - 1]).is_none());
    }
}
