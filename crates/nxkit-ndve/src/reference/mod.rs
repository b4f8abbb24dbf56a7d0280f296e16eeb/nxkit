//! A CPU implementation of the whole chain, with the arithmetic read from the game's shader code.
//!
//! It is slow on purpose and meant for tests and diagnostics: the Metal kernels are checked against it layer by layer.
//! Every layer is a function of its own (input tensor, weights bytes, bias bytes, output tensor), and [`run_chain`]
//! strings them together from NV12 planes to the RGBA8 output. The numbers of the arithmetic come from
//! `docs/plans/ndve-native.md`; sections are named where it matters.
//!
//! # Tensors and weights
//!
//! A [`Tensor`] is NHWC with f16 values (as `u16` bit patterns). Weights are the f16 bytes of the blob as the game has
//! them, little endian, `[tap][cin][cout]`; a bias is `cout` f16 values.
//!
//! # The HMMA step
//!
//! One `HMMA.16816.F16` adds 16 products to a half `C` and rounds the result to half. The hardware does not say how it
//! sums inside (R, only a console could tell). The reference fixes this order, and package B's kernels are checked
//! against it: **in f32, the products `k = 0..15` in index order, `s = p0; s = s + p1; ...; s = s + p15`, then
//! `s = s + C`, then one rounding of `s` to half (nearest even).** A product of two halves is exact in f32. See
//! [`hmma_step`]. A chain starts with `C = +0.0` (the skip tensor for up layers) and every step's result is the next
//! step's `C`, so there is a rounding to half after each K = 16.
//!
//! # Marks
//!
//! What ndve-native.md marks I (inferred) or R (runtime) is implemented as the best guess and said so where it is:
//! the clamp-to-edge addressing of every sampler, the filter in f32, the RGB10A2 rounding, the unorm rounding of the
//! RGBA8 writes, the resampler's window and which 8x8 of the result is the delta, the dither texture's format.

use crate::half::{self, NEG_ZERO, ZERO};
#[cfg(test)]
use crate::net::RESAMPLER_LEN;
use crate::net::{Kind, LAYERS, Layer};
use std::fmt;

use picture::pack10;
#[cfg(test)]
use picture::unorm8;
pub use picture::{convert_nv12, resample};

mod picture;

/// A reference error: the inputs do not fit the layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A tensor or picture has a size or channel count the layer cannot take (the reason).
    Shape(&'static str),
    /// A weights, bias or plane slice has the wrong length (the reason).
    Length(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Shape(s) => write!(f, "bad shape: {s}"),
            Error::Length(s) => write!(f, "bad length: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// An f16 tensor, NHWC with batch 1: `data[(y * width + x) * channels + c]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tensor {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// Channels.
    pub channels: usize,
    /// The f16 bit patterns.
    pub data: Vec<u16>,
}

impl Tensor {
    /// A tensor of +0.0.
    pub fn zeros(width: usize, height: usize, channels: usize) -> Tensor {
        Tensor {
            width,
            height,
            channels,
            data: vec![ZERO; width * height * channels],
        }
    }

    /// The channels of pixel (`x`, `y`).
    pub fn pixel(&self, x: usize, y: usize) -> &[u16] {
        let at = (y * self.width + x) * self.channels;
        &self.data[at..at + self.channels]
    }

    fn pixel_mut(&mut self, x: usize, y: usize) -> &mut [u16] {
        let at = (y * self.width + x) * self.channels;
        &mut self.data[at..at + self.channels]
    }

    fn floats(&self) -> Vec<f32> {
        self.data.iter().map(|&h| half::to_f32(h)).collect()
    }
}

/// An RGBA8 image, rows top to bottom, 4 bytes a pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// The bytes.
    pub data: Vec<u8>,
}

/// An RGB10A2 unorm image, one `u32` a pixel: R in bits 0 to 9, G in 10 to 19, B in 20 to 29, A in 30 and 31.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture10 {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// The packed pixels.
    pub data: Vec<u32>,
}

/// The dither texture of the resampler: one unorm value (0 to 1) a texel. Its format is R in the notes; one channel
/// is the best guess. Reads use `x & 0x1ff`, `y & 0x1ff`, wrapped into the texture's size.
#[derive(Clone, Debug, PartialEq)]
pub struct Dither {
    /// Width in texels.
    pub width: usize,
    /// Height in texels.
    pub height: usize,
    /// The values, row by row.
    pub values: Vec<f32>,
}

/// The NV12 planes of one picture: Y with one byte a pixel, CbCr with two bytes a texel at half the size.
#[derive(Clone, Copy, Debug)]
pub struct Nv12<'a> {
    /// Width of the Y plane. A multiple of 8.
    pub width: usize,
    /// Height of the Y plane. A multiple of 8.
    pub height: usize,
    /// The Y plane, `width * height` bytes.
    pub luma: &'a [u8],
    /// The CbCr plane, `(width / 2) * (height / 2) * 2` bytes.
    pub chroma: &'a [u8],
}

/// The result of [`run_chain`], with the intermediate tensors kept for layer-by-layer checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    /// The RGBA8 picture of the colour conversion.
    pub picture: Image,
    /// The outputs of layers 1 to 11 (`layers[0]` is layer 1).
    pub layers: Vec<Tensor>,
    /// The RGB10A2 picture of layer 12.
    pub final_picture: Picture10,
    /// The RGBA8 output of the resampler.
    pub output: Image,
}

// ---------------------------------------------------------------------------------------------------------------
// The HMMA step and the epilogue

/// One `HMMA.16816.F16` step for one output element: `half(sum(a[k] * b[k], k = 0..15) + c)`.
///
/// The order is fixed: f32 products summed `k = 0` to `15` in order, then `+ c`, one rounding to half. See the module
/// documentation. Both slices must have 16 values.
pub fn hmma_step(a: &[u16; 16], b: &[u16; 16], c: u16) -> u16 {
    let a = a.map(half::to_f32);
    let b = b.map(half::to_f32);
    step(&a, &b, 0, 1, c)
}

/// The same with the operands as f32 values of halves. `b[k]` is `w[base + k * stride]`.
fn step(a: &[f32], w: &[f32], base: usize, stride: usize, c: u16) -> u16 {
    let mut s = a[0] * w[base];
    for k in 1..16 {
        s += a[k] * w[base + k * stride];
    }
    half::from_f32(s + half::to_f32(c))
}

/// `relu(half(d + bias))`, the epilogue of layers 1 to 11. `HFMA2.RELU` computes `d * 1.0 + bias` with one rounding
/// and clears negatives; [`half::relu`] says what it does with -0.0 (inferred).
pub fn epilogue(d: u16, bias: u16, relu: bool) -> u16 {
    let s = half::add(d, bias);
    if relu { half::relu(s) } else { s }
}

fn halves(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

fn half_floats(bytes: &[u8]) -> Vec<f32> {
    halves(bytes).into_iter().map(half::to_f32).collect()
}

fn finish(t: &mut Tensor, bias: &[u16], relu: bool) {
    let n = t.channels;
    for px in t.data.chunks_exact_mut(n) {
        for (d, &b) in px.iter_mut().zip(bias) {
            *d = epilogue(*d, b, relu);
        }
    }
}

/// Check the weights of a layer with `taps` taps and the bias; give the output channels.
fn out_channels(input: &Tensor, weights: &[u8], bias: &[u8], taps: usize) -> Result<usize, Error> {
    if input.channels == 0 || !input.channels.is_multiple_of(16) {
        return Err(Error::Shape("input channels are not a multiple of 16"));
    }
    if input.data.len() != input.width * input.height * input.channels {
        return Err(Error::Shape("tensor data does not match its size"));
    }
    let per = taps * input.channels * 2;
    if weights.is_empty() || !weights.len().is_multiple_of(per) {
        return Err(Error::Length("weights are not taps x cin x cout halves"));
    }
    let cout = weights.len() / per;
    if bias.len() < cout * 2 {
        return Err(Error::Length("bias is shorter than cout halves"));
    }
    Ok(cout)
}

// ---------------------------------------------------------------------------------------------------------------
// The layers

/// A 3x3 convolution, stride 1, zero padding on all four sides, without bias or ReLU: the HMMA chain only.
///
/// Per output pixel and channel the chain runs over the taps `t = 0..8` (`dy = t / 3 - 1`, `dx = t % 3 - 1`) and, in
/// each, over the K blocks of 16 input channels, starting from `C = +0.0`. A tap outside the picture adds a step with
/// zero products, as the game's masked loads do.
pub fn conv3x3_raw(input: &Tensor, weights: &[u8], bias: &[u8]) -> Result<Tensor, Error> {
    let cout = out_channels(input, weights, bias, 9)?;
    let cin = input.channels;
    let w = half_floats(weights);
    let a = input.floats();
    let zeros = vec![0.0f32; cin];
    let mut out = Tensor::zeros(input.width, input.height, cout);
    for y in 0..input.height {
        for x in 0..input.width {
            let mut acc = vec![ZERO; cout];
            for t in 0..9 {
                let sx = x as i64 + (t % 3) as i64 - 1;
                let sy = y as i64 + (t / 3) as i64 - 1;
                let inside = sx >= 0
                    && sy >= 0
                    && (sx as usize) < input.width
                    && (sy as usize) < input.height;
                let src = if inside {
                    let at = (sy as usize * input.width + sx as usize) * cin;
                    &a[at..at + cin]
                } else {
                    &zeros[..]
                };
                for kb in 0..cin / 16 {
                    let av = &src[kb * 16..kb * 16 + 16];
                    let base = (t * cin + kb * 16) * cout;
                    for (n, c) in acc.iter_mut().enumerate() {
                        *c = step(av, &w, base + n, cout, *c);
                    }
                }
            }
            out.pixel_mut(x, y).copy_from_slice(&acc);
        }
    }
    Ok(out)
}

/// A 3x3 convolution with bias and optional ReLU (layers 1, 2, 4, 6, 7, 9, 11). See [`conv3x3_raw`].
pub fn conv3x3(input: &Tensor, weights: &[u8], bias: &[u8], relu: bool) -> Result<Tensor, Error> {
    let mut out = conv3x3_raw(input, weights, bias)?;
    let b = halves(bias);
    finish(&mut out, &b, relu);
    Ok(out)
}

/// A 2x2 convolution with stride 2 and no padding (layers 3 and 5), with bias and ReLU.
///
/// Tap `t = 2 * dy + dx` reads the input pixel (`2x + dx`, `2y + dy`). The chain runs over the taps, then the K blocks.
/// The input size must be even.
pub fn down2x2(input: &Tensor, weights: &[u8], bias: &[u8]) -> Result<Tensor, Error> {
    let cout = out_channels(input, weights, bias, 4)?;
    if !input.width.is_multiple_of(2) || !input.height.is_multiple_of(2) {
        return Err(Error::Shape("a down layer needs an even size"));
    }
    let cin = input.channels;
    let w = half_floats(weights);
    let a = input.floats();
    let mut out = Tensor::zeros(input.width / 2, input.height / 2, cout);
    for y in 0..out.height {
        for x in 0..out.width {
            let mut acc = vec![ZERO; cout];
            for t in 0..4 {
                let (sx, sy) = (2 * x + t % 2, 2 * y + t / 2);
                let at = (sy * input.width + sx) * cin;
                for kb in 0..cin / 16 {
                    let av = &a[at + kb * 16..at + kb * 16 + 16];
                    let base = (t * cin + kb * 16) * cout;
                    for (n, c) in acc.iter_mut().enumerate() {
                        *c = step(av, &w, base + n, cout, *c);
                    }
                }
            }
            out.pixel_mut(x, y).copy_from_slice(&acc);
        }
    }
    let b = halves(bias);
    finish(&mut out, &b, true);
    Ok(out)
}

/// A 2x2 transposed convolution with stride 2 (layers 8 and 10), with bias and ReLU.
///
/// Input pixel (`x`, `y`) and tap `t = 2 * dy + dx` give output pixel (`2x + dx`, `2y + dy`). The chain over the K
/// blocks starts with `C` = the skip tensor's value at that output pixel and channel. `skip` must be twice the input's
/// size with the output's channels.
pub fn up2x2(input: &Tensor, skip: &Tensor, weights: &[u8], bias: &[u8]) -> Result<Tensor, Error> {
    let cout = out_channels(input, weights, bias, 4)?;
    if skip.width != input.width * 2 || skip.height != input.height * 2 || skip.channels != cout {
        return Err(Error::Shape(
            "the skip tensor does not fit the up layer's output",
        ));
    }
    if skip.data.len() != skip.width * skip.height * skip.channels {
        return Err(Error::Shape("skip data does not match its size"));
    }
    let cin = input.channels;
    let w = half_floats(weights);
    let a = input.floats();
    let mut out = Tensor::zeros(skip.width, skip.height, cout);
    for y in 0..input.height {
        for x in 0..input.width {
            for t in 0..4 {
                let (ox, oy) = (2 * x + t % 2, 2 * y + t / 2);
                let mut acc = skip.pixel(ox, oy).to_vec();
                let at = (y * input.width + x) * cin;
                for kb in 0..cin / 16 {
                    let av = &a[at + kb * 16..at + kb * 16 + 16];
                    let base = (t * cin + kb * 16) * cout;
                    for (n, c) in acc.iter_mut().enumerate() {
                        *c = step(av, &w, base + n, cout, *c);
                    }
                }
                out.pixel_mut(ox, oy).copy_from_slice(&acc);
            }
        }
    }
    let b = halves(bias);
    finish(&mut out, &b, true);
    Ok(out)
}

/// Run layer `layer` of the table on `input` with the weights and bias taken from `blob`. Layers 1 to 11 only; layer 12
/// is [`final_layer`]. `skip` is needed by the up layers.
pub fn run_layer(
    layer: &Layer,
    input: &Tensor,
    skip: Option<&Tensor>,
    blob: &[u8],
) -> Result<Tensor, Error> {
    let (Some(w), Some(b)) = (layer.weights(blob), layer.bias(blob)) else {
        return Err(Error::Length(
            "the blob is shorter than the layer's weights",
        ));
    };
    match layer.kind {
        Kind::Conv3x3 => conv3x3(input, w, b, layer.relu),
        Kind::Down2x2 => down2x2(input, w, b),
        Kind::Up2x2 => up2x2(
            input,
            skip.ok_or(Error::Shape("an up layer needs its skip tensor"))?,
            w,
            b,
        ),
        Kind::Final => Err(Error::Shape("layer 12 is final_layer")),
    }
}

/// The colour conversion, space to depth: an RGBA8 picture of `2w x 2h` becomes a tensor of `w x h x 16`.
///
/// Packed pixel (`X`, `Y`) channel `k = 4 * (2 * sy + sx) + c` is component `c` of the texel (`2X + sx`, `2Y + sy`),
/// as `c / 255` rounded to half (section 6).
pub fn space_to_depth(picture: &Image) -> Result<Tensor, Error> {
    if !picture.width.is_multiple_of(2)
        || !picture.height.is_multiple_of(2)
        || picture.data.len() != picture.width * picture.height * 4
    {
        return Err(Error::Shape(
            "the picture needs an even size and 4 bytes a pixel",
        ));
    }
    let mut out = Tensor::zeros(picture.width / 2, picture.height / 2, 16);
    for y in 0..out.height {
        for x in 0..out.width {
            let px = out.pixel_mut(x, y);
            for s in 0..4 {
                let (tx, ty) = (2 * x + s % 2, 2 * y + s / 2);
                let at = (ty * picture.width + tx) * 4;
                for c in 0..4 {
                    px[4 * s + c] = half::from_f32(f32::from(picture.data[at + c]) / 255.0);
                }
            }
        }
    }
    Ok(out)
}

/// Layer 12: a 3x3 convolution without ReLU, added to the RGBA8 picture, depth to space (section 6). The input tensor
/// is `w x h x 16`; `picture` is `2w x 2h`; the result is the RGB10A2 picture of `2w x 2h`.
///
/// For the output pixel (`2X + sx`, `2Y + sy`), `s = 2 * sy + sx`, component `c = 0..3` comes from network channel
/// `n = 2s, 2s + 1, 8 + 2s, 9 + 2s`: `out = ((half(picture_c) + D_n) + bias_n) + (-0.0)`, each sum rounded to half,
/// then written as RGB10A2 unorm. The rounding to 10 bits (and 2 bits for alpha) is round to nearest even after a clamp
/// to 0 to 1 (inferred: the format is from the PR, R to confirm).
pub fn final_layer(
    input: &Tensor,
    picture: &Image,
    weights: &[u8],
    bias: &[u8],
) -> Result<Picture10, Error> {
    if input.channels != 16 {
        return Err(Error::Shape("the final layer takes 16 channels"));
    }
    if picture.width != input.width * 2
        || picture.height != input.height * 2
        || picture.data.len() != picture.width * picture.height * 4
    {
        return Err(Error::Shape(
            "the picture is not twice the size of the tensor",
        ));
    }
    let d = conv3x3_raw(input, weights, bias)?;
    if d.channels != 16 {
        return Err(Error::Length(
            "the final layer's weights are not 16 channels",
        ));
    }
    let b = halves(bias);
    let mut out = Picture10 {
        width: picture.width,
        height: picture.height,
        data: vec![0; picture.width * picture.height],
    };
    for y in 0..d.height {
        for x in 0..d.width {
            let px = d.pixel(x, y);
            for s in 0..4 {
                let (ox, oy) = (2 * x + s % 2, 2 * y + s / 2);
                let at = (oy * picture.width + ox) * 4;
                let chan = [2 * s, 2 * s + 1, 8 + 2 * s, 9 + 2 * s];
                let mut v = [0.0f32; 4];
                for (c, &n) in chan.iter().enumerate() {
                    let pic = half::from_f32(f32::from(picture.data[at + c]) / 255.0);
                    let t = half::add(pic, px[n]);
                    let t = half::add(t, b[n]);
                    v[c] = half::to_f32(half::add(t, NEG_ZERO));
                }
                out.data[oy * picture.width + ox] = pack10(v);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------------------------
// The chain

/// Run the whole chain and keep what each stage made. `conversion` is the colour conversion's push block as 16 float
/// words, `resampler_push` the resampler's 21 words, `blob` the weight blob (0x3c320 bytes), `resampler` its 0x3000
/// bytes of weights. The picture size must be a multiple of 8.
///
/// The skip of layer 8 is the output of layer 4, the skip of layer 10 the output of layer 2 (inferred, I).
pub fn run_chain(
    nv12: &Nv12,
    conversion: &[f32; 16],
    resampler_push: &[u32; 21],
    dither: Option<&Dither>,
    blob: &[u8],
    resampler: &[u8],
) -> Result<Chain, Error> {
    let picture = convert_nv12(nv12, conversion)?;
    let mut layers: Vec<Tensor> = Vec::with_capacity(11);
    let mut cur = space_to_depth(&picture)?;
    for l in &LAYERS[..11] {
        let skip = l.skip.map(|n| &layers[n - 1]);
        cur = run_layer(l, &cur, skip, blob)?;
        layers.push(cur.clone());
    }
    let last = &LAYERS[11];
    let (Some(w), Some(b)) = (last.weights(blob), last.bias(blob)) else {
        return Err(Error::Length(
            "the blob is shorter than the layer's weights",
        ));
    };
    let final_picture = final_layer(&cur, &picture, w, b)?;
    let output = resample(&final_picture, resampler, resampler_push, dither)?;
    Ok(Chain {
        picture,
        layers,
        final_picture,
        output,
    })
}

/// Run the whole chain and give the RGBA8 output. See [`run_chain`].
pub fn run(
    nv12: &Nv12,
    conversion: &[f32; 16],
    resampler_push: &[u32; 21],
    dither: Option<&Dither>,
    blob: &[u8],
    resampler: &[u8],
) -> Result<Image, Error> {
    run_chain(nv12, conversion, resampler_push, dither, blob, resampler).map(|c| c.output)
}

#[cfg(test)]
#[path = "../reference_tests.rs"]
mod tests;
