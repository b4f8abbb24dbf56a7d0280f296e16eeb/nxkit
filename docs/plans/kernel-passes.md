# Kernel passes: one-draw render passes as compute work in one encoder

Status: plan, 2026-11-08. Nothing of it is built.

## Why

In Breath of the Wild a gameplay frame has about 165 render passes, and 111 of them draw exactly once: downsample,
bloom, blur and other post chains, each into a target of its own (16x16, 320x180, 800x450, ...). Every render pass is an
encoder of its own, with a tile load and store and a barrier. A Metal System Trace of a frame shows 187 encoders under
50 us that add up to about 4.7 ms, and the GPU time of a loaded scene is 12 to 15 ms against 8.5 ms right after a load.

The passes form a chain: of 176 barriers a frame, 159 wait for the pass right before and 15 for the one before that
(`NXKIT_DIAG=barr`). Finer barriers or fences gain nothing; dropping the barriers (`nobarrier` knob) gives under 8 ms
but a broken picture. What can be won is the cost of the pass boundary itself.

A measurement on this machine (M2 Max): 132 dependent dispatches with a barrier each in one compute encoder
take 0.37 ms, as one-draw render passes 4.6 ms.

## Prior art: an earlier kernel pass experiment (October 2026, not merged)

In the experiment, a pass of one draw of at most two triangles,
without depth, stencil and blending, ran as one compute dispatch that runs the pair's vertex shader for its 3 to 6
vertices, rasterizes the triangles itself (pixel centres, top-left rule, perspective-correct varyings) and runs the
fragment body per pixel, with texture writes for the colour outputs. Threads run as 2x2 quads, so implicit LOD and
`dfdx`/`dfdy` take the coarse derivatives through `quad_shuffle_xor`, as the hardware does. A chain of such passes runs
in one compute encoder with encoder-local barriers.

Its first run was worse (frame p50 8.97 -> 9.69 ms, p95 9.98 -> 13.14 ms). Its open points are the risks of this plan:

- The render passes did not drop (132 -> 133) although about 171000 kernel passes ran: the replaced passes were still
  made, or the kernels ran next to them.
- Most candidates were refused because their pass had queries.
- A colour target that a kernel writes needs the `ShaderWrite` usage, which can cost the lossless compression of the
  target and so the speed of every pass that reads it. The experiment limited it to targets of at most 512x512.
- The kernels came from regular expressions over SPIRV-Cross output, and 12 pairs had no library.

## What is different here

- Our translator (`nxb-xlate`) has its own IR and MSL emitter. A kernel entry can be emitted from the IR next to the
  vertex and fragment entries, instead of rewriting MSL text.
- The pass plan already knows each pass's draws, attachments, loads and stores (`nxkit-nvn` analyse and actions), and
  the encoder knows each draw's state from the walker detail.
- Render pipelines already compile in the background with `skip.pending`; kernel pipelines can use the same path.

## The plan

### Step 0: measure the ceiling (no behaviour change)

A diagnostic counts, per frame, the render passes that could become kernel passes, and why the others cannot:

- one draw; at most 6 vertices (arrays) or 6 indices; one instance;
- no depth or stencil test or write, no blending, no logic op, no alpha to coverage, one sample;
- all colour targets of formats that a compute shader can write (not sRGB without a manual conversion, not compressed);
- no query or counter in the pass;
- the target at most 512x512 (configurable), because of `ShaderWrite`.

With the GPU trace that maps encoders to passes (`NXKIT_PASS_LIST`), this gives the GPU time that the candidates cost now,
and so the most that the plan can win. If it is under about 1.5 ms in a loaded scene, the plan stops here.

Result in Breath of the Wild, a loaded scene (GPU 12 to 13 ms), per frame: 33 of 146 to 167 render passes are
candidates, in 5 runs, almost all tiny (twelve of 16x16, 1x1 to 96x96, three of 400x225). Of the other one-draw passes
38 blend, 16 to 26 test depth or stencil, about 10 are too large or have more vertices. At about 32 us a pass boundary
(the microbenchmark), the candidates are worth about 0.9 ms; with blending in the kernel (read the target, blend,
write) up to 71 passes, about 2 ms if they chain.

### Step 1: the encoder

- A run of consecutive candidate passes becomes one compute encoder. Each candidate is one dispatch over its target
  rectangle (viewport and scissor), with an encoder-local barrier before it when it reads what the one before wrote.
- The replaced render passes are not made at all; the stats count render passes, kernel passes and kernel encoders, so
  that "passes did not drop" shows at once.
- Targets that a kernel writes get `ShaderWrite` at creation; a texture that becomes a candidate target later is made
  again with the usage (counted, like the other re-creations).
- A check mode renders a candidate both ways on sampled frames and compares the pixels, as `NXKIT_VERIFY_CACHE` does for
  the walker.

### Step 2: the kernels

- `nxb-xlate` emits, for a vertex/fragment pair, a kernel: the vertex body for the 3 to 6 vertices (into threadgroup
  memory), the triangle test and the interpolation per pixel, the fragment body, and texture writes for the colour
  outputs. Discard, implicit LOD and derivatives follow the quad scheme.
- Pairs are known only at run time. Two ways, to decide in step 0:
  - offline: the host writes the pairs that it met (`want` list), nxbuild's shader step builds their kernels, and the
    next run uses them;
  - at run time: the shader step ships the MSL of each stage (compressed), and the host builds a pair's kernel in the
    background on first use, as it builds render pipelines now. This needs no second build, but costs disk and compile
    time.
- A pair that cannot be made (an unsupported construct) is recorded once and stays a render pass.

### Step 3: measure and tune

- GPU time and frame time in Zora's Domain and a loaded open-world scene, with and without, and the cost of `ShaderWrite`
  on the targets (a run that only adds the usage without kernels).
- A switch (`NXKIT_KPASS`, then a `game.json` key), off by default until a game renders without differences in check mode.

## Risks

- `ShaderWrite` costs more than the encoders save (an open point). Step 3 measures it apart.
- Kernels that differ in rounding or derivatives from the fragment path; the check mode finds them.
- Candidates are rarer than the count of one-draw passes suggests (queries, blending, large targets); step 0 says so
  before anything else is built.
- The nxbuild side: the kernel emitter is translator work, and the shader set of a game must be built again (offline
  way) or ship its MSL (run-time way).
