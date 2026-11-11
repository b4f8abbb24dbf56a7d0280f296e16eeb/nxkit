# Pipelines at build time: every render pipeline the game uses comes from the archive

Status: plan, 2026-11-09. Phase 1 (vertex pulling) is built in the host and the plan, not yet measured in a game (see
"State of the pull path"); the other phases are not built. Measured on Ocarina of Time (OoT) with the build and logs of
2026-11-09 (chain `71c2f44e5744`, archive of 17:50, host before and after `d54f925`).

## Recommendation

1. **Draw every vertex shader with stage inputs through its `_bp` pull library** and archive the `_bp` base of every
   pair. This is the one change that moves the archive from 21 % to 96 % of OoT's pairs. The translator side is done
   and tested; the work is in the host (fill `dm_vlayout`, bind it) and a few lines of `plan.rs`.
2. **Keep colour state (formats, blend, write mask) as Metal 4 specialization**, not as programmable blending. Metal 4
   already makes colour state a cheap, build-time-free step from an archived unspecialized pipeline (`cache/specialize.rs`).
   Programmable blending in every fragment shader would cost GPU time on every opaque draw (no hidden surface removal,
   per-sample shading under MSAA) to save a step that is already cheap.
3. **Record the render states the game uses** (pair × colour state × samples × alpha to coverage) into the profile and
   let the build archive those pipelines fully specialized. The runtime then does an archive load per pipeline, no
   compile, no specialization.
4. **Fallback for states never seen**: a fetch-blend "ubershader" pipeline per pair and format set, archived, so a
   draw whose exact pipeline is not ready draws through it instead of being skipped. Build it only if the measurements
   of phase 0 show that specialization latency still causes visible skips after phases 1 to 3.

Order: measure (phase 0), pull (1), archive the rest of the baked state (2), recorded states (3), fallback (4).

## Where it stands (numbers)

| | value | source |
|---|---|---|
| shader pairs in the build | 34 649 (16 341 vertex, 22 329 pixel keys) | `shader_pairs.tsv` 28 178 + `pso_pairs.tsv` 6 471 |
| mesh pairs | 1 284 | `mesh_stage` in `_b.json` |
| vertex shaders with stage inputs | 10 950 of 16 341 (67 %); 1 to 12 inputs, all `float4`/`uint4`/`int4` | `vertex_inputs` |
| … of them with a `_bp` pull library | 10 950 (100 %) | `<key>_bp.metallib` |
| pairs whose vertex shader has stage inputs | 27 468 (79 %) | |
| pairs the archive can serve today (HEAD `plan.rs`: no stage inputs, no mesh) | ≈ 7 181 (21 %) | |
| pairs the archive served at runtime in the logs | 0 (`bases from archive 0`) | the 17:50 archive held `_bp`, the host asked for `_b` |
| archive of 17:50: render bases | 33 365 = every non-mesh pair, `_bp` vertex functions | `pso/mac/archive.tsv` |
| archive size and build time | 4.6 to 5.7 GB, 387 shards; ≈ 50 min cold, 10 min with every batch reused | `oot-rebuild2.out`, `oot-rebuild3.out` |
| fragment shaders writing > 1 colour | 1 787 pairs; one colour 21 150 shaders, none 56 | `raster.colors` |
| fragment shaders with an integer entry `main0_u` | 9 189 (17 316 pairs) | `entry_u` |

Runtime, the 7.7-minute session `oot-prev2.log` (host before `d54f925`, `NXKIT_DIAG=pipe`):

| | value |
|---|---|
| distinct render/mesh pipelines asked for (new jobs) | 1 431 |
| bases (pair × vertex layout × rest of state) compiled | 1 249, all refused for specialization by the point size rule (fixed in `d54f925`) |
| pipelines per base | 1.15: colour state adds few pipelines, the vertex layout made one base per pipeline |
| largest burst | 1 026 new pipelines in one 60-frame window (scene change, frame 7020) |
| draws skipped while pending | 29 077 + 56 170 + 40 120 in the three windows after it; 304 430 in the session |
| creation times | full compile 1.9 / 6.2 ms mean/max; specialization incl. base compile 29 to 394 ms mean, 9.8 s max |
| `oot-white.log` (other run) | 1 326 919 draws skipped as pending |
| pipeline memory | 1 548 MiB in `nxkit-mem` at the end of the session |

Not in the logs and needed: the number of distinct **pairs** at runtime, the distinct colour states per pair, and
whether OoT uses MSAA, alpha to coverage or ICB draws for render pipelines (`icb_draws: 0` in the four frames that
print it). Phase 0 adds the counters.

Expected after each phase, for the session above (1 431 pipelines, at most 1 249 pairs):

| | compiles from source | specializations | archive loads | skipped draws |
|---|---|---|---|---|
| today (HEAD, archive rebuilt) | ≈ 1 100 (pairs with inputs, 1 to 6 ms each) | ≈ 300 | ≈ 300 | bursts of ≈ 1 s |
| phase 1 (pull) | ≈ mesh pipelines only (tens) | 1 431 | ≤ 1 249 | 1 026 × T_spec / 6 threads per burst |
| phase 3 (recorded states) | 0 for recorded states | only states never recorded | 1 431 | load time only |
| phase 4 (fallback) | 0 | in the background | | 0 |

## 1. Vertex pulling

State of the pull path:

- Translator: complete. `variant/pull.rs` writes `_bp` for every vertex shader whose inputs are below location 16 and
  of type `float4`/`uint4`/`int4` (all of OoT's). The entry reads a `constant dm_vlayout&` at buffer 16; `dm_fetch`
  decodes every NVN vertex format the old host knew (8/16/32-bit unorm, snorm, int, float, int-as-float, 10:10:10:2,
  R11G11B10F, BGRA8), bit-identical to Metal's fetch (`t_pull.m` of the old host). It handles stride 0, instance divisors
  with the base instance, and unaligned bases (low bits move into the attribute offset). `vin_tests.rs` covers it.
- Build: `_bp.metallib` exists for all 10 950 shaders. The 17:50 archive already compiled `_bp` bases for all 33 365
  non-mesh pairs, so the archive cost is known (see the table).
- Host: built behind `NXKIT_PULL` (off by default since it stalled the GPU in the title of OoT; `NXKIT_PULL=1`
  turns it on). `programs.rs` loads
  `<key>_bp.metallib` and the `pull` field's entry for a vertex shader with a `pull` field at slot 16
  (`StageProgram::pull`); a `_bp` that does not load keeps `_b`. `pull/mod.rs` writes the 320-byte block per draw from
  `DrawDetail::streams` / `attributes` (the stream address is already the host GPU address of the guest buffer),
  `draws.rs` puts it into the sink (shared through the pass's `ArgsMemo` while unchanged) and binds it at 16 in place
  of the streams and the default vertex; ICB draws take it from their arena (`icbs.rs`). The pipeline key and
  `LAST_PIPELINE` leave the layout out for pulling vertex functions; the descriptor has an empty layout. The `pipe`
  line counts `draws pulled` and `stage_in`. Mesh draws are unchanged; the host has no transform feedback path.
- Plan: `plan.rs` archives `(<vk>_bp, pull.entry)` for a vertex shader with a `pull` field at slot 16, `(<vk>_b,
  entry)` otherwise; `left[2]` now counts only shaders with inputs that cannot pull.
- Open: the pixel test (pull vs stage_in per format class, divisor, stride 0, unaligned base) and the GPU cost
  measurement; the tests of this phase check the block against `dm_fetch.metal`'s struct and table, not pixels.

Host work:

- `programs.rs`: for a vertex shader with a `pull` field, load `<key>_bp.metallib` beside `_b` (`StageProgram::pull`).
- `pipelines/render.rs`: when pulling, use the `_bp` library and an empty `vertex_layout`. The pipeline key loses the
  vertex layout; `LAST_PIPELINE` stops comparing attributes and streams.
- Encoder: per draw, write a `dm_vlayout` (16 streams × {GPU address 8, stride 4, divisor 4} + 16 attribute words = 320
  bytes) from `DrawDetail::streams` / `attributes`. The address is the host GPU address of the guest address (the guest
  buffer's `gpu_address` + offset, as `gpu/frame.rs` does for `gmem`); the attribute word is NVN's (format 8 bits,
  offset 19, stream 5). Bind with `setVertexBytes` at 16, or from the frame's upload ring for ICB draws (ICB commands
  cannot take bytes). Reuse the block while the vertex state is unchanged. Stop binding streams at 16 + n and the
  default-vertex buffer at 30 for pulled draws.
- Residency: the pull reads through raw pointers, so the guest buffer must be in the residency set for the vertex
  stage (it is for the other stages; check, add if not).
- `plan.rs`: archive `(<vk>_bp, entry)` when the vertex shader has a pull field, `(<vk>_b, entry)` otherwise; drop the
  `left[2]` case. The host must ask for exactly what the plan archives (the bug of 17:50).

Switch: `NXKIT_PULL=1` draws through `_bp`; without it the host draws with stage_in (the `_b` library stays).
Default on once the pixel test (`nxkit-metal` `tests/pull.rs`) passes and the title of OoT runs steady with it.

Effort: host ≈ 300 lines + a pixel test (pull vs stage_in for each format class, divisor, stride 0, unaligned base),
`plan.rs` ≈ 20 lines + `mesh_pairs.rs` test update. 2 to 3 days.

Cost: Apple GPUs have no fixed-function vertex fetch; Metal compiles the vertex descriptor into a fetch prologue of
the vertex function. Pulling replaces the known-at-compile-time decode with a table lookup and a switch on a uniform
format (same for all vertices of a draw: no divergence) and loads through a pointer from a constant buffer. Expect a
few tens of ALU instructions per attribute and slightly worse load scheduling, a few percent of vertex shader time;
OoT draws ≈ 550 Metal draws per frame and is not vertex bound, so frame time should not move measurably. Measure it
(phase 0).

## 2. Blend state: specialization, not programmable blending

What Metal 4 already gives: an archived pipeline with all eight colour attachments `Unspecialized` (format, write mask,
blending state, operations, factors) is turned into the draw's pipeline by
`newRenderPipelineStateBySpecialization`, which reuses the compiled code and only builds the fragment output part. So
formats, blend and write mask need nothing at build time beyond one base per pair. The pixel test of `d54f925` shows
the results equal full compiles for straight and premultiplied alpha, a write mask and alpha to coverage.

Programmable blending (read `[[color(n)]]`, blend in the shader, blend state as per-draw data) assessed:

- Possible on all Apple GPUs (the translator already reads colour attachments for the logic-op entries `<entry>_l`,
  `variant/side.rs`, function constants 200 + n). Ordering of overlapping fragments in a pass is guaranteed for
  colour attachment reads on Apple GPUs; raster order groups are only needed for device-memory images.
- sRGB: the read returns linear values and the write encodes, as fixed blending does. Integer formats: readable, and
  blending is off for them anyway. Formats that cannot be read (none of the colour formats OoT renders to, but
  e.g. compressed or depth) keep fixed state.
- Dual-source blending: possible (both outputs are in the shader), but it is a second set of entries (`main0_d`).
- Logic ops: same mechanism; the host ignores `nvnColorStateSetLogicOp` today.
- MSAA: a shader that reads the colour attachment runs per sample. 4× MSAA costs up to 4× fragment time on those draws.
- Render passes: the read only sees the current pass; the host's pass splits do not change.
- Cost: a fragment that reads its destination counts as blended for hidden surface removal, so opaque draws lose HSR
  and overdraw is shaded in full. A per-draw "blend off" uniform does not help: the compiler sees the read either way.
  Only a function constant would, and that brings the pipeline variant back.

Verdict: use it only for the fallback pipeline (section 5), where a correct image now beats the GPU cost for the few
frames until the exact pipeline exists.

## 3. What stays baked, and how the build gets it

| state | after phase 1 | how to know it at build time |
|---|---|---|
| vertex layout | gone (pull) | |
| colour formats, blend, write mask | specialization from the pair's base | recorded states for exact pipelines (phase 3) |
| primitive class | none in the base (draws every class; required for point-size vertex functions) | |
| fragment entry (`main0` / `main0_u` / mixed) | one base per entry | the plan archives `main0_u` too when the shader has it (+ up to 17 316 bases), or only for recorded integer targets |
| sample count | a base per count | recorded; or a second base at 4× for pairs drawn into MSAA targets |
| alpha to coverage | a base with it on | recorded; or emulate in the shader through `[[sample_mask]]` (then no pipeline state) |
| ICB support | a base with it on | archive bases ICB-capable if ICB draws use the pair (phase 0 says whether) |
| depth / stencil formats | not pipeline state at all: `MTL4RenderPipelineDescriptor` has no depth or stencil format | |
| mesh pipelines | not archived (`mesh (not in the build's archive)`) | archive mesh bases (task, mesh, fragment) with unspecialized colours: 1 284 pairs |

The game's shader files tell the pairs but not the states: NVN state objects are built by game code at runtime. The
build gets the states from recorded play (phase 3), the way emulators ship pipeline caches. Without recordings, the
state space per pair is: colour states (seen 1.15 per base in OoT) × samples (1, maybe 4) × alpha to coverage (rare),
which specialization covers at runtime anyway.

## 4. Recorded states

- Host: `NXKIT_RECORD_PSO=<file>` appends one line per new pipeline: vertex key, pixel key, entry, the colour
  attachments (format, blend, mask) per slot, sample count, alpha to coverage, ICB, primitive class. Distinct lines
  only; a few hundred kB per game.
- Profile: `profile/pso_states.tsv` (evidence, merged from recordings; `nxb profile merge-states`).
- Build: `plan.rs` adds a fully specialized descriptor per recorded state beside the base of its pair; the shard of
  a pair holds both. Shards are keyed by their batch content, so new recorded states rebuild only the shards of the
  pairs they touch; the stamp covers the states file per shard.
- Host lookup order: exact pipeline in the archive → specialization of the archived base → fallback (phase 4) →
  compile.

Size: ≈ 140 kB per archived pipeline today (4.6 GB / 33 365); 1 431 recorded states add ≈ 200 MB.

Built (2026-11-11, step 3 above): the build archives each recorded render pipeline fully specialized (`nxb-archive`
`exact.rs`: colour formats, blend, write masks, vertex layout, class, samples, alpha to coverage, ICB, the descriptor
that the host builds) in batches of their own, one per stream group, listed in a second section of the shard index
(`nxkit_abi::shards`, version 2). The host asks them first (`PipelineCache::render_traced`: exact in the exact shards of
the pair, exact in the runtime archives, specialization of the base, compile), so a recorded state is an archive load.
The list's compute pipelines are archived too. Ocarina of Time, 1 676 render pipelines of the list through the host's
prewarm: 1 614 archive loads, 62 compiles (states whose vertex attribute lies beyond its stream's stride, which
`applegpu-nt` refuses), 0.3 s; with the 100 compute pipelines (all loads) and the 52 mesh pipelines (compiled, not
archivable yet) 18.6 s, which is the mesh compiles. What no archive holds is compiled at the start and written to the
runtime archive before the start reports done (the serializer does not capture specializations), so the second start
loads it. See pipeline-stages.md "Recorded states".

## 5. Fallback: the ubershader pipeline

One pipeline per (pair, colour formats, sample count) that draws any blend state, write mask and logic op correctly:

- fragment: the pair's body, then blend in the shader from a per-draw block (equation, factors, constant colour,
  mask, logic op) reading `[[color(n)]]`; the pipeline's own blending is off and its write mask all.
- vertex: `_bp` (no layout).
- formats: specialized from an archived base like any other pipeline, or archived fully for the recorded format sets.
- Host: when the exact pipeline is pending, draw with the fallback and the block; never `Skip::Pending` for a pair
  that has one. The exact pipeline replaces it when ready (the existing background builds).

Translator: a `_bu` entry per pixel shader (`<entry>_b`) built like `<entry>_l` in `side.rs`, ≈ 250 lines with the
blend equations, dual source as a second variant. Host: ≈ 200 lines. 3 to 4 days.

## 6. Costs and risks

| item | cost | risk |
|---|---|---|
| pulling, GPU | a few % of vertex shader time, expected | a format the table lacks reads zeros (stage_in today gives `Skip::Format`) |
| pulling, CPU | 320 bytes per draw when the vertex state changes | ICB draws need the block in a buffer |
| archive | back to ≈ 33 k render bases + ≈ 1.3 k mesh, 5 to 6 GB, ≈ 50 min cold | `main0_u` bases can double it: gate on recordings |
| specialization latency | unknown per pipeline; 1 026 in one burst | if ≈ 5 ms each, bursts of ≈ 1 s remain on 6 threads: phase 3/4 |
| fetch-blend fallback, GPU | per-sample shading under MSAA, no HSR, only while pending | blend bugs show only in fallback frames: a pixel test per equation |
| incremental builds | `_bp` vs `_b` changes every affected base once | none after that: shards keep their content keys |

## 7. Phases

| phase | what | switch | effort |
|---|---|---|---|
| 0 | measure (below) | `NXKIT_DIAG=pipe` counters | 1 day |
| 1 | vertex pulling in the host, `plan.rs` archives `_bp` | `NXKIT_PULL` | 2 to 3 days |
| 2 | mesh bases; `main0_u`, sample count, alpha to coverage, ICB bases where phase 0 shows them | per-kind plan flags | 2 days |
| 3 | recorded states → exact pipelines in the archive | `NXKIT_RECORD_PSO`, plan reads `pso_states.tsv` | 3 days |
| 4 | fetch-blend fallback | `NXKIT_PIPELINE_FALLBACK` | 3 to 4 days, only if needed |

## Measurements to take first (phase 0)

1. Runtime counters per session in the `pipe` line: distinct pairs, distinct (pair, colour state), sample counts,
   alpha to coverage and ICB per pipeline, and archive base hits vs misses. One OoT run through a few scene changes,
   on the archive rebuilt with HEAD's plan.
2. Specialization cost: a micro-benchmark in `nxkit-metal/tests` that takes 20 archived bases of OoT and times
   (a) archive exact load, (b) specialization to 5 colour states, (c) full compile from the functions, (d) unspecialized
   compile. Median and p99, cold and warm.
3. Pull GPU cost: a Metal test drawing 1 M vertices with 8 attributes of mixed formats through stage_in and through
   `_bp`, GPU time from the command buffer timestamps; then OoT frame GPU time with `NXKIT_PULL` on and off.
4. Fetch-blend cost (only before phase 4): a full-screen overdraw test, 10 layers, fixed blending vs `[[color(0)]]`
   blending, 1× and 4× MSAA.
