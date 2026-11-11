# Pipeline prewarm: no compile while playing

Status: runtime cache built (identity by key scheme); the list is kept across builds (`seen-<scheme>.list`); the build compiles the recorded states into the archive (`--pipeline-list`, the lists of the app it replaces); the start waits for all pipelines of a list that its build has not made, with a loading screen; 2026-11-11. Not yet run on a title: the compile of the recorded bases by `metal-tt` and their lookup by the host are untested outside synthetic data. Mac only.

## The problem

The build archives one unspecialized pipeline for each shader pair. A draw needs the pair with a colour state, so the first run of every scene specializes or compiles it, and the draw waits for it (`skip.pending`). PC games and Steam avoid this with a shader cache that is filled before play. The user builds an app once and does not rebuild after playing, so the cache must be filled by the host itself.

## Design

The host owns a cache next to the title (details: `docs/shader-step.md` "The runtime pipeline cache").

1. Every pipeline that the cache of `nxkit-metal` makes is captured by the pipeline data set serializer and written as an `rt-*.mtl4archive` file; its description goes into `pipelines.seen` (`nxkit-gpu/src/pipelines/record.rs`, hooked in `build_render`, the mesh builds and `Pipelines::compute`).
2. At the next start the `rt-` files are extra lookup archives (`ArchiveConfig::runtime`) and `prewarm.rs` makes every pipeline of the list under the key the encoder uses, before the first frame. Anything new compiles once and is written for the run after.
3. The archives are in a folder named by an identity of the app build, the key scheme, the macOS build and the GPU; another identity removes it. The size is cut to 512 MiB and 64 files, oldest first.
4. The list is not in that folder. It is kept in `pipecache/seen-<scheme>.list` across builds, because it depends on the key scheme only. A rebuild drops the archives (binaries of the old build) but starts with the full list.
5. The build side: `nxb build` reads the list of the app it replaces (or `--pipeline-list`) and adds a base for each recorded state that the plain base cannot serve (vertex layout, samples, alpha to coverage, ICB support) to the archive script, so the new app ships them compiled for the GPU. The host finds them in the archive and specializes them.
6. A start with a list that its build has not made waits for all of it (a loading screen shows the count), then writes `prewarm.done`.

Choices:

- Blocking start with a budget (20 s) over a background thread. A background list races the first frames: their draws build the same pipelines and wait. A start that is a few seconds longer is cheaper than the hitches of the first minutes. The budget keeps a list that grew over many sessions from stalling the first GPU call for long; the rest goes on in threads.
- The recorded key makes the pipeline of the start the one the first draw finds, without a copy of the hashing of `render.rs`. The price: the recorded keys follow the key scheme of the host (`KEY_SCHEME`, `keys.rs`), so the identity holds the scheme and a change of how a key is computed needs the constant raised. Tests pin one key of each kind (render, mesh, compute, point size), so such a change fails them.
- Several small files over one rewritten archive: the serializer flushes what it captured (to be confirmed, see open points) and Metal cannot merge archives at run time, so a run writes few files (doubling gap) and the folder is cut by size.

## Not done

- Pairs whose vertex shader has stage inputs were left out of the build's archive. They are now covered when a list of recorded states exists: the base with the vertex layout of the state is in the archive. Without a list (the first build of a title) they compile at the first start, once.
- Mesh and compute pipelines of the list are not added to the archive by the list (compute has its own list from the profile; mesh pipelines are built by the host).
- The recorded colour state is not used by the build: the host specializes colours. A state that Metal cannot specialize from the base compiles at run time.
- The shard of a state is the shard of its pair unless a very large group is cut between them; a lookup in the wrong shard misses and the pipeline compiles.

## Open points

- Whether `serializeAsArchiveAndFlushToURL` writes only what was captured since the last call or everything. The code assumes the first (a file per write, new content only). If it writes everything, the `rt-` files repeat each other and the size cut removes the oldest; check the file sizes after a long run.
- Whether the serializer captures a pipeline that Metal found in a lookup archive or made by specialization. If it does not capture a specialization, the archive holds the base and the start specializes it (still no compile). The `compiled` count of the second run tells.
- Done: the identity holds `KEY_SCHEME` and not the host executable, so a host build keeps the cache. The keys come from `DefaultHasher`, whose algorithm Rust does not fix across releases; the pinned key tests fail after such a change and the scheme is raised then.
- `pipelines.seen` grows with every state a run met and is not cut (200,000 entries at most). A list entry whose archive was cut compiles at the start.
- A cache folder in the run folder is lost with the app when the app is replaced; the fallback folder in `~/Library/Caches` is not.

## Verify

1. Run the title, play the scenes that hitch, quit. Look for `runtime pipeline cache <folder>` at the start of the log and the `rt-*.mtl4archive` and `pipelines.seen` in the folder.
2. Run again: `pipeline prewarm done` should show `compiled 0` and no `pending draws` in `NXKIT_DIAG=pipe` in the first minute of the same scenes.
3. `NXKIT_PIPELINE_CACHE=0` gives the old behaviour.
