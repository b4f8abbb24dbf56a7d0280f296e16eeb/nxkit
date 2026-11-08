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
use crate::net::{Kind, LAYERS, Layer, RESAMPLER_BLOCK, RESAMPLER_LEN};
use std::fmt;

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

fn pack10(v: [f32; 4]) -> u32 {
    let q = |x: f32, max: f32| (x.clamp(0.0, 1.0) * max).round_ties_even() as u32;
    q(v[0], 1023.0) | (q(v[1], 1023.0) << 10) | (q(v[2], 1023.0) << 20) | (q(v[3], 3.0) << 30)
}

fn unpack10(p: u32) -> [f32; 3] {
    [
        (p & 0x3ff) as f32 / 1023.0,
        ((p >> 10) & 0x3ff) as f32 / 1023.0,
        ((p >> 20) & 0x3ff) as f32 / 1023.0,
    ]
}

fn unorm8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8
}

// ---------------------------------------------------------------------------------------------------------------
// Colour conversion

/// A bilinear sample at the unnormalised texel position (`x`, `y`), clamp to edge. The filter is in f32; the hardware
/// uses fixed-point weights (R: the difference is below an 8-bit step).
fn bilinear<const N: usize>(
    fetch: impl Fn(usize, usize) -> [f32; N],
    x: f32,
    y: f32,
    w: usize,
    h: usize,
) -> [f32; N] {
    let (px, py) = (x - 0.5, y - 0.5);
    let (fx0, fy0) = (px.floor(), py.floor());
    let (fx, fy) = (px - fx0, py - fy0);
    let clamp = |v: f32, n: usize| (v as i64).clamp(0, n as i64 - 1) as usize;
    let (x0, x1) = (clamp(fx0, w), clamp(fx0 + 1.0, w));
    let (y0, y1) = (clamp(fy0, h), clamp(fy0 + 1.0, h));
    let (a, b, c, d) = (fetch(x0, y0), fetch(x1, y0), fetch(x0, y1), fetch(x1, y1));
    let mut r = [0.0f32; N];
    for i in 0..N {
        let top = a[i] * (1.0 - fx) + b[i] * fx;
        let bottom = c[i] * (1.0 - fx) + d[i] * fx;
        r[i] = top * (1.0 - fy) + bottom * fy;
    }
    r
}

/// The colour conversion of pass 3 (section 7): NV12 to an RGBA8 picture.
///
/// `push` holds the 16 words of the push block as floats: matrix rows at words 0 to 2, 4 to 6 and 8 to 10, offsets at
/// words 12 to 14 (words 3, 7, 11 and 15 are unused). With `Y' = Y + w12`, `U' = Cb + w13`, `V' = Cr + w14`:
/// `r = fma(V', w2, fma(Y', w0, U' * w1))` and likewise for g (words 4 to 6) and b (words 8 to 10), all f32. The
/// chroma is sampled bilinearly at `uv = ((x + 0.5) / W, (y + 0.5) / H)` (clamp to edge, a guess: R). Alpha is 1.
/// The unorm write rounds to nearest even (inferred).
pub fn convert_nv12(nv12: &Nv12, push: &[f32; 16]) -> Result<Image, Error> {
    let (w, h) = (nv12.width, nv12.height);
    if w == 0 || h == 0 || !w.is_multiple_of(8) || !h.is_multiple_of(8) {
        return Err(Error::Shape("the picture size must be a multiple of 8"));
    }
    if nv12.luma.len() != w * h || nv12.chroma.len() != (w / 2) * (h / 2) * 2 {
        return Err(Error::Length("the planes do not match the picture size"));
    }
    let (cw, ch) = (w / 2, h / 2);
    let mut data = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let yy = f32::from(nv12.luma[y * w + x]) / 255.0 + push[12];
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let c = bilinear(
                |sx, sy| {
                    let at = (sy * cw + sx) * 2;
                    [
                        f32::from(nv12.chroma[at]) / 255.0,
                        f32::from(nv12.chroma[at + 1]) / 255.0,
                    ]
                },
                u * cw as f32,
                v * ch as f32,
                cw,
                ch,
            );
            let (cb, cr) = (c[0] + push[13], c[1] + push[14]);
            let rgb = [
                cr.mul_add(push[2], yy.mul_add(push[0], cb * push[1])),
                cr.mul_add(push[6], yy.mul_add(push[4], cb * push[5])),
                cr.mul_add(push[10], yy.mul_add(push[8], cb * push[9])),
            ];
            let at = (y * w + x) * 4;
            for (i, &c) in rgb.iter().enumerate() {
                data[at + i] = unorm8(c);
            }
        }
    }
    Ok(Image {
        width: w,
        height: h,
        data,
    })
}

// ---------------------------------------------------------------------------------------------------------------
// Resampler

/// The weights of one resampler block as f32 values of halves.
struct Block {
    /// `w1[g][m * 16 + k]`: the 16x16 matrix `W1_g`, stored as four 8x8 row-major tiles in the order `[kb][rb]`.
    w1: [[f32; 256]; 2],
    /// `w2[g][j][k * 8 + n]`: the 16x8 tile `W2_gj` of two 8x8 row-major sub-tiles (`k` 0 to 7, then 8 to 15).
    w2: [[[f32; 128]; 2]; 2],
}

fn parse_block(bytes: &[u8]) -> Block {
    let h = |off: usize| half::to_f32(u16::from_le_bytes([bytes[off], bytes[off + 1]]));
    let mut b = Block {
        w1: [[0.0; 256]; 2],
        w2: [[[0.0; 128]; 2]; 2],
    };
    for g in 0..2 {
        for kb in 0..2 {
            for rb in 0..2 {
                for i in 0..8 {
                    for j in 0..8 {
                        let off = g * 0x200 + (kb * 2 + rb) * 0x80 + i * 16 + j * 2;
                        b.w1[g][(8 * rb + i) * 16 + 8 * kb + j] = h(off);
                    }
                }
            }
        }
        for jj in 0..2 {
            for s in 0..2 {
                for i in 0..8 {
                    for n in 0..8 {
                        let off = 0x400 + 0x200 * g + 0x100 * jj + s * 0x80 + i * 16 + n * 2;
                        b.w2[g][jj][(8 * s + i) * 8 + n] = h(off);
                    }
                }
            }
        }
    }
    b
}

/// The resampler MLP on one 16x16 luma window `x` (`x[row * 16 + col]`, halves as f32). Returns the 8x8 delta
/// (`delta[row * 8 + col]`, half bit patterns).
///
/// Six blocks. In each: `H_g = W1_g . X` for g = 0, 1 (as two 16x8 column halves, one HMMA each: 4 HMMAs), then
/// `Y_j = H_0 . W2_0j + H_1 . W2_1j` for j = 0, 1 (a chain of two HMMAs, g = 0 first, starting from C = 0), and
/// `X = max(Y, 0)` for the next block. The last block computes only j = 0 and has no ReLU: its rows 0 to 7 are the
/// delta (inferred: the notes say "the 8x8 delta" of a 16x8 result, not which rows).
/// Every HMMA is a [`hmma_step`]-ordered sum with the rounding to half.
fn mlp(blocks: &[Block; 6], x: &[f32; 256]) -> [u16; 64] {
    let mut x = *x;
    let mut delta = [ZERO; 64];
    for (bi, blk) in blocks.iter().enumerate() {
        let last = bi == 5;
        // Stage 1.
        let mut hh = [[0.0f32; 256]; 2];
        for (h, w1) in hh.iter_mut().zip(&blk.w1) {
            for m in 0..16 {
                let a = &w1[m * 16..m * 16 + 16];
                for col in 0..16 {
                    h[m * 16 + col] = half::to_f32(step(a, &x, col, 16, ZERO));
                }
            }
        }
        // Stage 2.
        let mut next = [0.0f32; 256];
        for j in 0..if last { 1 } else { 2 } {
            for m in 0..16 {
                for n in 0..8 {
                    let d0 = step(&hh[0][m * 16..m * 16 + 16], &blk.w2[0][j], n, 8, ZERO);
                    let d1 = step(&hh[1][m * 16..m * 16 + 16], &blk.w2[1][j], n, 8, d0);
                    if last {
                        if m < 8 {
                            delta[m * 8 + n] = d1;
                        }
                    } else {
                        next[m * 16 + 8 * j + n] = half::to_f32(half::relu(d1));
                    }
                }
            }
        }
        x = next;
    }
    delta
}

/// The resampler (pass 16, section 8): the RGB10A2 picture to the RGBA8 output, 4/3 larger.
///
/// `push` is the 21 words of the push block (words 2 and 3 are the integer maximum x and y of the luma window, 10 and
/// 11 the floats 1/2560 and 1/1440, 18 the flip base, 19 the flip enable, 20 the quantise and dither enable).
/// `weights` are the 0x3000 bytes of the six blocks. `dither` is used when word 20 is not zero; without a texture the
/// dither value is 0.5, which adds nothing.
///
/// The output is `8 * ceil(w / 6)` by `8 * ceil(h / 6)` (2560x1440 for 1920x1080). Output block (`bx`, `by`) of 8x8
/// pixels uses the luma window of 16x16 pixels at (`6 bx - 5`, `6 by - 5`), each coordinate clamped to
/// `[0, w2]` x `[0, w3]` (the window mapping is inferred from the PR: I). Window element `X[row][col]` is the pixel at
/// (`x0 + col`, `y0 + row`). Luma is `fma(B, 0.0722, fma(R, 0.2126, G * 0.7152))` in f32, rounded to half. Each output
/// pixel is `(-0.0) + (s + delta)` per channel in half, with `s` the picture sampled bilinearly at
/// `((x + 0.5) * w10, (y + 0.5) * w11)` and rounded to half (the TEX unit returns half); with `w20 != 0` then
/// `k = rint(fma(v, 255, dither * 2 - 1))` and `v = k * (1/255)`; alpha is 1; the row is `w18 - y` when `w19 != 0`.
pub fn resample(
    picture: &Picture10,
    weights: &[u8],
    push: &[u32; 21],
    dither: Option<&Dither>,
) -> Result<Image, Error> {
    if weights.len() != RESAMPLER_LEN {
        return Err(Error::Length(
            "the resampler's weights are not 0x3000 bytes",
        ));
    }
    let (pw, ph) = (picture.width, picture.height);
    if pw == 0 || ph == 0 || picture.data.len() != pw * ph {
        return Err(Error::Shape("the picture data does not match its size"));
    }
    let blocks: [Block; 6] = std::array::from_fn(|i| {
        parse_block(&weights[i * RESAMPLER_BLOCK..(i + 1) * RESAMPLER_BLOCK])
    });
    let luma: Vec<u16> = picture
        .data
        .iter()
        .map(|&p| {
            let [r, g, b] = unpack10(p);
            half::from_f32(b.mul_add(0.0722, r.mul_add(0.2126, g * 0.7152)))
        })
        .collect();
    let max_x = i64::from(push[2]);
    let max_y = i64::from(push[3]);
    let (w10, w11) = (f32::from_bits(push[10]), f32::from_bits(push[11]));
    let quantise = push[20] != 0;
    let flip = push[19] != 0;
    let (bxs, bys) = (pw.div_ceil(6), ph.div_ceil(6));
    let (ow, oh) = (bxs * 8, bys * 8);
    let mut out = Image {
        width: ow,
        height: oh,
        data: vec![0; ow * oh * 4],
    };
    for by in 0..bys {
        for bx in 0..bxs {
            let mut win = [0.0f32; 256];
            for row in 0..16 {
                let wy = (6 * by as i64 - 5 + row as i64).clamp(0, max_y.min(ph as i64 - 1).max(0))
                    as usize;
                for col in 0..16 {
                    let wx = (6 * bx as i64 - 5 + col as i64)
                        .clamp(0, max_x.min(pw as i64 - 1).max(0))
                        as usize;
                    win[row * 16 + col] = half::to_f32(luma[wy * pw + wx]);
                }
            }
            let delta = mlp(&blocks, &win);
            for py in 0..8 {
                for px in 0..8 {
                    let (x, y) = (bx * 8 + px, by * 8 + py);
                    let u = (x as f32 + 0.5) * w10;
                    let v = (y as f32 + 0.5) * w11;
                    let s = bilinear(
                        |sx, sy| unpack10(picture.data[sy * pw + sx]),
                        u * pw as f32,
                        v * ph as f32,
                        pw,
                        ph,
                    );
                    let yo = if flip {
                        i64::from(push[18]) - y as i64
                    } else {
                        y as i64
                    };
                    if yo < 0 || yo >= oh as i64 {
                        continue;
                    }
                    let at = (yo as usize * ow + x) * 4;
                    for (c, &sc) in s.iter().enumerate() {
                        let sum = half::add(half::from_f32(sc), delta[py * 8 + px]);
                        let mut val = half::to_f32(half::add(NEG_ZERO, sum));
                        if quantise {
                            let d = dither.map_or(0.5, |t| {
                                t.values[(y & 0x1ff) % t.height * t.width + (x & 0x1ff) % t.width]
                            });
                            let k = val.mul_add(255.0, d * 2.0 - 1.0).round_ties_even();
                            val = k * f32::from_bits(0x3b80_8081);
                        }
                        out.data[at + c] = unorm8(val);
                    }
                    out.data[at + 3] = 255;
                }
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
#[path = "reference_tests.rs"]
mod tests;
