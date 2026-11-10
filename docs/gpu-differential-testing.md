# GPU differential testing

`gpudiff` decides if two compiled versions of one MSL shader give the same bits on the GPU.
A is the reference (Apple's `metal -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv` and `metallib`). B is the candidate (the own front end).
The tool makes a pipeline for each entry point of A and B and gives the two the same random inputs.
Then it runs them on the GPU and compares each output word bit for bit.

The harness is in `tools/gpudiff` (Swift and Metal, no packages). Set `NXB_CAS` (the store of a reference build) and
`NXB_PROTO` (the Python prototype) to use your own corpus. Keep the corpus, the specs and the results on your machine.
Do not upload them. This repository contains no corpus.

## Procedure

1. Do `./build.sh`. This writes `bin/gpudiff`.
2. Compile the reference with `./mtlc.sh shader.metal A.metallib`.
3. For a vertex shader, do `python3 -I augment.py shader.metal shader.aug.metal`. Then compile the `.aug.metal` file for A and for B.
4. Write the spec with `python3 -I gen_spec.py shader.metal A.metallib shader.spec.json`.
5. Do `bin/gpudiff run A.metallib B.metallib shader.spec.json`.
6. For many shaders, do `bin/gpudiff batch jobs.jsonl results.jsonl --workers 6`.

Exit codes of `run`: 0 same, 1 difference or mismatch, 3 nondeterministic, 4 error. `batch` stops with exit code 5 after a GPU hang.
The primary options are `--vectors N` (default 12), `--seed S`, `--budget SEC` and `--dump DIR`.

Each result line gives a status for each shader (`same`, `diff`, `mismatch`, `nondet`, `error`, `hang`). For each entry
point and function-constant set, it gives `written_words` and the first different words with position, bit patterns,
float values and ULP distance. A case with `written_words` 0 is not a test.

## Function

- **Inputs from reflection.** `gen_spec.py` reads the bindings, function constants and vertex attributes from the reflection
  of the reference library. The MSL source adds the data that Metal does not reflect: `stage_in` members, color inputs and
  outputs, depth output, and the values that the shader compares with a function constant.
- **Seeded values.** The spec holds no data. `gpudiff` makes each byte from seed, vector and resource name.
  Thus A and B get the same bytes, and each failure is reproducible.
- **Loops and indices.** 28 % of the pixel units have loops with a data-controlled bound. Random data there causes a GPU hang.
  `msl_info.py` finds the data that controls each loop and each dynamic index, and gives it small integers.
- **One arena.** All buffers are in one 24 MiB buffer with guard zones. A read past an allocation returns zeros on this GPU (measured).
  The harness puts back the arena before each run and compares all of it after the run.
- **Reference determinism.** When A and B differ, A runs two more times and B one more time.
  If no word changes in these runs, A runs 4 more times and B 2 more times. If a word changes between runs of one library, the case is `nondet`.
- **Invariance.** All vertex entries of one shader get the same inputs. The harness compares the clip-space x, y and w bits across the entries.
  This finds the z-fighting defect class, also when A and B agree.
- **Capture twin.** `augment.py` adds a twin `<entry>_hcap` to each vertex entry. The twin stores all output words without raster.
- **GPU safety.** Each command buffer has a timeout. Each worker is a separate process, because the GPU address space is for each process.
  After a hang, `batch` stops and the runner starts again without the bad shader.

## Results

Measured on the reference machine, macOS 27.2, `metal` 32023.922. Other programs used CPU during the runs.

**Self-test A = B.** 1,271 shaders (a minimum of 200 of each entry kind, or all), 7,954 cases, 12 vectors each:
`diff` 0, `mismatch` 0, `error` 0, `hang` 0. 28 shaders (2.2 %) are `nondet`: the reference differs from itself on the same inputs.
The causes are implicit-LOD sample operations in divergent control flow and writes of more than one thread to one address.
The self-test found four harness defects. The harness has repairs for all four.

**Mutation test.** 200 shaders, one mutation each (13 operators, for example a changed constant or an inverted comparison).
191 mutants are decidable. With 12 vectors, the harness found 152 (79.6 %). With 48 vectors, it found 164 (85.9 %).
The target of 95 % is not reached. The 27 mutants that it did not find are mostly boundaries that the inputs do not touch,
values that a clamp removes, and paths that the random data does not take. Better inputs will increase the rate: knowledge of data structures, all vertex formats, and captured frames.

**Throughput.** 150 new shaders. The candidate had a different AIR, thus the Metal pipeline cache did not help.

| step | time |
|---|---|
| prepare (augment, compile the reference, spec), 8 in parallel | 90 s for 150 shaders |
| `gpudiff batch`, 6 processes, 12 vectors | 75 s for 150 shaders: 2.0 shaders each second |
| one shader | median 1.3 s, p90 5.9 s, maximum 12.4 s |
| warm (pipelines in the cache) | 1,271 shaders in 421 s: 3.0 each second |

The corpus has approximately 31,600 distinct compile inputs. A full run takes approximately 4.5 hours (derived), plus 5.3 hours one time for the specs.

## Limits

The harness decides if A and B gave the same bits on these inputs. It cannot prove equivalence in general. It does not see
code that the inputs do not reach, differences that the shader itself removes (for example by a clamp), fixed-function
state that it does not set (MSAA, blend, stencil, layers, tessellation, mesh), and defects that only real game data shows.
Shaders that are not deterministic on the GPU stay `nondet`. Captured frames from the engine can close most of these gaps.

## Gates for the front end

1. Make the specs one time for each corpus shader from the Apple library. Keep them by source hash.
2. For each pull request that changes the front end, do a test of the self-test selection (1,271 shaders).
   Use 12 vectors and `--budget 20`.
   The gate is zero `diff`, zero `mismatch`, and zero new `error` or `hang`. `nondet` is permitted only for shaders on the known list.
3. Each night, do a test of the full corpus. Report the changes from the previous night.
4. Before a release of the front end, do the night run with `--vectors 48` and a second seed.
5. Keep a list of `nondet` and `hang` shaders, each with its cause.
6. To find the cause of a difference, do `gpudiff run A B spec --vectors <n+1> --dump DIR`.
