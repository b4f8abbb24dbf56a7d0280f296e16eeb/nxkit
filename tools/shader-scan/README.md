# nxkit shader quick scan

A read-only, opt-in diagnostic for known shader support gaps. The default profile
reflects nxkit 0.10.1 (`65642bd9`); `--profile msl-test` selects the later
compiler test snapshot (`da371138`).
It needs only Python 3 (standard library) and runs no build tools. Run it **before**
an expensive full shader compilation when the build already has inspection inputs.

```sh
# Check an existing translator job file (the JSON array of job objects, not a game manifest):
python3 shader_scan.py --jobs /path/to/job-list.json

# Scan existing translated source and report libraries without a same-name source:
python3 shader_scan.py /path/to/shader-output-directory

# Combine the passes; use --json for machine-readable output:
python3 shader_scan.py --json --jobs /path/to/job-list.json /path/to/shader-output-directory
```

The translator job schema is defined by
[`nxb-xlate/src/job.rs`](https://github.com/v0lumehi/nxkit/blob/65642bd909a7f229e8119f34de60bd2224658c74/crates/nxb-xlate/src/job.rs).
The regular shader stage can write `<key><suffix>.metal`; not every producer persists
its job list or intermediate source. This helper does not extract jobs from a game
or inspect NVN machine code. For producers that keep jobs in memory, the future
integration point is the job list immediately before the shader loop: reuse that
metadata or export it explicitly, without launching the loop.

| Input | Scans | Does not infer |
| --- | --- | --- |
| Existing translator job JSON | Stage census and the pinned translator's explicit rejection of tessellation-control (`hs`) and tessellation-evaluation (`ds`) jobs, among other stage checks | Instruction or GPU behavior |
| Existing `.metal`/`.msl` | Known candidate gaps in call names and barrier flags, deduplicated per source and bucket | Preprocessing, overloads, complete language support, or GPU correctness |
| `.metallib` path | Looks for adjacent same-stem `.metal` or `.msl`; otherwise reports unexamined | Original source, whether a sidecar belongs to the binary, or binary features |

The [shader-step documentation](https://github.com/v0lumehi/nxkit/blob/65642bd909a7f229e8119f34de60bd2224658c74/docs/shader-step.md)
shows one Rust guest-to-MSL translator, followed by either nxkit’s frontend or Apple’s
toolchain to make Metal libraries. Scan the **existing source before that split**,
or a retained source sidecar whichever path made the library. A compiled-only library
is not a source file, and the helper does not try to reverse it. A format with an
explicit source payload or a separate source archive would need its own format-aware
reader before the same source scan can run; no binary conversion is assumed here.

If nxkit’s **prebuilt** `mslcheck` is available, run its parser-only check separately
on existing source files and feed its JSON-lines output into the same report:

```sh
/path/to/mslcheck -j 2 --json -l metal-files.txt > parser-verdicts.jsonl
python3 shader_scan.py --mslcheck-json parser-verdicts.jsonl /path/to/shader-output-directory
```

A parser acceptance is not approval by semantic analysis, AIR lowering, Apple’s
toolchain, or the runtime. Results are tied to the pinned coverage table and must
be refreshed as support changes. Advance to focused reproductions for hits; only
a clean compile and runtime checks establish working support.

Counts are affected jobs or source files per reason, not distinct shader IDs.
Input errors return 2 without suppressing other results. Findings alone return 0
(this is triage, not a compatibility gate). Reports include local paths; redact
them before sharing. Run the synthetic tests with `python3 -m unittest -v`.
