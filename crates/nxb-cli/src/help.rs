//! The command table of `nxb`: groups, one-line summaries and usage lines (`nxb help`, `nxb <command> --help`).

use std::io::Write;

/// One command of the table.
pub struct Cmd {
    /// The command name.
    pub name: &'static str,
    /// The group of the `help` listing.
    pub group: &'static str,
    /// What the command does, one line.
    pub summary: &'static str,
    /// The usage text printed by `--help`.
    pub usage: &'static str,
}

const fn c(
    name: &'static str,
    group: &'static str,
    summary: &'static str,
    usage: &'static str,
) -> Cmd {
    Cmd {
        name,
        group,
        summary,
        usage,
    }
}

/// The commands of `nxb`, in help order.
pub const NXB: &[Cmd] = &[
    c(
        "build",
        "Whole build",
        "the whole build (the reference script's command line, JSON progress events)",
        "usage: nxb build <folder of packages | .nup file | dump folder> [-k prod.keys] [--platform mac|mac-app|ios] [-o OUT] [--work-dir D] --engine DIR [--progress json|bar]\n  inputs: NSP, NSZ, XCI, XCZ and merged packages in a folder (--title-id or --app picks a game), --nup FILE, --dump FOLDER, --aoc DLC_PACKAGE (repeatable)\n  options of the reference script: --app-name --bundle-id -l/--language --env-file --motion/--no-motion --seen-shaders --allow-damaged --allow-shader-regressions --allow-other-metal --mac-sign ID --gpu ARCH|none --romfs bundle|separate --keep-extracted --show-fps --program N (one program of a multi-program application)\n  a game with no profile in the engine is surveyed and gets a provisional profile (<work>/profile/<title ID>/, or --profile-out D): --provisional (also for a game with a profile) --survey-only (survey.md only) --no-heap-check (no heap and region check: they boot the game on the Mac, which nxb build does not do; without the option such a game stops with exit 4)\n  title resource patches: --resource-patch OPTION=VALUE|stock (repeatable; Ocarina of Time v131072: resolution-scale=S, 0 < S <= 1, pins the dynamic resolution; an archive of another version is left stock with a warning)\n  iOS: --team-id --framework --shell-framework FOLDER --prepare-xcode --xcode-workspace W\n  exit codes: 0 done, 1 error, 2 bad arguments, 3 damaged data, 4 not supported yet (a step that boots the game on the Mac), 6 missing key, 130 interrupted",
    ),
    c(
        "extract",
        "Stages",
        "extract a package (NSP, NSZ, XCI, NUP, merged, dump)",
        "usage: nxb extract --keys <prod.keys> --base <base.nsp> --out <dir> [--update <update.nsp> <dir>]... [--threads N] [--mem-mib N] [--cache-mib N] [--digests <tsv>] [--allow-damaged] [--manifest] [--program <title ID>] | --keys <prod.keys> --aoc <dlc.nsp> [<title ID>] --out <dir> | --nup <game.nup> [--work <dir>] [--version N] [--discard-images]",
    ),
    c(
        "split",
        "Stages",
        "split the shader containers of the extracted versions",
        "usage: nxb split --game <game.tsv> --pkg <dir> --map <tsv> --facts <tsv> --pairs <tsv> --pso-pairs <tsv> [--threads N] <version dir>...",
    ),
    c(
        "rewrite",
        "Stages",
        "the rewrite step of one NSO (patch_nso)",
        "usage: nxb rewrite <NSO file> <output dir> <module name>",
    ),
    c(
        "profile",
        "Stages",
        "the provisional profile of a game with no profile: guess, show, check, manifest",
        "usage: nxb profile guess --engine <engine> (--extract <dir> | --exefs <dir> --romfs <manifest|folder>) [--app <hex>] [--name <text>] [--out <folder>] [--report <survey.md>] [--census <file>] [--no-heap-check] [--strict]\n       nxb profile show <game.tsv | profile folder> [--toml]\n       nxb profile check --engine <engine> <profile folder> (--extract <dir> | --exefs <dir> --romfs <manifest|folder>)\n       nxb profile manifest <RomFS.img> <out folder>",
    ),
    c(
        "rewrite-rules",
        "Stages",
        "write the registered patches as private rule files",
        "usage: nxb rewrite-rules <snapshot.tsv> <out dir>",
    ),
    c(
        "nro",
        "Stages",
        "collect the NROs of a RomFS image (nro_collect)",
        "usage: nxb nro <RomFS image> <out dir> [<nrr name>]",
    ),
    c(
        "shaders",
        "Stages",
        "the shader step: guards, own-verdicts, build",
        "usage: nxb shaders <guards|own-verdicts|build|prune|regressions|self-check> [args]\n  nxb shaders guards <b-dir> <out-dir> [jobs]\n  nxb shaders own-verdicts <validation.jsonl> [data-dir]\n  nxb shaders build <pkg> <out> --profile D --chain V --game ID --inputs DIGEST --store D --work D [--jobs N] [key...]",
    ),
    c(
        "app",
        "Stages",
        "the Mac app: stage the files and sign them",
        "usage: nxb app <out.app> --host <dmhost> --companion-app <app> --patched <dir> --romfs <img> --msl <dir> --tables <dir> --font <dir> --title-id <id> --name <name> [--version v] [--category c] [--lang l] [--marker m] [--bundle-id id] [--html <dir>] [--pso <dir>] [--pso-pairs <tsv>] [--env-file f] [--settings <json>] [--icns <file>] [--fork-patches <file>] [--commit c] [--msl-name n] [--previous <old.app>] [--threads N] [--dest-digests <tsv>] [--program <row>]...",
    ),
    c(
        "ipa",
        "Stages",
        "write the IPA zip",
        "usage: nxb ipa <out.ipa> [--store .a:.b] [--skip <path>]... [--skip-suffix <s>]... <archive path>=<source>...",
    ),
    c(
        "pso",
        "Stages",
        "the offline pipeline archive of a title",
        "usage: nxb pso --msl <b-chain dir> --out <dir> --work <dir> --store <dir> --pairs <tsv> [--pairs <tsv>] [--depth gl|zero] [--gpu g18p[,g17p]|none] [--platform ios|macos] [--os-min 27.0] [--script-only] [--target 1000] [--tokens N]",
    ),
    c(
        "archive",
        "Stages",
        "build one pipeline archive from a script",
        "usage: nxb archive --script <bases.mtl4-json> --arch <arch> --osmin <v> --store <dir> --work <dir> -o <out.mtl4a> [--platform macos|ios] [--target 1000] [--tokens N] [--cost 2] [--mem-gib 2.2] [--budget-gib G] [--pack-threads 1] [--nt-jobs N]",
    ),
    c(
        "sign",
        "Stages",
        "the ad-hoc signer: sign, appstage, digests, cdhash",
        "usage: nxb sign sign <bundle | mach-o file> [--entitlements <plist>] [--digests <tsv>] [--identifier <id>] [--no-sign-nested] [--runtime] [--threads N] [--verbose]\n  nxb sign appstage <out.app> --manifest <tsv> [--threads N] [--entitlements <f>] [--no-hash-while-staging] [--verbose]\n  nxb sign digests <path.app> [--threads N]\n  nxb sign cdhash <mach-o file>",
    ),
    c(
        "ios",
        "Stages",
        "the iOS scripts: ipa and shell",
        "usage: nxb ios ipa [<out.ipa> | --prepare-xcode] | nxb ios shell --prepare-xcode | --build\nThe environment is the one of build_ipa.sh and build_shell.sh. NXB_ENGINE names the folder holding dm/.",
    ),
    c(
        "airc",
        "Stages",
        "the own front end compiler: emit, bc, check, build, rank and more",
        "usage: nxb airc emit|bc IN.metal [--ios] [-o OUT] | nxb airc check -l LIST [-j N] --work DIR [--ios] [--keep] [--opt] [--ref-store DIR] | nxb airc deny --results R.jsonl [-o DENY.txt] | nxb airc build -l LIST --work DIR [--ios] [--direct] | nxb airc rank -l LIST [--results R] [--top K] | nxb airc repack LIB... | nxb airc time -l LIST [--ios]",
    ),
    c(
        "cache",
        "Store",
        "export, import, prune and list a title's shader cache",
        "usage: nxb cache export <store> <out> [filters] | import <store> <file> [--cap-mb N] | prune <store> --cap-mb N | list <store> [filters]\nfilters: --device-family F --translator T --emitter E --variant V",
    ),
    c(
        "prune",
        "Store",
        "keep a store under a size cap (the same as cache prune)",
        "usage: nxb prune <store> --cap-mb N",
    ),
    c(
        "regressions",
        "Checks",
        "the shaders that built in the previous build and fail now (shader_regressions)",
        "usage: nxb regressions <library dir> <chain version>\nexit: 0 none or nothing to compare, 1 regressions, 2 no build report",
    ),
    c(
        "self-check",
        "Checks",
        "find the external tools and print their versions",
        "usage: nxb self-check\nexit: 0 all required tools found, 1 a required tool is missing",
    ),
    c(
        "version",
        "Other",
        "print the version",
        "usage: nxb version",
    ),
    c("help", "Other", "this list", "usage: nxb help [<command>]"),
];

/// The shader-step commands (`nxb shaders ...`).
pub const SHADERBUILD: &[(&str, &str)] = &[
    (
        "build",
        "usage: nxb shaders build <pkg> <out> --profile D --chain V --game ID --inputs DIGEST --store D --work D [--jobs N] [--batch N] [--batch-first N] [--scratch D] [--ios D] [--own-front off|VERDICTS [--own-allow V,..]] [key...]",
    ),
    (
        "guards",
        "usage: nxb shaders guards <b-dir> <out-dir> [jobs]",
    ),
    (
        "own-verdicts",
        "usage: nxb shaders own-verdicts <validation.jsonl> [data-dir]",
    ),
    ("prune", "usage: nxb shaders prune <store> --cap-mb N"),
    (
        "regressions",
        "usage: nxb shaders regressions <library dir> <chain version>",
    ),
    ("self-check", "usage: nxb shaders self-check"),
    ("version", "usage: nxb shaders version"),
    ("help", "usage: nxb shaders help"),
];

/// The usage line of a shaderbuild command.
pub fn shaderbuild_usage(cmd: &str) -> Option<&'static str> {
    SHADERBUILD.iter().find(|(n, _)| *n == cmd).map(|(_, u)| *u)
}

/// The commands of a binary by name.
pub fn names(table: &[Cmd]) -> Vec<&'static str> {
    table.iter().map(|c| c.name).collect()
}

/// The usage text of an `nxb` command.
pub fn usage_of(cmd: &str) -> Option<&'static str> {
    NXB.iter().find(|c| c.name == cmd).map(|c| c.usage)
}

/// The grouped command list of `nxb help`.
pub fn write_nxb_help(out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "usage: nxb <command> [args]   (nxb <command> --help for the arguments)"
    )?;
    let mut groups: Vec<&str> = Vec::new();
    for c in NXB {
        if !groups.contains(&c.group) {
            groups.push(c.group);
        }
    }
    for g in groups {
        writeln!(out, "\n{g}:")?;
        for c in NXB.iter().filter(|c| c.group == g) {
            writeln!(out, "  {:<14} {}", c.name, c.summary)?;
        }
    }
    Ok(())
}
