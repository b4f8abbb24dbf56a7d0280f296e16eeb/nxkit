# nn::ndve on the Neural Engine: the model and its interface

Status: the model writer is done (`crates/nxkit-ndve/src/ane/`); loading and running it is `nxkit-coreml`'s job; the wiring into the game's frame is described in the last section (on by default, `NXKIT_NDVE_ANE=0` turns it off, off by default). The first wiring (the game's frame writes the input, the model runs after that frame, the next picture reads the result) never had a result in time in the game: 1095 of 1101 movie pictures were `late`, and the extra input pass raised the GPU time. The wiring below runs the model at decode time, ahead of the game. Numbers are from the feasibility study (scratch prototype built with coremltools): the "fullnb" model, 50 operations, all on the ANE, 9.1 ms per 1080p frame, output within 1/255 of the reference after the GPU pass below; the U-Net alone is 33 operations, 4.8 ms, 2 of 1023 on the RGB10 picture.

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

After the Neural Engine: quantise `final` to RGB10 (`round(final * 1023) / 1023`, half to even), sample it with a linear sampler at `((X + 0.5) / 2560, (Y + 0.5) / 1440)` (bilinear 4/3, f16 result), `v = f16(s + delta[Y][X])` for R, G, B, then the game's quantise/dither step (`k = rint(fma(v, 255, dither * 2 - 1))`, `out = k / 255`, dither texture read at the pixel masked with `0x1ff`), alpha 1, flip rows when the game says so, write RGBA8 2560x1440. These are the kernels `ndve_ane_quant` and `ndve_ane_epilogue` (`msl/ane.metal`). The colour conversion that makes the input is not the GPU's: the host does it on the CPU when the picture is decoded (below).

## Tests

Structure (50 and 33 operations, names, shapes), the weight file's offsets (equal to the prototype's, total 862528 bytes) and alignment, the layer-1 permutation, the dense folding, determinism. An ignored test loads the package with coremltools on the CPU and compares it with the prototype's model on synthetic weights. The wiring has unit tests without Metal or Core ML: the slot table's lifecycle and keying and the plane bindings (`slots.rs`), and the CPU conversion against the kernel's formula written out term by term (`conv.rs`).

## Wiring: the model runs at decode time (`crates/nxkit-gpu/src/native/{ane,feed,conv,slots,offload}.rs`)

The Neural Engine path is on by default, `NXKIT_NDVE_ANE=0` turns it off; `NXKIT_NDVE_ANE_CACHE=<dir>` moves the cache (default `~/Library/Caches/nxkit/ndve-ane`: `<blob sha>-<os build>.mlpackage` and the compiled model next to it). When the NDVE run is found, a background thread writes the package (`Variant::Full`), compiles and loads it, and refuses it unless `compute_plan().all_on_neural_engine()` (the off-ANE operations go to the log). Five sets of tensors (input `x`, outputs `final` and `delta`, IOSurface f16) are made with it. The outputs are wrapped as R16Float textures; the input is only ever touched by the CPU and the Neural Engine.

### Why ahead

The host decodes AV1 itself on a worker thread that is up to three pictures ahead of the game (`LOOKAHEAD`). The picture is in memory as NV12 the moment it is decoded, so the model's input can be made then, and the Neural Engine (9 ms) has up to three picture times before the game's frame needs the result. The game's frame adds no pass for it, and no GPU work waits for the Neural Engine.

### The flow

1. `nxkit-svc-video` gives every decoded `Frame` a number (`Frame::id`) and reports to a `PictureSink`: `decoded(frame)` when the worker holds the picture (with the decoder's lock held: the sink only queues), and `read_nv12(id, luma, chroma, ..)` when the game reads the picture as NV12 into its guest buffers (`ReadDecoderOutputImageNv12`), with the host addresses of the two planes.
2. The feeder (`feed.rs`, installed when an `_ANE` switch is on) queues the picture on its own thread. The thread converts it to the input on the CPU (`conv.rs`) and calls `predict_async`. Its completion makes the picture's result `Ready` in the table (`slots.rs`).
3. The game's NDVE dispatch is prepared (`offload.rs`, `attach_ane`): the luma texture it binds has a GPU address, which is a guest address (`address_map`). `Feed::pick` finds the picture from it (next section), then asks the table for its result. If it is `Ready` (the completion handler has already run, so no GPU wait is needed), the frame runs `ndve_ane_quant` and `ndve_ane_epilogue` in place of the Metal plan, with the slot's outputs bound; nothing else is encoded. Otherwise the Metal plan runs, exactly as without the Neural Engine path.

### How the dispatch's picture is found

The game does not sample the planes that `ReadDecoderOutputImageNv12` wrote: the dispatch's planes lie at other addresses than the ones the read reported (health `unknown_picture`: "no picture was read into the planes ..."). The game copies the picture into textures of its own. Three ways to the picture, tried in order (`matching.rs` counts them):

1. `address`: the dispatch's luma plane is the memory of a read.
2. `copy` (`trace.rs`): the walk of every frame lists its copies with the GPU addresses of both sides (`CopyDetail::src_addr`, `dst_addr`; `CopyBufferToTexture`, `CopyTextureToBuffer`, `CopyTextureToTexture`, `CopyBufferToBuffer`). `Copies` keeps the newest copy into each address across frames, and `chain(luma texture)` follows it back, up to four hops, to the buffer the picture was read into. A GPU copy that the game records is thereby seen; a CPU copy is not.
3. `fingerprint` (`print.rs`): a hash of 256 scattered luma bytes and the size, taken from the decoded picture (`Frame::with_planes`, at decode time, kept for the last 16 pictures) and from guest memory at the dispatch: the memory the copies read from (rows at the copy's row stride, or tight) and the luma texture's own storage (through the header's layout, pitch or block-linear). The positions depend on the texel, not on the layout, so a tight, a padded and a swizzled copy of one picture give one fingerprint. It finds a picture the game copied with the CPU, or through a copy the walker did not see. Pictures that are equal at all samples (black frames) share a fingerprint and the newest is taken.

Back-off: when 30 dispatches in a row match no picture, the stream is given up. The feeder then converts and predicts nothing (no fingerprint either), the dispatches count `ndve.ane_fallback(no_match)`, and the path tries again when the video service starts a decoder (`PictureSink::stream_started`, from `nn::codec`'s decoder start). A line per stream goes to the log: `nxkit: ndve.ane: stream N: X of Y dispatches matched a picture (P%): a by address, b by copy, c by fingerprint`, printed at the first match of each kind, after 60 dispatches, when the stream is given up and when the next stream starts.

### The conversion constants

The colour conversion's matrix and offsets are the game's push block of the conversion dispatch (60 bytes: matrix rows at words 0 to 2, 4 to 6 and 8 to 10, offsets at 12 to 14). They are not known at decode time, so the first NDVE dispatch's block is taken (`Feed::note_params`) and used for every picture the feeder converts after that. A different block later bumps an epoch: results made with the old one are not used (`params`).

### Slots and identity

A picture is known by its `Frame::id`, which the table keeps with its state (`Queued`, `Running`, `Ready`, `Reading` with a count of the GPU frames that read it, `Skipped` with the reason). The slot of a result is held while a GPU frame that reads it is in flight (`Hold`, released from `after_gpu`, or when the pass could not be encoded). When the last such frame has finished the result stays `Ready` so that a picture shown twice finds it again; a newer picture that needs a slot takes the oldest result that nobody reads. The planes are matched by both addresses: the game reuses its buffers, so the newest read into an address names the picture, and the game does not overwrite the memory before the GPU frame that reads it is done.

### Conversion on the CPU

`conv.rs` is the kernel's `convert_bytes` written on the CPU: `Y / 255 + offset`, chroma sampled at the pixel centre with a bilinear filter (weights 3/4 and 1/4 in each direction, clamped to the edge), `fma(V, m2, fma(Y, m0, U * m1))` for each channel, clamp, `rint(c * 255)`, and the f16 of `k / 255` from a 256-entry table. Done on the CPU rather than on a queue of its own because the picture already lies in memory, nothing is uploaded, the GPU does no extra work, and the feeder's threads (up to four, bands of rows) are free to work ahead. A result can differ from the GPU's by one code where a value lies on a rounding edge.

### Health lines

`ndve.ane` counts pictures made from the Neural Engine. `ndve.ane_fallback(reason)` counts those the Metal plan made: `warmup` (the model was still loading, or the picture was decoded before the model and the constants were known), `late` (the result was not ready when the frame was encoded, or no slot was free for it), `unknown_picture` (no picture was read into the plane the dispatch binds or into what it was copied from, and no fingerprint matches, or the picture is not 1920x1080; the text names the last addresses the game read into), `no_match` (the stream was given up after 30 such dispatches, until the next stream), `error` (loading was refused, or the conversion or the Neural Engine failed; the text says why), `params` (the conversion constants changed). With `NXKIT_NDVE_COMPARE=1` the sampled frame compares the epilogue's output with the game's chain. The first three pictures log `nxkit: ndve.ane: picture N: input written in .. ms, result after .. ms`.

Expected in the game: after `ndve.ane: ready`, `ndve.ane` rises with the pictures and the fallbacks stay at the few `warmup` pictures of the first seconds. A large `unknown_picture` means the game's NDVE input does not come from `ReadDecoderOutputImageNv12` into the addresses reported, a large `late` that the feeder or the Neural Engine is slower than the pictures.

## Idea, not built: the colour conversion inside the model

Today the CPU turns each decoded NV12 picture into the model's f16 RGB input (about 2-3 ms per picture, `conv.rs`). The
conversion could move into the Core ML program: two inputs (the luma plane and the half-size chroma plane, 8-bit), the
chroma upsample and the colour matrix as small convolutions on the Neural Engine, so the CPU only copies the planes into
the shared tensors, or rav1d writes them there directly and the CPU does nothing. The cost is exactness: the game rounds
the converted colour to 8 bits before the network, and a rounding op on the Neural Engine gave wrong results once (see
above), so the input would differ by up to about 1/255. Worth doing only when the CPU time per picture matters.
