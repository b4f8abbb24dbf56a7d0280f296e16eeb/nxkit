//! The rewrite step (`patch_nso.py`) for `nxb build` and as `nxb rewrite <NSO> <out dir> <name>`.
//!
//! The Rust rewriter ([`nxb_rewrite`]) needs the engine's registered patches as rule files (`*.toml`). They ship
//! with the tool: `rewrite-rules` next to the executable (the release archive), or `data/rewrite-rules` of a checkout.
//! `NXB_REWRITE_RULES` (a folder of rule files) and `NXB_REWRITE_PATCHES` (the snapshot they come from, made by
//! `tools/gen_rewrite_patches.py`) override them, and a `rewrite_patches.tsv` beside `patch_nso.py` is the last
//! resort. The port refuses (`nxb rewrite` gives exit code 4 so that the caller runs the reference script, and `nxb build`
//! stops at the rewrite with the same message and exit code 1) when the engine is not the one this port was checked against: the rewriter's Python files, the patch files and `game.tsv` must hash as the snapshot says,
//! `DM_GAMES_DIR` must be empty, and an unreplayable frame step stub is
//! unsupported. A game with `.nrr` files gets its collected NROs rewritten too (see [`ensure`]).

use std::path::{Path, PathBuf};

use nxb_model::sha256;
use nxb_rewrite::{Options, RewriteError, patches::PatchTable, rewrite};

/// The SHA-256 of the engine files (`dm/tools`) this port was checked against: the engine at its pinned commit
/// (nxnative `88c7e03f`). The first `SOURCES` of them are the ones `build_src.sha256` lists, in the script's order.
/// When one differs, `nxb rewrite` stops with exit code 4 naming the file and both hashes, and `nxb build` stops at
/// the rewrite with the same message and exit code 1: the rewriter is checked against the new engine files (diff them, port what
/// changed, run the oracle on real modules) and these pins are updated with it, never skipped.
const PINNED: [(&str, &str); 13] = [
    (
        "patch_nso.py",
        "6ca3aeeda9cb608f2298926f546b7ab3488afba9dcd23ebcce0077fe2b84f009",
    ),
    (
        "nso.py",
        "58537c0eb57008f5f856da5502b5fde1290dedee9c09d14ac6b1615c1e77886b",
    ),
    (
        "nso_code.py",
        "fda401560ff836fdf25a3100f94157ba02fb63e181f84ffb29ccaa6ed3417a82",
    ),
    (
        "nso_asm.py",
        "ae1b3d83bec3720fd6c806b2c8caa559a5652ac333ad44c5e3e6ef091deb7d20",
    ),
    (
        "ue_cvar.py",
        "8f6c2129316be791376a84a57595d870cdcf23950213a772a6233c28f57e4220",
    ),
    (
        "ue_malloc.py",
        "9c2d28076b79cff87c0f30295e4e9a8c32ba2194c0c2620522eb4cd60ee66c3c",
    ),
    (
        "fps_target.py",
        "9ca20da648881beab2f94e4194dfbafcece9fa79655623ce15166704bf012647",
    ),
    (
        "ue_region.py",
        "3b2a12e97839a3505e68614ede5faf3f411f262fe527675f87c8c1a43105b675",
    ),
    (
        "ue_ring.py",
        "01835663846cff75a6bb287bb7724e842323b79b74c19d818f7dbf7873cea082",
    ),
    (
        "snowdrop_pools.py",
        "3c1cca822fbe3f0b3614489d3a50da23dd5dace6a00c4d639a9a0e4fc2095b0c",
    ),
    (
        "loader_census.py",
        "1de24790659556b4b7d14fae4ad0347cc02898920fb32db6966e67711501f81f",
    ),
    (
        "nso_match.py",
        "abf5d48242650455dd87ce9f57c777084c869f2fabb243350fac3ec86cfb2496",
    ),
    (
        "nso_syms.py",
        "5d2c5c0437f9578050382a561e93f78738b368d85ba4b04dc3bd2ef1a9c9bcb2",
    ),
];

/// How many of [`PINNED`] go into `build_src.sha256`.
const SOURCES: usize = 11;

/// Exit code of a rewrite outside this port's contract (the script runs instead).
pub const UNSUPPORTED: i32 = 4;

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn file_sha(p: &Path) -> Result<String, String> {
    std::fs::read(p)
        .map(|d| hex(&sha256(&d).0))
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Reads the rule files (`*.toml`) of a private rules folder (`NXB_REWRITE_RULES`).
fn read_rules(dir: &Path) -> Result<PatchTable, String> {
    let mut files = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| format!("rules folder {}: {e}", dir.display()))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".toml") {
            let text = std::fs::read_to_string(e.path()).map_err(|e| format!("{name}: {e}"))?;
            files.push((name, text));
        }
    }
    if files.is_empty() {
        return Err(format!("no rule files in {}", dir.display()));
    }
    PatchTable::from_toml(&files).map_err(|e| e.to_string())
}

/// The shipped rule files: `rewrite-rules` beside the executable, else `data/rewrite-rules` in a folder above the
/// executable (a checkout's `target/release`), else the checkout this crate was built from.
fn shipped_rules() -> Option<PathBuf> {
    let has = |d: &Path| d.join("files.toml").is_file();
    if let Ok(exe) = std::env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        for dir in exe.ancestors().skip(1).take(5) {
            for sub in ["rewrite-rules", "data/rewrite-rules"] {
                if has(&dir.join(sub)) {
                    return Some(dir.join(sub));
                }
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/rewrite-rules");
    has(&src).then_some(src)
}

/// `nxb rewrite-rules <snapshot.tsv> <out dir>`: writes the rule files (`rules/*.toml`) of a snapshot made by
/// `tools/gen_rewrite_patches.py`. The shipped files are `data/rewrite-rules` of the repository.
pub fn rules_main(args: &[String]) -> i32 {
    let [snap, out] = args else {
        eprintln!("usage: nxb rewrite-rules <snapshot.tsv> <out dir>");
        return 2;
    };
    let run = || -> Result<usize, String> {
        let text = std::fs::read_to_string(snap).map_err(|e| format!("{snap}: {e}"))?;
        let t = PatchTable::parse(&text).map_err(|e| e.to_string())?;
        let files = t.to_toml();
        // the files must read back as the same table
        let back = PatchTable::from_toml(&files).map_err(|e| e.to_string())?;
        if back.to_snapshot() != t.to_snapshot() {
            return Err("the rule files do not read back as the snapshot".into());
        }
        std::fs::create_dir_all(out).map_err(|e| format!("{out}: {e}"))?;
        for (n, f) in &files {
            std::fs::write(Path::new(out).join(n), f).map_err(|e| format!("{n}: {e}"))?;
        }
        Ok(files.len())
    };
    match run() {
        Ok(n) => {
            println!("{n} rule files in {out}");
            0
        }
        Err(m) => {
            eprintln!("[RED] {m}");
            1
        }
    }
}

/// The engine's patch snapshot, checked against the engine's files.
pub struct Engine {
    table: PatchTable,
    /// `(SHA-256, path relative to dm/tools)` as `build_src.sha256` lists them.
    sources: Vec<(String, String)>,
    /// The engine's profile folders (`dm/games`), for the profile keys the rewriter reads.
    games: PathBuf,
}

/// Why a rewrite did not happen.
#[derive(Debug)]
pub enum Fail {
    /// Run the reference script instead (exit code 4).
    Unsupported(String),
    /// A damaged module or a failed rule (exit code 1).
    Error(String),
}

impl Engine {
    /// Loads and verifies the snapshot of `engine` (the folder holding `dm/`).
    pub fn open(engine: &Path) -> Result<Engine, Fail> {
        let un = |m: String| Fail::Unsupported(m);
        if std::env::var_os("DM_GAMES_DIR").is_some_and(|v| !v.is_empty()) {
            return Err(un("DM_GAMES_DIR names profiles outside the engine".into()));
        }
        let tools = engine.join("dm/tools");
        let table = match std::env::var_os("NXB_REWRITE_RULES") {
            Some(dir) if !dir.is_empty() => read_rules(Path::new(&dir)).map_err(un)?,
            _ => {
                let given = std::env::var_os("NXB_REWRITE_PATCHES").map(PathBuf::from);
                let shipped = if given.is_none() {
                    shipped_rules()
                } else {
                    None
                };
                if let Some(dir) = shipped {
                    read_rules(&dir).map_err(un)?
                } else {
                    let snap = given.unwrap_or_else(|| tools.join("rewrite_patches.tsv"));
                    let text = std::fs::read_to_string(&snap)
                        .map_err(|e| un(format!("no patch snapshot {}: {e}", snap.display())))?;
                    PatchTable::parse(&text).map_err(|e| un(e.to_string()))?
                }
            }
        };
        for (rel, want) in PINNED {
            let have = file_sha(&tools.join(rel)).map_err(un)?;
            if have != want {
                return Err(un(format!(
                    "{rel} of the engine ({}) differs from the version this rewriter was checked against (pinned sha256 {want}, engine sha256 {have}): the engine changed, the rewriter must be checked against it and the pins updated",
                    tools.display()
                )));
            }
        }
        let mut sources: Vec<(String, String)> = PINNED[..SOURCES]
            .iter()
            .map(|(r, h)| ((*h).to_string(), (*r).to_string()))
            .collect();
        for (rel, want) in &table.files {
            if file_sha(&tools.join(rel)).map_err(un)? != *want {
                return Err(un(format!(
                    "{rel} changed since the patch snapshot was made"
                )));
            }
            if rel.ends_with("patches.py") {
                sources.push((want.clone(), rel.clone()));
            }
        }
        // a patch file the snapshot does not know
        if let Ok(rd) = std::fs::read_dir(engine.join("dm/games")) {
            for e in rd.flatten() {
                let p = e.path().join("patches.py");
                if p.is_file() {
                    let rel = format!("../games/{}/patches.py", e.file_name().to_string_lossy());
                    if !table.files.iter().any(|(r, _)| *r == rel) {
                        return Err(un(format!("{rel} is not in the patch snapshot")));
                    }
                }
            }
        }
        Ok(Engine {
            table,
            sources,
            games: engine.join("dm/games"),
        })
    }

    /// Rewrites one module file into `out`.`profile` is the game profile's folder name (`DM_GAME`'s base name).
    pub fn module(
        &self,
        nso: &Path,
        out: &Path,
        name: &str,
        profile: &str,
    ) -> Result<Vec<String>, Fail> {
        let data =
            std::fs::read(nso).map_err(|e| Fail::Error(format!("{}: {e}", nso.display())))?;
        let patch_front = std::env::var("DM_PATCH_FRONT").ok().and_then(|v| {
            let v = v.trim();
            v.strip_prefix("0x")
                .map_or_else(|| v.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
        });
        // the pool sizes of Snowdrop's allocator (`snowdrop_pools.profile_sizes`): the profile's key, else a default
        let (snowdrop_words, snowdrop_from) = if profile.is_empty() {
            (Vec::new(), "default, no DM_GAME")
        } else if let Ok(g) = crate::host::profile::Game::open(&self.games.join(profile)) {
            let w = g.words_or("snowdrop_pools", "");
            if w.is_empty() {
                (w, "default, the profile has no snowdrop_pools")
            } else {
                (w, "profile")
            }
        } else {
            (Vec::new(), "default, no profile")
        };
        let opts = Options {
            name: name.into(),
            patch_front,
            profile: profile.into(),
            source_hashes: self.sources.clone(),
            snowdrop_words,
            snowdrop_from: snowdrop_from.into(),
        };
        let o = rewrite(&data, &opts, &self.table).map_err(|e| match e {
            RewriteError::Unsupported(m) => Fail::Unsupported(m),
            RewriteError::Failed(m) => Fail::Error(m),
        })?;
        std::fs::create_dir_all(out).map_err(|e| Fail::Error(format!("{}: {e}", out.display())))?;
        for (file, bytes) in &o.files {
            std::fs::write(out.join(file), bytes)
                .map_err(|e| Fail::Error(format!("{file}: {e}")))?;
        }
        Ok(o.log)
    }
}

/// `nxb rewrite <NSO file> <output dir> <module name>`: `patch_nso.py`'s command line, the engine is `NXB_ENGINE`
/// (else three folders above the executable), the profile `DM_GAME`. Exit code 0, 1, or 4 (unsupported).
pub fn main(args: &[String]) -> i32 {
    let [nso, out, name] = args else {
        eprintln!("usage: nxb rewrite <NSO file> <output dir> <module name>");
        return 2;
    };
    let engine = std::env::var_os("NXB_ENGINE")
        .map(PathBuf::from)
        .unwrap_or_default();
    let profile = std::env::var("DM_GAME")
        .ok()
        .and_then(|g| {
            Path::new(g.trim_end_matches('/'))
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let run = || Engine::open(&engine)?.module(Path::new(nso), Path::new(out), name, &profile);
    match run() {
        Ok(log) => {
            for l in log {
                println!("{l}");
            }
            0
        }
        Err(Fail::Unsupported(m)) => {
            eprintln!("nxb rewrite: unsupported: {m}");
            UNSUPPORTED
        }
        Err(Fail::Error(m)) => {
            eprintln!(
                "{}",
                if m.starts_with("[RED]") {
                    m
                } else {
                    format!("[RED] {m}")
                }
            );
            1
        }
    }
}

/// `nxb nro <RomFS image> <out dir> [<nrr name>] [--missing-ok]`: `nro_collect.py`'s command line (exit code 0, 1,
/// or 4).
pub fn nro_main(args: &[String]) -> i32 {
    let missing_ok = args.iter().any(|a| a == "--missing-ok");
    let args: Vec<&String> = args.iter().filter(|a| *a != "--missing-ok").collect();
    let (Some(romfs), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: nxb nro <RomFS image> <out dir> [<nrr name>] [--missing-ok]");
        return 2;
    };
    match nxb_nro::collect(
        Path::new(romfs.as_str()),
        Path::new(out.as_str()),
        args.get(2).map(|s| s.as_str()),
        missing_ok,
    ) {
        Ok(s) => {
            println!(
                "{} NROs written, {} refused ({:.1} MB) -> {out}",
                s.written,
                s.refused,
                s.bytes as f64 / 1048576.0
            );
            0
        }
        Err(nxb_nro::Fail::Unsupported(m)) => {
            eprintln!("nxb nro: unsupported: {m}");
            UNSUPPORTED
        }
        Err(nxb_nro::Fail::Error(m)) => {
            eprintln!("[RED] {m}");
            1
        }
    }
}

/// Whether the RomFS image names a `.nrr` file (a game that loads NRO modules: left to the script).
fn romfs_has_nrr(img: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(img) else {
        return true;
    };
    let mut h = [0u8; 32];
    if f.read_exact(&mut h).is_err() || &h[..8] != b"DMROMFS1" {
        return true;
    }
    let u = |o: usize| {
        u64::from_le_bytes([
            h[o],
            h[o + 1],
            h[o + 2],
            h[o + 3],
            h[o + 4],
            h[o + 5],
            h[o + 6],
            h[o + 7],
        ])
    };
    let (off, size, n) = (u(8), u(16), u(24) as usize);
    let mut idx = vec![0u8; size as usize];
    if f.seek(SeekFrom::Start(off)).is_err()
        || f.read_exact(&mut idx).is_err()
        || idx.len() < n * 24
    {
        return true;
    }
    let names = &idx[n * 24..];
    (0..n).any(|i| {
        let e = &idx[i * 24..i * 24 + 24];
        let no = u32::from_le_bytes([e[16], e[17], e[18], e[19]]) as usize;
        let nl = usize::from(u16::from_le_bytes([e[20], e[21]]));
        names.get(no..no + nl).is_some_and(|n| n.ends_with(b".nrr"))
    })
}

/// The NROs a game registers: the rows of `nro.tsv` that the script's collection (`nro_collect.py`, run by the
/// script's rewrite stage into `nro_<package>`) wrote, as `(module name, file)`. Rows named `refused` (online SDKs
/// the host never loads) are skipped. `None` when there is no collection yet or a listed file is missing.
fn collected_nros(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    let text = std::fs::read_to_string(dir.join("nro.tsv")).ok()?;
    let mut v = Vec::new();
    for l in text.lines() {
        if l.is_empty() || l.starts_with('#') || l.starts_with("refused\t") {
            continue;
        }
        let name = l.split('\t').next()?.to_string();
        let file = dir.join(format!("{name}.nro"));
        if !file.is_file() {
            return None;
        }
        v.push((name, file));
    }
    Some(v)
}

/// The message of a build that stops at the rewrite: the rewriter has no answer for this engine or module, and a
/// later stage would need the patched modules, so there is no fallback.
fn stopped(why: &str) -> String {
    format!("nxb rewrite: unsupported: {why}")
}

/// The rewrite stage of `nxb build`: makes `<work>/patched_<package>` from the extracted ExeFS when it is not there.
/// A game with `.nrr` files in its RomFS also gets its NROs rewritten (`nro0.patched.bin` and so on, and the
/// manifest `nro.tsv` beside them, as the script does) when the script has collected them into `nro_<package>`
/// before or `nxb-nro` collects them now (the script's collection, ported). Returns a note for the console, or `None`
/// when the work folder already holds the patched modules. A case this port does not support (an engine whose
/// pinned files differ, a module outside the rewriter's contract) is an error: the build stops here with the message
/// of `nxb rewrite` (exit code 4 there), it does not go on without the patched modules.
pub fn ensure(
    engine: &Path,
    profile_dir: &Path,
    work: &Path,
    package: &str,
) -> Result<Option<String>, String> {
    ensure_with(engine, profile_dir, work, package, None)
}

/// [`ensure`] that reports the NRO collection (bytes) and the modules (count) to `p`.
pub fn ensure_with(
    engine: &Path,
    profile_dir: &Path,
    work: &Path,
    package: &str,
    p: Option<&crate::events::Progress>,
) -> Result<Option<String>, String> {
    use crate::events::Update;
    use std::sync::atomic::Ordering::Relaxed;
    let game = crate::host::profile::Game::open(profile_dir)?;
    let modules = game.words("modules")?;
    let patched = work.join(format!("patched_{package}"));
    let ex = work.join("extract").join(package);
    let nros = if romfs_has_nrr(&ex.join("RomFS.img")) {
        let dir = work.join(format!("nro_{package}"));
        if !dir.join("nro.tsv").is_file() {
            let image = ex.join("RomFS.img");
            if !image.is_file() {
                // `romfs_has_nrr` answers yes for an image it cannot open: say what is missing, not an NRO failure
                return Err(format!(
                    "no RomFS image of {package}: {} is not there (the extract stage makes one image for each version that the input holds)",
                    image.display()
                ));
            }
            // the first build: the collection of the script, in Rust (`nxb-nro`)
            let scanned = nxb_nro::Scanned::default();
            let collect = || {
                nxb_nro::collect_counted(&ex.join("RomFS.img"), &dir, None, true, Some(&scanned))
            };
            let collected = match p {
                Some(p) => {
                    p.set(Update {
                        note: Some("collecting NROs"),
                        ..Default::default()
                    });
                    p.tracked(
                        None,
                        "B",
                        || (scanned.done.load(Relaxed), scanned.total.load(Relaxed)),
                        collect,
                    )
                }
                None => collect(),
            };
            match collected {
                Ok(_) => {}
                Err(nxb_nro::Fail::Unsupported(m)) => {
                    return Err(stopped(&format!("nro collection: {m}")));
                }
                Err(nxb_nro::Fail::Error(m)) => return Err(format!("nro collection: {m}")),
            }
        }
        match collected_nros(&dir) {
            Some(n) => n,
            None => return Err(stopped("the NRO collection is incomplete")),
        }
    } else {
        Vec::new()
    };
    if modules
        .iter()
        .chain(nros.iter().map(|(n, _)| n))
        .all(|m| patched.join(format!("{m}.patched.bin")).is_file())
    {
        return Ok(None);
    }
    let eng = match Engine::open(engine) {
        Ok(e) => e,
        Err(Fail::Unsupported(m)) => return Err(stopped(&m)),
        Err(Fail::Error(m)) => return Err(m),
    };
    let profile = profile_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = work.join(format!("patched_{package}.partial"));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut todo: Vec<(String, PathBuf)> = modules
        .iter()
        .map(|m| (m.clone(), ex.join("ExeFS").join(m)))
        .collect();
    todo.extend(nros.iter().cloned());
    for (i, (m, file)) in todo.iter().enumerate() {
        if let Some(p) = p {
            p.set(Update {
                done: Some(i as u64),
                total: Some(todo.len() as u64),
                unit: Some("modules"),
                note: Some(m),
                ..Default::default()
            });
        }
        match eng.module(file, &tmp, m, &profile) {
            Ok(_) => {}
            Err(Fail::Unsupported(m)) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(stopped(&format!("{m} ({m_name})", m_name = file.display())));
            }
            Err(Fail::Error(e)) => return Err(format!("rewrite of {m}: {e}")),
        }
    }
    if !nros.is_empty() {
        let tsv = work.join(format!("nro_{package}")).join("nro.tsv");
        std::fs::copy(&tsv, tmp.join("nro.tsv")).map_err(|e| format!("nro.tsv: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&patched);
    std::fs::rename(&tmp, &patched).map_err(|e| format!("{}: {e}", patched.display()))?;
    Ok(Some(format!(
        "rewrite: {} modules and {} NROs (Rust)",
        modules.len(),
        nros.len()
    )))
}

#[cfg(test)]
mod tests {
    use super::{PatchTable, collected_nros, ensure, read_rules, shipped_rules};

    #[test]
    fn shipped_rules_are_found_and_equal_the_shipped_snapshot() {
        let dir = shipped_rules().expect("data/rewrite-rules");
        let table = read_rules(&dir).expect("rule files");
        let snap = std::fs::read_to_string(dir.join("snapshot.tsv")).expect("snapshot");
        let from_snap = PatchTable::parse(&snap).expect("parse");
        assert_eq!(table.to_snapshot(), from_snap.to_snapshot());
        let regenerated = from_snap.to_toml();
        for (name, text) in &regenerated {
            let shipped = std::fs::read_to_string(dir.join(name)).expect("rule file");
            assert_eq!(&shipped, text, "{name} is not what nxb rewrite-rules makes");
        }
        assert_eq!(
            regenerated.len() + 2,
            std::fs::read_dir(&dir).expect("dir").count()
        );
    }

    #[test]
    fn collected_nros_skip_refused_rows_and_need_every_file() {
        let d = std::env::temp_dir().join(format!("nxb-nro-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("dir");
        assert!(collected_nros(&d).is_none(), "no manifest yet");
        std::fs::write(
            d.join("nro.tsv"),
            "# name\tsha256\tsize\tnrr\twhere\nnro0\taa\t4\tx.nrr\tindex a\nrefused\tbb\t4\tx.nrr\tindex b\nnro1\tcc\t4\tx.nrr\tindex c\n",
        )
        .expect("tsv");
        assert!(collected_nros(&d).is_none(), "files missing");
        std::fs::write(d.join("nro0.nro"), b"x").expect("nro0");
        std::fs::write(d.join("nro1.nro"), b"x").expect("nro1");
        let v = collected_nros(&d).expect("complete");
        assert_eq!(
            v.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["nro0", "nro1"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_version_with_no_image_is_named_and_not_reported_as_an_nro_failure() {
        let d = std::env::temp_dir().join(format!("nxb-rewrite-noimage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let profile = d.join("0100000000000000");
        let work = d.join("work");
        std::fs::create_dir_all(&profile).expect("profile");
        std::fs::create_dir_all(&work).expect("work");
        std::fs::write(profile.join("game.tsv"), "modules\tmain\n").expect("game.tsv");
        let e = ensure(&d, &profile, &work, "v720896").expect_err("no image");
        assert!(e.contains("no RomFS image of v720896"), "{e}");
        assert!(!e.contains("nro collection"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
