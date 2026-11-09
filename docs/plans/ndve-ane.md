# nn::ndve on the Neural Engine: the model and its interface

Status: the model writer is done (`crates/nxkit-ndve/src/ane/`); loading and running it is `nxkit-coreml`'s job; the wiring into the game's frame is described in the last section (`NXKIT_NDVE_ANE=1`, off by default, untested in the game). Numbers are from the feasibility study (scratch prototype built with coremltools): the "fullnb" model, 50 operations, all on the ANE, 9.1 ms per 1080p frame, output within 1/255 of the reference after the GPU pass below; the U-Net alone is 33 operations, 4.8 ms, 2 of 1023 on the RGB10 picture.

## What the crate gives

`ane::build_package(blob, resampler, Variant::Full)` returns the files of an `.mlpackage` as `(relative path, bytes)`: `Manifest.json`, `Data/com.apple.CoreML/model.mlmodel` (Model.proto with an ML Program, spec 9, opset CoreML8) and `Data/com.apple.CoreML/weights/weight.bin` (f16, 64-byte aligned entries). `blob` is the 0x3c320-byte weight blob of the `.dat` (`file::Network::weights`), `resampler` the 0x3000 bytes of `file::Network::resampler`. No I/O, no dependencies; equal weights give equal bytes. Load with `compute_units = cpuAndNeuralEngine`. `Variant::UnetOnly` is the U-Net without the resampler (output `final` only).

## The graph (op for op the prototype's)

U-Net: `pixel_unshuffle(2)`; layers 1 to 11 as `conv`/`conv_transpose` + bias + `relu` (the up layers are `conv_transpose`, `add` of the skip, `add` of the bias); layer 12 as a 3x3 conv with the 12 output channels the picture needs, `pixel_shuffle(2)`, `slice_by_size` of RGB from the input, `add`, `clip(0, 1)`. Resampler: luma 1x1 conv, `pad` (replicate, 5/7), `pixel_unshuffle(6)`, a 3x1 conv 36 to 192 and a grouped 1x3 conv to 256 channels (block 0, the window gather), then blocks 1 to 5 as dense 1x1 convs 256 to 256 (the last 256 to 64, no ReLU) whose weights are `sum_g W1_g (x) W2_g` computed in f32 and rounded to f16, `pixel_shuffle(8)`. No `round` (silently wrong on the ANE). Weight permutations: layer 1's input channels follow the space-to-depth order, layer 12 keeps output channels `2s, 2s+1, 8+2s, 9+2s` for `s` in 0..4 (see the module docs of `ane/graph.rs` and `ane/resampler.rs`).

## Tensors

| | name | shape (N,C,H,W) | type | content |
|---|---|---|---|---|
| input | `x` | 1,4,1080,1920 | f16 planar | R, G, B of the colour conversion's RGBA8 picture as `k / 255` rounded to f16, row 0 on top; plane 3 = 1.0 |
| output | `delta` | 1,1,1440,2560 | f16 | the resampler network output, added equally to R, G, B (`Variant::Full`) |
| output | `final` | 1,3,1080,1920 | f16 planar | `clip(picture + d, 0, 1)`, not quantised |

## What the GPU still does (not in the model)

1. Colour conversion (NV12 to RGBA8) writes the picture; a GPU pass copies it to the input tensor's IOSurface as planar f16 (`k / 255`, alpha plane 1.0).
2. After the ANE: quantise `final` to RGB10 (`round(final * 1023) / 1023`, half to even), sample it with a linear sampler at `((X + 0.5) / 2560, (Y + 0.5) / 1440)` (bilinear 4/3, f16 result), `v = f16(s + delta[Y][X])` for R, G, B, then the game's quantise/dither step (`k = rint(fma(v, 255, dither * 2 - 1))`, `out = k / 255`, dither texture read at the pixel masked with `0x1ff`), alpha 1, flip rows when the game says so, write RGBA8 2560x1440.

## Tests

Structure (50 and 33 operations, names, shapes), the weight file's offsets (equal to the prototype's, total 862528 bytes) and alignment, the layer-1 permutation, the dense folding, determinism. An ignored test loads the package with coremltools on the CPU and compares it with the prototype's model on synthetic weights.

## Wiring in the frame (`crates/nxkit-gpu/src/native/{ane,offload,ring}.rs`)

`NXKIT_NDVE_ANE=1` turns it on; `NXKIT_NDVE_ANE_CACHE=<dir>` moves the cache (default `~/Library/Caches/nxkit/ndve-ane`: `<blob sha>-<os build>.mlpackage` and the compiled model next to it). When the NDVE run is found, a background thread writes the package (`Variant::Full`), compiles and loads it, and refuses it unless `compute_plan().all_on_neural_engine()` (the off-ANE operations go to the log). Three sets of tensors (input `x`, outputs `final` and `delta`, IOSurface f16, wrapped as R16Float textures) form the ring (`ring.rs`: Free, Filling, Running, Ready, Reading).

Per picture the frame always runs `ndve_ane_input` (colour conversion written as `x`; the game's conversion push block). When the previous picture's result is ready, the frame runs `ndve_ane_quant` (`final` to the RGB10A2 picture of layer 12, which the image write quantises) and `ndve_ane_epilogue` (linear sample, `+ delta`, dither, flip, RGBA8; the game's resampler push block) instead of the Metal plan. Otherwise the Metal plan runs. A command buffer's `after_gpu` callback releases the slot it read and, for the slot it wrote, starts `predict_async`; its completion makes the slot Ready. The result is used by the next picture only (one picture of latency); an older result is dropped.

Health lines: `ndve.ane` (pictures made from the Neural Engine) and `ndve.ane_fallback(reason)` with the reasons `warmup` (still loading), `error` (loading refused; the text says why), `first`, `late` (previous result not ready), `ring` (no free slot, the picture is not sent), `predict` (the Neural Engine failed), `textures`. With `NXKIT_NDVE_COMPARE=1` the sampled frame compares the epilogue's output with the game's chain.
