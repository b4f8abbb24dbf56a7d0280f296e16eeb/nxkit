# Build Breath of the Wild with nxkit

This page shows how to make the app of The Legend of Zelda: Breath of the Wild from your own game files. The build uses nxkit only. It needs no nxnative, no Python and no `--engine`.

You need a Mac with Apple silicon, Xcode command line tools and Rust 1.99 or newer. You also need the game files and your `prod.keys` file. nxkit does not contain either of them.

## What the build makes

The build makes one app bundle. The program of the app is `nxkit-host`. The game's files are inside the bundle, in `Contents/Resources/nxkit/`. Refer to [nxkit-host.md](nxkit-host.md) for the layout.

## The game files

The build needs three kinds of files. All of them are NSP files.

| file | title ID tag | required |
|---|---|---|
| base game | `[01007EF00011E000]` | yes |
| update | `[01007EF00011E800]` and `[v1114112]` | yes |
| DLC The Master Trials | `[01007EF00011F001]` | no |
| DLC The Champions' Ballad | `[01007EF00011F002]` | no |

The build finds each file by the title ID in its name and by the content of the file. Keep the tags in square brackets in the names. The text around the tags does not matter. These are the names of the files in `/Volumes/NX/games/Zelda Breath of the Wild [01007EF00011E000]/`:

```text
The Legend of Zelda Breath of the Wild [01007EF00011E000][v0] (13.48 GB).nsp
The Legend of Zelda Breath of the Wild [01007EF00011E800][v1114112] (0.94 GB).nsp
The Legend of Zelda Breath of the Wild [DLC The Master Trials] [01007EF00011F001][v196608] (1.30 GB).nsp
The Legend of Zelda Breath of the Wild [DLC The Champions Ballad] [01007EF00011F002][v196608].nsp
```

Put the files you want in one folder. The build reads every NSP in that folder. The title module of Breath of the Wild is made for the update `v1114112`. If the folder has another update, the build does not use it and says so. If the folder has no update `v1114112`, the build stops.

The keys file is `/Volumes/NX/keys/prod.keys`.

## Steps

1. Build the tools. Run this in the nxkit folder:

   ```sh
   cargo build --release -p nxb-cli -p nxkit-host
   ```

   The two programs are `target/release/nxb` and `target/release/nxkit-host`. `nxb` finds `nxkit-host` in its own folder. To use another file, set `NXB_NXKIT_HOST` to its path.

2. Check the plan. This writes no app:

   ```sh
   target/release/nxb build --dry-run \
     "/Volumes/NX/games/Zelda Breath of the Wild [01007EF00011E000]" \
     -k /Volumes/NX/keys/prod.keys --platform mac
   ```

   The output lists the base game, the update and the DLC it found, the paths and the seven stages. Check that each file is there.

3. Run the build. Give the output and the work folder on a disk with enough free space, so that nothing is written to `/Volumes/NX`. The work folder takes many GB:

   ```sh
   target/release/nxb build \
     "/Volumes/NX/games/Zelda Breath of the Wild [01007EF00011E000]" \
     -k /Volumes/NX/keys/prod.keys --platform mac \
     -o "$HOME/Games/Breath of the Wild.app" \
     --work-dir "$HOME/Games/botw-work"
   ```

4. Start the game. Open the app in Finder, or run:

   ```sh
   open "$HOME/Games/Breath of the Wild.app"
   ```

The first build takes a long time. The extract stage reads the 13 GB base game and the shader stage compiles the pipelines. A second build with the same files reuses the extract stage.

## Where the app lands

The app is the path you gave with `-o`. Without `-o`, the app is `The Legend of Zelda Breath of the Wild.app` in the folder of the game files. The build writes the app as `<name>.app.partial` first and renames it when all stages are done. A failed build can leave a `.partial` folder. Delete it. The build replaces an old app only when that app was made by `nxb build`. Otherwise it stops.

The work folder holds the extracted files. You can delete it after the build. A later build then starts from the beginning.

## The stages

1. Extract. The packages become `RomFS.img`, `ExeFS` and the shader split. The update layers on top of the base game.
2. Romfs. The files of `RomFS.img` go to `romfs/`. The build checks that `romfs/Pack/Bootup_Graphics.pack` exists. Each DLC goes to its own folder `aoc/<index>/`. The Master Trials is `aoc/1` and The Champions' Ballad is `aoc/2`. The DLC is not merged into `romfs/`. Refer to R50 in [decisions.md](decisions.md).
3. Rewrite. For each of the modules `main`, `subsdk0` and `sdk`, `nxb-rewrite` changes the svc calls, the reads of `tpidrro_el0` and the imports, and applies the patch rules of the title module.
4. Manifest. `game.json` gets the modules, the NVN function list, the services, the main thread from `main.npdm`, the memory layout and the run settings.
5. Link. `nxgame.dylib` holds the modules at their fixed addresses. `codesign` signs it ad hoc. Refer to R48.
6. Shaders. The build compiles the shaders into `shaders/` and makes `pipelines.mtl4archive` for the GPU of this Mac. `--gpu none` skips the archive.
7. Bundle. The build adds `Info.plist`, signs `nxkit-host` ad hoc and moves the app to its final path.

## What does not work yet

- The build was written without a run on the game files. The first real build can stop at a stage with a named error.
- `--platform ios` is not supported for this game. The build stops with exit code 4. Options of the old path (for example `--shaders`, `--companion-app` and `--dump`) stop the build with exit code 4 and name the option.
- A missing pipeline archive is a `[RED]` warning. The game then compiles its pipelines while it runs.
- The bundle is not sealed. Whether macOS starts it without a seal is not tested.
- The NVN function list has 197 names from one run of nxnative. It can be too short.
- Without `nxkit-host` beside `nxb` and without `NXB_NXKIT_HOST`, the build stops.
