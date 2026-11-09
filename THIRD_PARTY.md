# Third-party notes

The only external Rust dependencies are the `objc2` family of crates, which `nxkit-metal` uses to bind Metal and `nxkit-svc-input` uses to bind GameController and Core Haptics on macOS. They are Zlib OR Apache-2.0 OR MIT. `deps.toml` lists them with their pinned commit. `Cargo.lock` lists the other crates of the workspace only.
The crate `rav1d` (BSD-2-Clause, the Rust port of dav1d by the rav1d developers and Prossimo) is the other external dependency. `nxkit-svc-video` uses it to decode AV1 on the CPU. `deps.toml` lists it and the small crates that it brings.
The only external Rust dependencies are the `objc2` family of crates, which `nxkit-metal` uses to bind Metal and `nxkit-svc-video` uses to bind VideoToolbox, CoreVideo and CoreMedia on macOS. They are Zlib OR Apache-2.0 OR MIT. `deps.toml` lists them with their pinned commit. `Cargo.lock` lists the other crates of the workspace only.
On macOS, `nxb-sys` links the system zlib (`/usr/lib/libz`) dynamically. The shader translator front end is a Rust port of the Ryujinx
shader translator, see below. The other third-party material is the rewrite rule data below.

## Formats and algorithms written from public documentation

The code was written from public specifications, public documentation, or observation of the output of tools. No code was
taken from other implementations.

| area | where | basis |
|---|---|---|
| Mach-O code signature (SuperBlob, CodeDirectory, requirements, entitlements, `CodeResources`) | `nxb-pack` | output of Apple `codesign` on test bundles, and public headers |
| metallib container and AIR conventions | `air-bc`, `air-emit`, `air-lib` | observation of the output of Apple tools. No Apple code is copied |
| LLVM 14 bitcode | `air-bc` | the public LLVM bitcode documentation |
| NCA, NSP (PFS0), IVFC, BKTR, RomFS, NSO, NACP, SARC, BNSH, BAHS, Yaz0 | `nxb-extract`, `nxb-split`, `nxb-romfs`, `nxb-hos`, `nxb-rewrite` | public format documentation of the homebrew community. The `prod.keys` text format is the common `name = hex` layout |
| zstd decoder | `nxb-split`, `nxb-codec` | RFC 8878 |
| LZ4 block decoder | `nxb-codec`, `nxb-split` | the public LZ4 block format description |
| AES-128 (ECB, CTR, XTS with the console tweak) | `nxb-extract`, `nxb-sys` | FIPS 197, NIST SP 800-38A, IEEE 1619. The test vectors are the published ones |
| SHA-1, SHA-256 | `nxb-model`, `nxb-sys` | FIPS 180-4. The test vectors are the published ones |
| AArch64 decoding and patching | `nxb-a64`, `nxb-rewrite` | the Arm Architecture Reference Manual |
| Property lists (XML and binary) | `nxb-plist`, `nxb-pack` | public Apple documentation and observed output |
| Block-linear texture layout (mip block height, layer stride) | `nxkit-metal` | the layout rules of deko3d (`dk_image.cpp`). The code is written anew and no source is copied |

## Shader translator ported from Ryujinx

The crate `nxb-maxwell` and the front end of `nxb-xlate` (every module except `msl`) are a Rust port of the Ryujinx shader
translator (`Ryujinx.Graphics.Shader`). The port also carries the changes of the nxnative fork of that translator.
That fork keeps the Ryujinx licence file. See the section "nxnative" below. The MSL back end `nxb-xlate::msl` is new code.
Ryujinx is published under the MIT licence. This is the full notice of the licence:

```
MIT License

Copyright (c) Ryujinx Team and Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## nxnative

Parts of nxkit are ported or derived from nxnative (https://github.com/seedds/nxnative). nxnative is published under the MIT
licence, Copyright (c) 2026 seedds. This is the full notice of the licence. It applies to all the parts listed below:

```
MIT License

Copyright (c) 2026 seedds

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

The same notice is in `data/rewrite-rules/NOTICE`.

These parts of nxkit are ported or derived from nxnative:

- Rewrite rules. The rule files `data/rewrite-rules/*.toml` and `snapshot.tsv` hold data, not code. They are generated by
  `tools/gen_rewrite_patches.py` and `nxb rewrite-rules` from the patch data of nxnative (`sdk_patches.py`,
  `games/*/patches.py` and the build rows of `games/*/game.tsv`). `nxb rewrite` checks the SHA-256 of the files that the rules
  come from and refuses to run (exit code 4) when they differ. The Breath of the Wild rules in
  `titles/games/01007EF00011E000/rules/` come from `games/01007EF00011E000/patches.py` (`CUTS` and `FRAME_STEP`) and are
  made by `tools/gen_botw_rules.py`.
- Shader translator. The port of the Ryujinx translator in `nxb-maxwell` and `nxb-xlate` carries the changes of the nxnative
  fork of that translator (`dm/third_party/ryujinx`).
- Offline and network services. `nxkit-svc-offline` is a Rust port of the entries of `dm/host/hle_offline.c` and
  `dm/host/hle_net.c` that need no state of the host.
- File system. `nxkit-svc-fs` is a Rust port of the `nn::fs` take-overs of `dm/host/hle_fs.c`: the mangled names, the sizes and
  result codes, the save layout and the handling of case collisions. `nxkit-ipc` follows the message layout of `dm/host/hle_sf.c`.
- Horizon kernel. `nxkit-hos` follows `dm/host/hle_thread.c`, `dm/host/hle_svc.c`, `dm/host/thread.c`, `dm/host/hle_os.c` and
  `dm/host/hle_guestthread.c`: the lock handoff, the condition variable key rules, the address arbitration arithmetic, the
  `GetInfo` values, the memory profile numbers and the SDK take-overs. The global lock is not copied. The result codes were
  checked against Atmosphere (GPL-2.0-or-later, not copied).
- Input. `nxkit-svc-input` ports logic and data from `dm/host/hle_hid.c`, `hle_touch.c`, `pad_slots.h`, `pad_apple.h`,
  `motion_fusion.h`, `motion_apple.h`, `hle_haptics.c`, `haptics_apple.m` and `front_mac.m`. `touch.rs` also follows the gesture
  rules of yuzu (`hid_core`, `gesture_handler.cpp`). No yuzu code is copied.
- Video. `nxkit-svc-video` ports the protocols of `movie::Decoder` and of the `nn::codec` video decoders from
  `dm/host/hle_movie.c`, `hle_codec_video.c` and `hle_movie_av1.c`: slots, queues, events, error codes, format keys, pools, sync
  points, the `av1C` record and the 8-bit NV12 output. The decoder itself is new code on VideoToolbox and `rav1d`. `av1.rs` was
  written from the AV1 specification.
- Audio. `nxkit-svc-audio` follows `dm/host/hle_audio.c` and `dm/host/hle_audren.c`: the list of SDK functions that are taken
  over, the SDK layouts of the audio renderer and the prologue scan that finds them. The IPC layouts of `audout:u`, `audren:u`
  and `IAudioDevice` come from the public Switchbrew documentation.
- Pacing. `nxkit-svc-pacing` and the Breath of the Wild rules port logic and data from `dm/gfx/nvn/nvn_fstep.h`,
  `dm/host/loader.c`, `dm/host/hle_sdk16.c`, `dm/gfx/tests/t_fstep.c`, `dm/gfx/nvn/hle_nvn.c`, `dm/games/game.py`,
  `dm/host/hle_fpslock.c` and the stubs of `dm/games/01007EF00011E000/patches.py` (frame step state page, pool factor, extra
  memory, output size, variable step, load-driven step, present interval, the frame rate lock mechanism).
- System services. `nxkit-svc-system` takes values and data from `dm/host/hle_account.c`, `hle_oe.c`, `hle_time.c`, `hle_sdk16.c`,
  `hle_misc.c`, `hle_net.c`, `hle_offline.c`, `hle_friends.c`, `hle_fs.c`, `hle.h`, `hle_sdkinit.c`, `hle_mii.c`, `hle_swkbd.c`,
  `hle_web.c`, `hle_ro.c`, `hle_sf.c`, `hle_nvdrv.c`, `hle_codec.c` and `hle_lifecycle.c`. The command IDs and layouts of the
  services are from libnx (ISC licence) and from the switchbrew wiki. No code is copied from them.
- Title module data. The title module of Breath of the Wild (`titles/games/01007EF00011E000/`) carries facts that nxnative keeps
  for the same game, entered as data: the 197 NVN function names (`nvn_functions.txt`), the `shader_facts_manual` rows
  (`shader_facts.tsv`) and the values of `game.tsv` and `game.py` (`SETTINGS` and `titles/nxtitle/src/settings.rs`). The title
  modules `nxgame-botw` and `nxgame-acnh` hold two facts from `dm/host/hle_sdk16.c` and `dm/host/hle_sf.c`. The DLC handling in
  `crates/nxb-build/src/standalone/` follows the behaviour of `dm/host/hle_fs.c` and `dm/host/hle_misc.c`, not the code.
- NVN front end. `nxkit-nvn` and the `nvn` modules of `nxkit-rt` port logic and data from `dm/gfx/nvn/nvn_tex.c`, `nvn_tex.h`,
  `dm/gfx/tex/nvn_deswizzle.c`, the format table of `dm/gfx/nvn/`, `names.txt` (`data/nvn_names.txt`), the list of implemented
  names (`data/nvn_implemented.txt`), `hle_nvn.c` and `nvn_obj.h`. The guest layouts and the recording path are written anew.
- Frame encoder. `nxkit-gpu` takes enum values and tables from `dm/gfx/nvn/nvn_mtl.m` and `nvn.h` (`translate.rs`,
  `format_map.rs`). The code is written anew.
- Host program. `nxkit-host` ports the approach and the assembly of the nxnative host: the svc gate, the thread entry and leave
  (`dm/host/entry.S`), the big-stack entry (`hle_bigstack.c`), the thread slots (`thread.c`), the module mapping, relocations and
  import fill (`loader.c`), and the memory and game profile (`dm/games/game.py`, `hle_svc.c`).

The nxnative file `nvn_tex.c` credits other work. The layout algorithm comes from deko3d, `source/dk_image.cpp`. The notice of deko3d follows.
Its source is under the zlib licence.

```
Copyright (C) 2018-2020 fincs

This software is provided 'as-is', without any express or implied
warranty.  In no event will the authors be held liable for any damages
arising from the use of this software.

Permission is granted to anyone to use this software for any purpose,
including commercial applications, and to alter it and redistribute it
freely, subject to the following restrictions:

1. The origin of this software must not be misrepresented; you must not
   claim that you wrote the original software. If you use this software
   in a product, an acknowledgment in the product documentation would be
   appreciated but is not required.
2. Altered source versions must be plainly marked as such, and must not be
   misrepresented as being the original software.
3. This notice may not be removed or altered from any source distribution.
```

The header of that file also names digitalhivehub and MacNativeGames for the packaged texture layout. The Rust code does not copy their code.

## Tools that nxkit starts and does not contain

| tool | licence | use |
|---|---|---|
| Apple tools (`metal`, `metallib`, `air-opt`, `air-as`, `codesign`, `metal-tt`, `metal-pack`, `xcrun`) | Apple licence | started from your own Xcode. Not redistributed |

## Code first published under MIT

nxkit was published under the MIT licence up to version 0.9.2. Those releases stay under MIT. Later versions are under
GPL-3.0-only, see `LICENSE`. The code from that time, including the contributions of Samuel Reed and thehobbit85, keeps this
notice:

```
MIT License

Copyright (c) 2026 v0lumehi

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## If more code is ported

If more code from another project is translated into this repository, the change must add the original copyright
notice and the licence text to this file. The change must also list which files come from which source.
