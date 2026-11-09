# Early cutoff

This document gives the key scheme of the shader step (decision W21). Each stage has its own content key. Only the translator key contains
the chain version V. A V change translates again, but compiles only the shaders with a changed MSL.

## Problem

The host loads the shader libraries from the folder `b-<V>/`. V is a hash of the host's shader chain files, plus the options string `debug=True reduced=True`. No Rust source goes into V. A cache key that holds the code of the full chain
would make a change of one line in the chain invalidate all cache
entries of all games, and the next build would compile all shaders again.

## Decision

Each stage output has a key from two parts: the version of that stage and the content hash of its inputs. Only the translator key contains V.

- A stage runs again only when its version or the content of its inputs changes.
- If the new output has the same bytes as the previous output, the keys of all subsequent stages stay the same. These stages do not run.

This is early cutoff, as in the build systems Shake, Bazel and Salsa.

| stage | store domain | version part | content part |
|---|---|---|---|
| translator | `nxb.xlate.v2` | translator identity: a hash of the label `nxb-xlate msl`, V, `nxb_xlate::msl::TRANSLATOR_ID` (now `msl-2.19`) and `NXB_TEST_XLATE_SALT`; the revisions of the features the code uses (`features`, only when it uses one) | job JSON, SHA-256 of the code bytes, V, source, paired shader, answer-table flag, dual-source and fetch-outputs-only flags, partner varyings |
| Metal compile and link | `nxb.metallib.v2` | target record (flags), compiler identity (`metal` and `metallib` versions, SDK), own front-end tag (`air-front@0.10.0+f<FORMAT>`, no crate version) | the last MSL text |
| pipeline archive batch | `nxb-archive batch 1` | architecture, platform, minimum OS, `metal-tt` version, driver (`metal-tt` or `applegpu-nt -j N`) | library bytes and the entries of the bases of the batch |

The translator stores the MSL, the reflection and the side library entries of each key. Nothing else sits between the translator and the compile.

The effect of a chain change:

- A change of the translator in `nxb-xlate` translates again only when someone increases `TRANSLATOR_ID` by hand. Without the increase the store gives the old MSL.
- A change that alters only the shaders of one feature increases the feature's revision (`nxb_xlate::msl::revision::FEATURES`). Only the shaders whose code uses the feature translate again. The feature test reads the code bytes (an Ampere word with the HMMA opcode), not the output, so the key is known before the translation.
- A release (a new workspace version) changes no key. The crate version is not in V of a build without the engine and not in the front-end id. Before 0.10.0 it was in both: a release translated and compiled every shader again.
- A change of the translator compiles again only the shaders with a changed MSL, and their archive batches.
- A new translator version with the same output translates again, but does not compile a shader.
- A V change translates all shaders again. It compiles only the shaders with a changed MSL.
- A new Xcode compiles all shaders again, because the compiler identity is part of each compile key.

## Example: Ocarina of Time, msl-2.17 to msl-2.19

The app was built at `msl-2.17` and 0.9.2, the next build ran at `msl-2.19` and 0.10.0. `msl-2.18` (bare locals, one body per vertex library) changed the MSL of nearly every shader: of 3,378 sampled sources 5 kept their MSL, and these 5 kept their library bytes. All 387 archive batches had a library with new bytes, so the archive compiled again in full. The archive keys did what they should. The crate version was a second cause: it was in V and in the front-end id, so the same build would have translated and compiled every shader also without a translator change.

## Rules

1. The output folder stays `b-<V>/`, because the host loads only that folder. Of the store keys, only the translator key contains V.
   A new V makes a new folder from compile cache hits where the MSL stays the same.
2. A chain change does not delete data. `nxb shaders` does not remove store entries. Old entries stay in the store until a prune by last use.
3. The version of a stage must include all data that changes its output, and no other data.
   Tool binaries go into the version by their content. The Rust passes are part of the translator. `TRANSLATOR_ID` is their version.
4. The reuse key of the build contains only the id of the `nxb` binary. The shader step owns the keys of each output.
5. If a pass becomes slow, it gets a key from its pass version and the hash of its input MSL, in the same scheme.

## Measurements

Measured on the reference title, Mac target, on the reference machine. The machine load was 21 to 47 during the runs.
Each run started from the store of a full build. The full build took 687 s.
The numbers are from 0.1.0. At that time the translator key `nxb.xlate.v1` did not contain V.

| change | shaders with a change | wall time |
|---|---|---|
| V changes, no other change | 0 compiles, 0 translator starts | 47 s |
| a pass changes 1 % of the pixel and compute shaders | 131 | 66 s |
| a pass changes 10 % of the pixel and compute shaders | 1,119 | 119 s |
| a pass changes 100 % of the pixel and compute shaders | 11,411 | 528 s |
| new translator build with the same output | 0 compiles | 107 s |
| new compiler identity | all | approximately the time of a full build |
| full build from an empty store | all | 687 s |

## Test hooks

These environment variables are only for the benchmark. They have no effect when they are not set.

- `NXB_TEST_PASS_SALT=<percent>` adds a comment to the MSL of that share of pixel and compute shaders. This simulates a translator change that changes the output.
- `NXB_TEST_XLATE_SALT=<s>` changes the translator id. The translator output stays the same.
- `NXB_TEST_COMPILER_SALT=<s>` changes the compiler identity.
