# Third-party notes

The only external Rust dependencies are the `objc2` family of crates. `nxkit-metal` uses them to bind Metal, and `nxkit-svc-input` uses them to bind GameController and Core Haptics on macOS. They are Zlib OR Apache-2.0 OR MIT. `deps.toml` lists them with their pinned commit. `Cargo.lock` lists the other crates of the workspace only.
The crate `rav1d` (BSD-2-Clause, the Rust port of dav1d by the rav1d developers and Prossimo) is the other external dependency. `nxkit-svc-video` uses it to decode AV1 on the CPU. `deps.toml` lists it and the small crates that it brings.
The crate `naga` (MIT OR Apache-2.0, by the wgpu developers) translates the SPIR-V of Vulkan games to MSL at build time in `nxb-spirv`. `deps.toml` lists it and the crates that it brings, with their licences. The licence is MIT OR Apache-2.0, except `spirv` and `codespan-reporting` (Apache-2.0), `libm` and `cfg_aliases` (MIT), `foldhash` (Zlib), `zerocopy` (BSD-2-Clause OR Apache-2.0 OR MIT) and `unicode-ident` ((MIT OR Apache-2.0) AND Unicode-3.0). The build tool links them, but the host app does not.
The only external Rust dependencies are the `objc2` family of crates. `nxkit-metal` uses them to bind Metal, and `nxkit-svc-video` uses them to bind VideoToolbox, CoreVideo and CoreMedia on macOS. They are Zlib OR Apache-2.0 OR MIT. `deps.toml` lists them with their pinned commit. `Cargo.lock` lists the other crates of the workspace only.
The external Rust dependencies are listed in `deps.toml` with their reason, licence and users. The `objc2` family and `block2` bind the Apple frameworks (Metal, GameController, Core Haptics, AppKit, Core Audio, VideoToolbox, CoreVideo, CoreMedia, Core ML) for the runtime crates. They are Zlib OR Apache-2.0 OR MIT. The crate `rav1d` (BSD-2-Clause, the Rust port of dav1d by the rav1d developers and Prossimo) decodes AV1 on the CPU in `nxkit-svc-video` and brings a few small crates. `foldhash` (Zlib) gives `nxkit-gpu` and `nxkit-nvn` a fast hasher. `libz-sys` (MIT OR Apache-2.0) binds zlib for `nxb-sys` on Windows.
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
| NCA, NSP (PFS0), IVFC, BKTR, RomFS, NSO, NACP, SARC, BNSH, BAHS, Yaz0 | `nxb-extract`, `nxb-split`, `nxb-romfs`, `nxb-rewrite` | public format documentation of the homebrew community. The `prod.keys` text format is the common `name = hex` layout |
| zstd decoder | `nxb-split`, and its ZBIC variant in `nxb-codec` | RFC 8878 |
| LZ4 block decoder | `nxb-split`, `nxb-rewrite` | the public LZ4 block format description |
| AES-128 (ECB, CTR, XTS with the console tweak) | `nxb-extract`, `nxb-sys` | FIPS 197, NIST SP 800-38A, IEEE 1619. The test vectors are the published ones |
| SHA-1, SHA-256 | `nxb-model`, `nxb-sys` | FIPS 180-4. The test vectors are the published ones |
| AArch64 decoding and patching | `nxb-a64`, `nxb-rewrite` | the Arm Architecture Reference Manual |
| Property lists (XML and binary) | `nxb-pack` | public Apple documentation and observed output |
| Block-linear texture layout (mip block height, layer stride) | `nxkit-metal` | the layout rules of deko3d (`dk_image.cpp`). The code is written anew and no source is copied |
| Core ML model package (`Model.proto` / `MIL.proto` messages, `weight.bin` layout) | `nxkit-ndve::ane` | the field numbers and enum values of the coremltools `.proto` files (BSD-3-Clause, Copyright (c) 2020, Apple Inc.) read from the published format, and the output of coremltools on a test model. The `.proto` files are not vendored. The encoder is written by hand |

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

- Rewrite rules. The rule files of the title modules (`titles/games/*/rules/*.toml`) hold data, not code. They were made from the
  patch data of nxnative (`games/*/patches.py` and the build rows of `games/*/game.tsv`). The Breath of the Wild rules in
  `titles/games/01007EF00011E000/rules/` come from `games/01007EF00011E000/patches.py` (`CUTS` and `FRAME_STEP`). The licence
  notice of that patch data is in `data/rewrite-rules/NOTICE`.
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

## NX2Apple

The Home button of `nxkit-host` (`home.rs`, `sys/homeui.rs`) ports the logic of the Home menu, the quit prompt and the pause in
the background from NX2Apple's patches of the dm front end (`engine/dm/host/home_quit.h`, `home_quit_mac.h` and `front_mac.m`,
https://github.com/LNPANDY/NX2Apple, MIT licence, Copyright (c) 2026 seedds, jhaizhou, LNPANDY and NX2Apple contributors).
The look of its screens (`sys/homeviews.rs`) takes the colours, sizes and type of NX2Apple's design tokens
(`frontend/design-tokens/tokens.json`) and of its console mode's question card. The Rust code is written anew.

The shared font stand-ins of `nxb-font` port NX2Apple's `engine/dm/tools/make_shared_fonts.py` and
`engine/dm/tools/make_ext_font.py`: the character set of each type, the names of the fonts, the choice of the source fonts,
and the shapes, sizes and code points of the button icons. The cut follows the behaviour of the fontTools subsetter that the
script calls (MIT licence, Copyright (c) 2017 Just van Rossum), and the icons follow its pens. No fontTools code is copied.
`tools/gen_font_charsets.py` reads the character sets from Python's codecs and the Unicode blocks of the OS/2 range bits from
fontTools. The per-type font files that `nxkit-svc-system` reads follow `engine/dm/host/hle_sdk16.c`. The notice of NX2Apple
follows. It applies to the parts listed in this section.

```
MIT License

Copyright (c) 2026 seedds
Copyright (c) 2026 jhaizhou
Copyright (c) 2026 LNPANDY and NX2Apple contributors

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

The fonts themselves are not in this repository or in a release. `nxb build` makes them on the Mac from the macOS system
fonts Arial Unicode and Arial Rounded Bold (Monotype), and they stay in the app on that Mac.

## RenHDrum

The rumble of `nxkit-svc-input` ports code and ideas of RenHDrum (https://github.com/trzz1214/RenHDrum, Copyright (c) 2026
Three-Dola). RenHDrum is published under MIT OR Apache-2.0. nxkit uses it under the MIT licence. These parts come from it:

- `src/hd/encoding.rs` ports `src/encoding/standard.rs`: the amplitude table, the frequency codes, the 4-byte side of a
  rumble report and the motor protection that scales two strong bands down. Its tests take the golden values of RenHDrum's
  tests. RenHDrum has the table, the frequency formula and the layout of reports 0x10 and 0x01 from dekuNukem's
  Nintendo_Switch_Reverse_Engineering (`rumble_data_table.md`, `bluetooth_hid_notes.md`,
  https://github.com/dekuNukem/Nintendo_Switch_Reverse_Engineering). The subcommand 0x48 that switches the actuators on is
  from the same notes.
- `src/hd/pacer.rs` ports the pacing of `src/backend/nintendo_hd.rs`: the 15 ms spacing over USB with the strongest
  value since the last report, and the Bluetooth level 4 "Compatible" with 32 ms spacing, the newest value and a resend
  after 40 ms. The values come from `docs/research/03-bluetooth-congestion-and-pacing.md`.
- `src/haptic_plan.rs` ports the idea of the Xbox mapper (`src/xbox/mod.rs`, `classify.rs`, `rotor.rs`,
  `docs/research/10-xbox-one-rumble.md`): the band split of the grips, the start of a hit, the extra kick of a resting
  rotor with its time constants, and the trigger tick on a strong hit with its rest. The code is written anew.

This is the full notice of the licence. It applies to the parts listed in this section.

```
MIT License

Copyright (c) 2026 Three-Dola

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

## Manrope

`nxkit-host` contains the Manrope typeface (`crates/nxkit-host/assets/Manrope.ttf`, Copyright 2018 The Manrope Project
Authors, https://github.com/googlefonts/manrope) for the screens of the Home button. It is licensed under the SIL Open Font
License 1.1. The full licence is in `crates/nxkit-host/assets/Manrope-OFL.txt`.

## Tools that nxkit starts and does not contain

| tool | licence | use |
|---|---|---|
| Apple tools (`metal`, `metallib`, `air-opt`, `air-as`, `codesign`, `metal-tt`, `metal-pack`, `xcrun`) | Apple licence | started from your own Xcode. Not redistributed |

## Licence history

nxkit is under the MIT licence, see `LICENSE`. Releases up to 0.9.2 were published under MIT, releases 0.9.3 to 0.10.0
under GPL-3.0-only, and from 0.10.1 on it is MIT again.

## If more code is ported

If more code from another project is translated into this repository, the change must add the original copyright
notice and the licence text to this file. The change must also list which files come from which source.
