# Pipeline archive back end

**Date:** 2026-11-05.

**Question:** What does the pipeline archive stage do? Can it be faster, or can own code replace Apple's GPU code generator?

**Method:**

- Read the help text, the manual pages and the symbol and string tables of the toolchain binaries (toolchain 32023.922).
- Ran `metal-tt` with `-verbose` and `-time-report` on 40 bases.
- Read the logs of the archive stage of an earlier build.
- Compatibility survey only. The study copied no code.

## Result

**Function.** The stage compiles one Metal 4 render pipeline base for each shader pair (vertex and fragment function,
color state not specialized) into native GPU code for one GPU architecture. The host loads the archive and looks up each
base. On a miss, the device compiles the base itself.

**Process chain.** `metal-tt` starts the driver `air-tt`, which plans the work. `applegpu-nt` (with a 164 MB code-generator
plugin) translates and uses 91 % of the wall time in a 40-base sample. It has a `-j` option, but `metal-tt` does not pass it.
`air-pack` writes the output file (7.5 % of the wall time).

**Inputs and outputs.** The input is a Metal 4 pipelines script (`bases.mtl4-json`) and the AIR libraries. The output
`<arch>.mtl4a` is a Mach-O "GPU Executable" with private `AIR_*` notes and the embedded AIR libraries. For the reference
title: 18,300 bases, 497 MB. The OS refuses an archive from a different GPU compiler version. The host then compiles the bases on the device.

**Time.** The stage took 4:17 for 19 batches of 1,000 bases in series (approximately 13 ms wall for each base).
`metal-tt` on 40 bases at load 40: 2.78 s real and 5.43 s user, thus approximately 2 cores busy.
With `-j 1` and `-j 4`, the output size was different (972,400 against 1,069,488 bytes).

**Options.**

| option | effect | risk |
|---|---|---|
| no change | none | none. The stage does not run when the archive key does not change. |
| parallel batches | estimate 2x to 3x, if `applegpu-nt` does not fill all cores | memory: 2.065 GiB peak for each batch on an 8 GB Mac |
| direct `applegpu-nt -j N` | unknown | undocumented command line, output not deterministic across thread counts |
| batches by content hash | a small change compiles only its own batch | low |
| one archive piece for each base | best reuse | merge time for 18,000 pieces unknown |
| compile at run time | no build time | long first start on the device |
| own GPU code generator | none | not possible |

**Own code generator: not possible.** The code generator is a closed plugin for each GPU generation. The output format
and the ISA are private and change with each generation. The OS accepts only archives from its own compiler build.
On a miss, the device uses the same code generator. Thus an own generator saves no time.

**Subsequent measurements.** The first archive stage of `nxkit` ran `metal-tt` for each batch. It took 273 s
(measured, with load). Then each batch ran `applegpu-nt -j 1` and `air-pack`, with one batch
for each core. This took 207 s cold and 958 to 1,076 CPU seconds (1,974 with `metal-tt` for each batch). The loader probe found all 18,300 bases (measured, load 7 to 33).

## Decision

- Keep Apple's code generator. Call `applegpu-nt -j 1` and `air-pack` for each batch, with one batch for each core.
- Copy the planning byte for byte. Run the batches as scheduler tasks with a memory resource.
- Compare archives by content, because `metal-tt` output is not byte-deterministic.
- Examine content-hashed batches and the `-j` determinism before a change of the default.
- Do not write an own GPU code generator.
