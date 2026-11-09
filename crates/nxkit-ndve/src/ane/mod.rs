//! The movie network as a Core ML ML Program for the Apple Neural Engine.
//!
//! [`build_package`] turns the weights of the game's `.dat` (the blob of [`crate::file::Network::weights`] and the
//! resampler weights of [`crate::file::Network::resampler`]) into the files of an `.mlpackage` directory. The
//! caller writes the files and loads them with Core ML (`compute_units = cpuAndNeuralEngine`). Nothing here does I/O,
//! and equal weights give equal bytes. The graph is the "fullnb" model of the feasibility study
//! (`docs/plans/ndve-ane.md`): 50 operations, all on the Neural Engine, 9.1 ms on an M-series Mac; the model of
//! [`Variant::UnetOnly`] has 33 operations. The protobuf encoder is hand written ([`mod@proto`]); the field numbers are
//! from the public coremltools `.proto` files (BSD-3-Clause, `THIRD_PARTY.md`).
//!
//! # What the model computes
//!
//! The twelve U-Net layers on the 2x2 space-to-depth of the picture (layers 1 to 11 with ReLU, layer 12 without), the
//! 16 -> 12 channel selection of layer 12, depth to space, the add of the picture and a clip to `[0, 1]`. That is the
//! `final` output. For [`Variant::Full`] the resampler's six blocks run on the luma of `final` and give `delta`.
//! The bilinear 4/3 resize, the add of `delta`, the dither, the flip and the RGBA8 write are not in the model: they
//! are a GPU pass (below). The model has no `round` (it gives silently wrong results on the ANE).
//!
//! # Input
//!
//! - name `x`, shape `[1, 4, 1080, 1920]` (N, C, H, W), f16, planar: four planes of 1080 rows of 1920 values, row 0
//!   at the top of the picture, no flip. A row stride larger than 1920 values (IOSurface padding) is the caller's to
//!   declare to Core ML.
//! - plane 0, 1, 2 = R, G, B of the picture the game's colour conversion writes (RGBA8 unorm) as `k / 255` rounded
//!   to f16, i.e. the value in `[0, 1]` a shader reads from an RGBA8 texture, converted to half. plane 3 = alpha =
//!   `1.0` everywhere (the network's weights use it; the picture's alpha is 255).
//!
//! # Outputs (f16, NCHW, planar)
//!
//! - `final` `[1, 3, 1080, 1920]`: `clip(picture + d, 0, 1)` per channel R, G, B, not quantised. The game writes
//!   this to an RGB10A2 image: the GPU pass quantises it as `q = round(final * 1023) / 1023` (round half to even,
//!   what a unorm10 image write does) and uses `q` as the picture of the epilogue.
//! - `delta` `[1, 1, 1440, 2560]` ([`Variant::Full`] only): the resampler's network output, one value per output
//!   pixel, added equally to R, G and B.
//!
//! # The GPU pass that completes the output (not in the model)
//!
//! For each output pixel `(X, Y)` of the 2560x1440 RGBA8 image: sample `q` (the quantised `final`, three channels)
//! with a linear sampler at `uv = ((X + 0.5) / 2560, (Y + 0.5) / 1440)`, clamp-to-edge, result in f16 (this is the
//! bilinear 4/3 of the game); `v = f16(s + delta[Y][X])` for R, G and B; if the game's quantise flag is set,
//! `k = rint(fma(v, 255, dither * 2 - 1))` (the dither texture value read at the pixel with the coordinates masked
//! with `0x1ff`) and `out = k * (1 / 255)`, else `v` is written as it is (the RGBA8 store quantises); alpha = 1; the row is flipped when the game says so
//! (`Y_out = flip_base - Y`). `epi.py` of the feasibility study emulates it: it reached the reference within
//! 1/255.
//!
//! # Layout of the crate's code
//!
//! - [`mod@mil`]: the program builder (constants, operations, shapes) and its protobuf.
//! - [`mod@graph`]: the network as operations.
//! - `resampler`: the resampler weights as convolution weights.
//! - [`mod@blob`]: the file `weights/weight.bin`.
//! - [`mod@epilogue`]: the GPU's epilogue on the CPU (reference) and the reader of the game's push block.
//! - [`mod@package`]: `Manifest.json` and `Model.proto`.
//! - [`mod@proto`]: the protobuf writer.

pub mod blob;
pub mod epilogue;
pub mod graph;
pub mod mil;
pub mod package;
pub mod proto;
mod resampler;

pub use graph::Variant;
pub use package::{Package, TensorSpec};

/// Build the package of `variant` for the weight blob `blob` (`net::BLOB_LEN` bytes) and the resampler weights `res`
/// (`net::RESAMPLER_LEN` bytes; [`Variant::UnetOnly`] does not read them).
///
/// # Errors
///
/// When a blob has the wrong length.
pub fn build_package(blob: &[u8], res: &[u8], variant: Variant) -> Result<Package, String> {
    let mut b = mil::Builder::new();
    let outs = graph::build(&mut b, blob, res, variant)?;
    let (ops, infos, weights, entries) = b.finish();
    let spec = |t: &mil::Tensor| TensorSpec {
        name: t.name.clone(),
        shape: t.shape.clone(),
    };
    let input = spec(&graph::input());
    let outputs: Vec<TensorSpec> = outs.iter().map(spec).collect();
    Ok(package::assemble(
        ops, infos, weights, entries, input, outputs,
    ))
}

#[cfg(test)]
mod tests;

/// The key of the compiled model in a cache: the SHA-256 of the weight blob and the build of the OS (Core ML compiles
/// for the OS it runs on).
#[must_use]
pub fn cache_key(blob: &[u8], os_build: &str) -> String {
    format!(
        "{}-{}",
        nxb_model::to_hex(&nxb_model::sha256(blob).0),
        os_build
    )
}
