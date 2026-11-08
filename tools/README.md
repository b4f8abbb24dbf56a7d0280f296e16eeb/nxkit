# Tools

| path | purpose |
|---|---|
| `check.sh` | all quality gates. CI runs it |
| `no-game-data.sh` | fails on big files, game or toolchain files, private paths, attribution lines and key patterns |
| `ste_check.py` | finds candidates for Simplified Technical English problems in a text file |
| `front/` | scripts for the own front end (`airc`): corpus checks, IR and library equality, speed, GPU machine code comparison. They need inputs that you provide |
| `gpudiff/` | the GPU differential harness (Swift and Metal with Python glue). See `docs/gpu-differential-testing.md` |
| `hmma-precision/` | `run.sh` shows how the GPU of this Mac sums a half `simdgroup_matrix` product. See `docs/analysis/oot-open-issues.md` |

The scripts in `front/` and `gpudiff/` read their inputs from environment variables.
They stop with a message when a variable is not set. They never use a private path.

| variable | meaning |
|---|---|
| `NXB_AIRC` | the `airc` binary. Default is `target/release/airc` |
| `NXB_PROTO` | a checkout of the Python prototype of the front end |
| `NXB_REF` | a folder with reference tools and stored reference libraries |
| `NXB_CORPUS` | a text file with one MSL file path on each line |
| `NXB_SPECS` | a folder of prepared GPU test specifications (`*.spec.json`) |
| `NXB_CAS` | the content-addressed store of a reference build |
| `NXB_HARNESS` | the folder of the harness. Default is `tools/gpudiff` |

The prototype, the reference folder and every corpus are not in this repository. You must provide them.
