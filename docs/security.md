# Security notes

This document lists what the tools trust, what limits the code applies, and what gaps remain.
Where a gap is open, this document says so.

## Trust model

| input | trusted? | note |
|---|---|---|
| Game packages (NSP, NCA, RomFS, NSO and the archives inside them) | no | Anyone can change these files. The tools must reject damaged files and must not crash |
| Key file (`prod.keys`) | yes | You supply it. The tools never print key values, and they zero the title keys after use |
| Command line arguments and environment | yes | They come from the person who runs the tool |
| Files of a previous build (work folder, store) | partly | The store checks sizes and checksums. A damaged entry moves to quarantine |
| Apple tools | yes | You install them. nxkit starts them with an empty environment plus an allow-list |

The tools do not check the digital signatures of packages. A package with a valid structure but wrong content is accepted.
Use packages from your own dumps only.

## What the code does against damaged input

### Rules for all readers

- Only `nxb-sys` can contain `unsafe` code. All other crates use `forbid(unsafe_code)`.
- Clippy denies `unwrap`, `expect` and `panic!` outside tests. A reader returns a typed error.
- Integer reads from a byte slice go through helpers that return zero or an error when the slice is too short.

### NSP (PFS0) and NCA

- The header size is computed in 64 bits and must be inside the file. It must be at most 64 MiB.
- A file name offset must be inside the string table. A file entry must end inside the package (`checked_add`).
- Every storage read checks the range with `checked_add`. A read past the end is an error. `Storage::read_vec` checks the range before it reserves memory.
- ExeFS file names and RomFS paths are checked before they are joined to the output folder. Empty, `.` and `..` parts, NUL bytes and absolute ExeFS names are refused (`nxb-extract/src/safe_path.rs`).
- The IVFC hash levels are at most 256 MiB each. The BKTR tables are at most 256 MiB each. The RomFS tables are at most 512 MiB each.
- The hash tree (IVFC) and the section hashes are checked. A mismatch is a "damaged" error.
- The RomFS walk has a loop guard. It stops when the visit count is more than the number of table entries.

### Compression

- zstd: the decoder rejects unknown magic, truncated blocks, and dictionaries. The output of `decompress` is at most 1 GiB. A larger output is an `Unsupported` error.
- LZ4: every copy is checked against the input and against the declared output size. The result must have the exact size.
- Yaz0: the stream must end with the declared size. A short stream is an error. The decoder reserves at most 88 bytes of memory for each input byte before it decodes.
- Inflate (deflate): the caller gives a limit, and the decoder stops when the output grows past it.
- NSO: the segment layout must fit in 256 times the file size.

### Packing and signing

- The Mach-O reader checks every load command size and every read against the file length.
- Staging paths are checked. A destination path must be relative and must not contain empty, `.` or `..` parts.
- `nxb-split/src/jobs/zip.rs` reads ZIP archives in game data.

### Child processes

- `nxb-tool` starts tools with an argument vector (no shell), an empty environment plus an allow-list, and their own process group.
- Each run has a timeout. At the timeout, the whole group gets `SIGKILL`.

## Known gaps

These gaps are open. Until they are closed, treat a package from an unknown source as dangerous.

| # | gap | effect | where |
|---|---|---|---|
| 1 | The package signatures are not checked | a changed package is accepted | `nxb-extract` |
| 2 | The deterministic robustness tests cover a small set of seeds. They are not a replacement for a coverage-guided fuzzer | other defects can exist | `crates/*/tests/robustness.rs` |
| 3 | The plist, Mach-O and BNSH/SARC readers have no dedicated robustness tests yet | other defects can exist | `nxb-pack`, `nxb-split` |

## Robustness tests

The tests `crates/nxb-extract/tests/robustness.rs` and `crates/nxb-split/tests/robustness.rs` take small synthetic inputs
and change them in many ways. The changes are single bytes, extreme 32-bit and 64-bit values, cut-off tails and random bytes
from a fixed seed. A reader can return an error. It must not panic. The tests use the standard library only, and `cargo test` runs them.
The tests do not use game data.
