# Pipeline prewarm: no compile while playing

Status: runtime cache built, build-side shipping not built, 2026-11-09. Mac only.

## The problem

The build archives one unspecialized pipeline for each shader pair. A draw needs the pair with a colour state, so the first run of every scene specializes or compiles it, and the draw waits for it (`skip.pending`). PC games and Steam avoid this with a shader cache that is filled before play. The user builds an app once and does not rebuild after playing, so the cache must be filled by the host itself.

## Design

The host owns a cache next to the title (details: `docs/shader-step.md` "The runtime pipeline cache").

1. Every pipeline that the cache of `nxkit-metal` makes is captured by the pipeline data set serializer and written as an `rt-*.mtl4archive` file; its description goes into `pipelines.seen` (`nxkit-gpu/src/pipelines/record.rs`, hooked in `build_render`, the mesh builds and `Pipelines::compute`).
2. At the next start the `rt-` files are extra lookup archives (`ArchiveConfig::runtime`) and `prewarm.rs` makes every pipeline of the list under the key the encoder uses, before the first frame. Anything new compiles once and is written for the run after.
3. The folder is named by an identity of the app build, the host executable, the macOS build and the GPU; another identity removes it. The size is cut to 512 MiB and 64 files, oldest first.

Choices:

- Blocking start with a budget (20 s) over a background thread. A background list races the first frames: their draws build the same pipelines and wait. A start that is a few seconds longer is cheaper than the hitches of the first minutes. The budget keeps a list that grew over many sessions from stalling the first GPU call for long; the rest goes on in threads.
- The recorded key makes the pipeline of the start the one the first draw finds, without a copy of the hashing of `render.rs`. The price: keys depend on the host build, so the identity holds the executable's size and time and every host build starts a cache (see open points).
- Several small files over one rewritten archive: the serializer flushes what it captured (to be confirmed, see open points) and Metal cannot merge archives at run time, so a run writes few files (doubling gap) and the folder is cut by size.

## Not done

- Shipping recorded states in the build (`nxb-archive` reading a `seen` file and adding specialized descriptors to the script and the shard index): dropped because the user does not rebuild. The format in `record.rs` has what it needs (the stems, entries and the whole colour state). The batch keys would stay content keys, the index would need a section from the full description to the shard (index version 2).
- Pairs whose vertex shader has stage inputs are left out of the build's archive; they are in the list with their vertex layout and in the runtime archive after one run.

## Open points

- Whether `serializeAsArchiveAndFlushToURL` writes only what was captured since the last call or everything. The code assumes the first (a file per write, new content only). If it writes everything, the `rt-` files repeat each other and the size cut removes the oldest; check the file sizes after a long run.
- Whether the serializer captures a pipeline that Metal found in a lookup archive or made by specialization. If it does not capture a specialization, the archive holds the base and the start specializes it (still no compile). The `compiled` count of the second run tells.
- The host executable in the identity drops the cache at every host build. The binaries do not depend on the host, only the keys of `pipelines.seen` do; a constant for the key scheme could replace the executable stamp and keep the archives.
- `pipelines.seen` grows with every state a run met and is not cut (200,000 entries at most). A list entry whose archive was cut compiles at the start.
- A cache folder in the run folder is lost with the app when the app is replaced; the fallback folder in `~/Library/Caches` is not.

## Verify

1. Run the title, play the scenes that hitch, quit. Look for `runtime pipeline cache <folder>` at the start of the log and the `rt-*.mtl4archive` and `pipelines.seen` in the folder.
2. Run again: `pipeline prewarm done` should show `compiled 0` and no `pending draws` in `NXKIT_DIAG=pipe` in the first minute of the same scenes.
3. `NXKIT_PIPELINE_CACHE=0` gives the old behaviour.
