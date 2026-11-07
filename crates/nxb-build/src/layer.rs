//! Games that draw with Vulkan or OpenGL instead of NVN (`build_vk_shaders` and `build_gl_shaders` of the script, with
//! `dm/gfx/tools/vk_shaders.py` and `gl_shaders.py`). The host's Vulkan layer reads `<DM_MSL_DIR>/vk/vk_shaders.metallib`
//! and `vk_shaders.tsv`, its OpenGL layer `gl/gl_shaders.*`. `nxb_layer` translates the shaders to MSL, this stage finds
//! them in the game's files, compiles the MSL with Metal's tools and writes the library and the table into
//! `<work>/shader/vk` or `<work>/shader/gl`. Such a game has no NVN shader step and no pipeline archive.
//!
//! The shaders a game makes at run time come from the capture boot (`crate::boot::captures`, `capture_shaders` of the
//! script), which writes them to `<extract>/<package>/vk_capture` or `gl_capture`. This stage reads those folders like
//! the RomFS.

use crate::Args;
use crate::events::{Count, Progress, Update};
use crate::graph::Build;
use crate::host::Runner;
use crate::host::rom::Rom;
use nxb_layer::Shader;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The graphics API a game draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gfx {
    /// NVN: the NVN shader chain (`nxb_stages`).
    Nvn,
    /// Vulkan: SPIR-V shaders, the host's Vulkan layer.
    Vulkan,
    /// OpenGL: GLSL sources, the host's GL layer.
    Gl,
}

impl Gfx {
    /// The name the script's `GFXAPI` and the profile's `graphics` key use.
    pub fn name(self) -> &'static str {
        match self {
            Gfx::Nvn => "nvn",
            Gfx::Vulkan => "vulkan",
            Gfx::Gl => "gl",
        }
    }

    fn of(name: &str) -> Gfx {
        match name {
            "vulkan" => Gfx::Vulkan,
            "gl" => Gfx::Gl,
            _ => Gfx::Nvn,
        }
    }

    /// The library folder's name and the prefix of its files.
    fn dir(self) -> &'static str {
        match self {
            Gfx::Gl => "gl",
            _ => "vk",
        }
    }
}

/// The graphics API of the game and why, as `build_sources` finds it in the packed version's ExeFS and RomFS. None
/// while the work folder has no extract of the packed version.
fn detect(b: &Build) -> Result<Option<(Gfx, String)>, String> {
    let ex = b.work.join("extract").join(&b.package);
    let (exefs, romfs) = (ex.join("ExeFS"), ex.join("RomFS.img"));
    if !exefs.join("main").is_file() || !romfs.is_file() {
        return Ok(None);
    }
    let f = crate::host::guess_facts::exefs_facts(&exefs)?;
    let rom = Rom::open(&romfs)?;
    let mut mods: Vec<String> = b
        .profile
        .1
        .get("modules")
        .map(|m| m.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if mods.is_empty() {
        mods.push("main".into());
    }
    mods.retain(|m| exefs.join(m).is_file());
    let inp = crate::host::build_sources::build_inputs(&f, &exefs, &rom, &mods)?;
    let g = crate::host::guess_src::shader_sources(&inp).graphics;
    Ok(Some((Gfx::of(&g.0), g.1)))
}

/// The graphics API of the game with the reason. Found in the extract once it is there (and kept), before that the
/// profile's `graphics` key, else NVN.
pub fn gfx_why(b: &Build) -> Result<(Gfx, String), String> {
    if let Some(g) = b.gfx.get() {
        return Ok(g.clone());
    }
    match detect(b)? {
        Some(g) => Ok(b.gfx.get_or_init(|| g).clone()),
        None => {
            let p = b
                .profile
                .1
                .get("graphics")
                .map_or("nvn", |g| g.split_whitespace().next().unwrap_or("nvn"));
            Ok((Gfx::of(p), "the profile's graphics key".into()))
        }
    }
}

/// The graphics API of the game ([`gfx_why`]).
pub fn gfx(b: &Build) -> Result<Gfx, String> {
    gfx_why(b).map(|g| g.0)
}

/// The shader library folder the app and the run command take: `b.shaders` (`b-<chain>`) for an NVN game, else
/// `<work>/shader/vk` or `<work>/shader/gl` (or a `--shaders` folder with that name).
pub fn msl_dir(b: &Build) -> Result<PathBuf, String> {
    Ok(match gfx(b)? {
        Gfx::Nvn => b.shaders.clone(),
        g if b.shaders.file_name().is_some_and(|n| n == g.dir()) => b.shaders.clone(),
        g => b.work.join("shader").join(g.dir()),
    })
}

/// `f` over `items` on `threads` threads, in order. Each finished item is one atomic add to `count` (its total is the
/// number of items), which the progress reads.
fn par_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    count: &Count,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    count.set_total(items.len() as u64);
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads.clamp(1, items.len().max(1)))
            .map(|_| {
                s.spawn(|| {
                    let mut got = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(it) = items.get(i) else { break };
                        got.push((i, f(it)));
                        count.add(1);
                    }
                    got
                })
            })
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });
    out.sort_by_key(|x| x.0);
    out.into_iter().map(|x| x.1).collect()
}

/// The images the shaders are read from: the packed version's RomFS, the capture folders of its boot in the Mac host
/// (`vk_capture`, `gl_capture`, read like a RomFS as the script's tools do), then the DLC's (`<work>/aoc`, folders or
/// `.img` files).
fn images(b: &Build) -> Vec<PathBuf> {
    let ex = b.work.join("extract").join(&b.package);
    let mut out = vec![ex.join("RomFS.img")];
    out.extend(
        ["vk_capture", "gl_capture"]
            .into_iter()
            .map(|c| ex.join(c))
            .filter(|d| d.is_dir()),
    );
    let mut dlc: Vec<PathBuf> = std::fs::read_dir(b.work.join("aoc"))
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir() || p.extension().is_some_and(|x| x == "img"))
                .collect()
        })
        .unwrap_or_default();
    dlc.sort();
    out.extend(dlc);
    out
}

/// What the translation of one game's shaders gave: the shaders with where they come from, notes for the log and the
/// sources that failed.
#[derive(Default)]
struct Translated {
    shaders: Vec<(Shader, String)>,
    notes: Vec<String>,
    failed: Vec<String>,
}

/// The SPIR-V modules of the images (`vk_shaders.py`): every one must translate, stages Metal does not have are
/// skipped and counted.
fn vulkan(imgs: &[PathBuf], threads: usize, p: &Progress) -> Result<Translated, String> {
    let mut mods: Vec<(Vec<u8>, String)> = Vec::new();
    p.set(Update {
        note: Some("reading the SPIR-V files"),
        ..Default::default()
    });
    for img in imgs {
        let rom = Rom::open(img)?;
        for f in rom.files() {
            if f.size >= 4 && nxb_layer::is_spirv(&rom.read(f, 0, 4)?) {
                mods.push((rom.read(f, 0, f.size)?, f.path.clone()));
            }
        }
    }
    if mods.is_empty() {
        return Err(format!(
            "no SPIR-V files in {} (shaders the game makes at run time come from the Vulkan capture boot)",
            imgs.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let count = Count::default();
    p.set(Update {
        note: Some("translating"),
        ..Default::default()
    });
    let done = p.counted(None, "modules", &count, || {
        par_map(&mods, threads, &count, |(m, _)| nxb_layer::vk::translate(m))
    });
    let mut t = Translated::default();
    let mut skipped = std::collections::BTreeMap::new();
    for ((_, path), r) in mods.into_iter().zip(done) {
        match r {
            Ok(nxb_layer::vk::Outcome::Shader(s)) => t.shaders.push((s, path)),
            Ok(nxb_layer::vk::Outcome::Skipped(st)) => *skipped.entry(st).or_insert(0) += 1,
            Err(e) => t.failed.push(format!("{path}: {e}")),
        }
    }
    if !skipped.is_empty() {
        t.notes.push(format!(
            "[RED] skipped shader stages Metal does not have: {}",
            skipped
                .iter()
                .map(|(k, v)| format!("{k} {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(t)
}

/// The GLSL sources of the images and of the packed version's `main` (`gl_shaders.py`): the `.shad` files of MVGL
/// archives, `.vert` and `.frag` files, and the `#version` strings of `main`'s read-only segment. A source that does
/// not translate is reported, the others make the library.
fn gl(imgs: &[PathBuf], main: &Path, threads: usize, p: &Progress) -> Result<Translated, String> {
    p.set(Update {
        note: Some("reading the GLSL sources"),
        ..Default::default()
    });
    let mut srcs: Vec<(Vec<u8>, Option<&'static str>, String)> = Vec::new();
    for img in imgs {
        let rom = Rom::open(img)?;
        for f in rom.files() {
            if f.path.ends_with(".mvgl") {
                let Ok(es) = nxb_layer::mvgl_entries(|o, n| rom.read(f, o, n as u64)) else {
                    continue;
                };
                for e in es.iter().filter(|e| e.name.ends_with(".shad")) {
                    let b = e.decode(&rom.read(f, e.offset, u64::from(e.stored))?)?;
                    let src = nxb_layer::gl::text_of(&b).to_vec();
                    srcs.push((
                        src,
                        nxb_layer::gl::mvgl_stage(&e.name),
                        format!("{}:{}", f.path, e.name),
                    ));
                }
            } else if let Some(st) = [".vert", ".frag"].into_iter().find(|x| f.path.ends_with(x)) {
                let b = rom.read(f, 0, f.size)?;
                let stage = if st == ".vert" { "vert" } else { "frag" };
                srcs.push((
                    nxb_layer::gl::text_of(&b).to_vec(),
                    Some(stage),
                    f.path.clone(),
                ));
            }
        }
    }
    if main.is_file() {
        let d = std::fs::read(main).map_err(|e| format!("{}: {e}", main.display()))?;
        let m = nxb_rewrite::nso::load(&d, &main.to_string_lossy()).map_err(|e| e.to_string())?;
        let (o, n) = m.segments[1];
        let ro = m.image.get(o..o + n).unwrap_or_default();
        for (k, s) in nxb_layer::nso_glsl(ro).into_iter().enumerate() {
            srcs.push((s, None, format!("main#{k}")));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    srcs.retain(|(s, st, _)| seen.insert((s.clone(), *st)));
    if srcs.is_empty() {
        return Err(format!(
            "no GLSL shaders in {} and {} (shaders the game puts together at run time come from the GL capture boot)",
            imgs.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" "),
            main.display()
        ));
    }
    let count = Count::default();
    p.set(Update {
        note: Some("translating"),
        ..Default::default()
    });
    let done = p.counted(None, "sources", &count, || {
        par_map(&srcs, threads, &count, |(s, st, _)| {
            nxb_layer::gl::translate_source(s, *st)
        })
    });
    let mut t = Translated::default();
    let mut bad = Vec::new();
    for ((s, _, w), r) in srcs.into_iter().zip(done) {
        match r {
            Ok(v) => t.shaders.extend(v.into_iter().map(|x| (x, w.clone()))),
            Err(e) => bad.push((nxb_layer::gl::key_of(&s), w, e)),
        }
    }
    for (key, w, e) in bad {
        if !t.shaders.iter().any(|(s, _)| s.key == key) {
            t.notes
                .push(format!("[RED] {w} ({key}) does not translate: {e}"));
            t.failed.push(w);
        }
    }
    Ok(t)
}

/// The MSL files of the shaders, one function each (the first shader of a key and stage), in `dir`.
#[cfg(test)]
fn write_msl(shaders: &[(Shader, String)], dir: &Path) -> Result<(Vec<PathBuf>, usize), String> {
    write_msl_files(shaders, dir, &Count::default())
}

/// [`write_msl_files`] with its count reported to `p`.
fn write_msl_counted(
    shaders: &[(Shader, String)],
    dir: &Path,
    p: &Progress,
) -> Result<(Vec<PathBuf>, usize), String> {
    let all = shaders
        .iter()
        .map(|(s, _)| (&s.key, s.stage))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let count = Count::default();
    count.set_total(all as u64);
    p.set(Update {
        note: Some("writing the MSL files"),
        ..Default::default()
    });
    p.counted(None, "shaders", &count, || write_msl_files(shaders, dir, &count))
}

fn write_msl_files(
    shaders: &[(Shader, String)],
    dir: &Path,
    count: &Count,
) -> Result<(Vec<PathBuf>, usize), String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut n = 0;
    for (s, _) in shaders {
        if !seen.insert((s.key.clone(), s.stage)) {
            continue;
        }
        n += 1;
        count.add(1);
        let base = format!("{}_{}", s.key, s.stage);
        for (name, text) in std::iter::once((base.clone(), &s.msl))
            .chain(s.msl_dual.iter().map(|d| (format!("{base}_d"), d)))
        {
            let p = dir.join(format!("{name}.metal"));
            std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
            out.push(p);
        }
    }
    Ok((out, n))
}

/// Compiles the MSL files with `xcrun -sdk <sdk> metal` (on `threads` threads) and links them into `lib` (`metallib`
/// with paths relative to their folder, as thousands of absolute paths under a long work folder pass `ARG_MAX`).
fn metallib(
    r: &Runner,
    msls: &[PathBuf],
    dir: &Path,
    sdk: &str,
    extra: &[&str],
    lib: &Path,
    threads: usize,
    p: &Progress,
) -> Result<(), String> {
    let count = Count::default();
    p.set(Update {
        note: Some(&format!("compiling for {sdk}")),
        ..Default::default()
    });
    let airs = p.counted(None, "shaders", &count, || par_map(msls, threads, &count, |m| {
        let air = m.with_extension(format!("{sdk}.air"));
        let mut args: Vec<std::ffi::OsString> =
            ["-sdk", sdk, "metal", "-c", "-fpreserve-invariance"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(Into::into)
                .collect();
        args.extend([m.clone().into(), "-o".into(), air.clone().into()]);
        r.run("xcrun", args, None)
            .map(|_| air)
            .map_err(|e| format!("{}: metal compile failed: {e}", m.display()))
    }));
    p.set(Update {
        note: Some(&format!("linking the {sdk} library")),
        ..Default::default()
    });
    let airs = airs.into_iter().collect::<Result<Vec<_>, _>>()?;
    if let Some(d) = lib.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let lib = std::path::absolute(lib).map_err(|e| e.to_string())?;
    let mut args: Vec<std::ffi::OsString> = vec!["-sdk".into(), sdk.into(), "metallib".into()];
    for a in &airs {
        args.push(a.strip_prefix(dir).unwrap_or(a).as_os_str().to_owned());
    }
    args.extend(["-o".into(), lib.into_os_string()]);
    r.run("xcrun", args, Some(dir)).map(|_| ())
}

/// What the reuse key covers: the translator, the inputs' identities, whether the iPhone library is made.
fn reuse_key(imgs: &[PathBuf], main: &Path, ios: bool) -> String {
    let mut s = nxb_layer::translator_id();
    for p in imgs.iter().map(PathBuf::as_path).chain([main]) {
        if let Ok(m) = std::fs::metadata(p) {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let _ = write!(s, "\n{}:{}:{t}", p.display(), m.len());
        }
    }
    let _ = write!(s, "\nios {ios}");
    nxb_model::sha256(s.as_bytes()).to_hex()
}

/// The shader stage of a Vulkan or OpenGL game: `<work>/shader/vk` (`vk_shaders.metallib`, `vk_shaders.tsv`) or
/// `<work>/shader/gl`, and with `--also-ios` the iPhone's library in `<work>/shader/ios/vk` or `ios/gl`. Reused when
/// the inputs and the translator are unchanged (`<work>/shader/<vk|gl>.key`).
pub fn stage(a: &Args, b: &Build, p: &Progress) -> Result<String, String> {
    let (g, why) = gfx_why(b)?;
    let label = if g == Gfx::Vulkan { "Vulkan" } else { "OpenGL" };
    p.say(&format!(
        "{label} game ({why}): nxb-layer translates the shaders instead of the NVN shader chain"
    ));
    let d = g.dir();
    let sh = b.work.join("shader");
    let out = msl_dir(b)?;
    let ios = sh.join("ios").join(d);
    let imgs = images(b);
    let main = b.work.join("extract").join(&b.package).join("ExeFS/main");
    let key_file = sh.join(format!("{d}.key"));
    let key = reuse_key(&imgs, &main, a.also_ios);
    let lib_name = format!("{d}_shaders.metallib");
    let tsv_name = format!("{d}_shaders.tsv");
    let have = |o: &Path| o.join(&lib_name).is_file() && o.join(&tsv_name).is_file();
    if std::fs::read_to_string(&key_file).is_ok_and(|k| k.trim() == key)
        && have(&out)
        && (!a.also_ios || have(&ios))
    {
        return Ok(format!("reused: {} (inputs unchanged)", out.display()));
    }
    let _ = std::fs::remove_file(&key_file);
    let work = sh.join(format!("{d}-work"));
    for o in [&out, &work, &ios] {
        if o.exists() {
            std::fs::remove_dir_all(o).map_err(|e| format!("{}: {e}", o.display()))?;
        }
    }
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let threads = a.threads.max(1);
    let t = match g {
        Gfx::Vulkan => vulkan(&imgs, threads, p)?,
        Gfx::Gl => gl(&imgs, &main, threads, p)?,
        Gfx::Nvn => return Err("an NVN game's shaders are the NVN shader chain's".into()),
    };
    for n in &t.notes {
        p.say(n);
    }
    if g == Gfx::Vulkan && !t.failed.is_empty() {
        let n = t.failed.len();
        let list: Vec<&str> = t.failed.iter().take(5).map(String::as_str).collect();
        return Err(format!(
            "{n} SPIR-V modules do not translate (the host cannot draw with them): {}{}",
            list.join("; "),
            if n > 5 { "; ..." } else { "" }
        ));
    }
    if t.shaders.is_empty() {
        return Err(format!("no {label} shader translates"));
    }
    let (msls, n) = write_msl_counted(&t.shaders, &work, p)?;
    let tool = nxb_layer::translator_id();
    let tsv = if g == Gfx::Vulkan {
        nxb_layer::vk::table(&t.shaders, &tool)
    } else {
        nxb_layer::gl::table(&t.shaders, &tool)
    };
    let r = Runner::from_parent()?.with_var("TMPDIR", work.as_os_str().to_owned())?;
    let mut dirs: Vec<(&Path, &str, &[&str])> = vec![(&out, "macosx", &[])];
    if a.also_ios {
        dirs.push((
            &ios,
            "iphoneos",
            &["-mios-version-min=18.0", "-fmetal-math-mode=relaxed"],
        ));
    }
    for (o, sdk, extra) in dirs {
        metallib(&r, &msls, &work, sdk, extra, &o.join(&lib_name), threads, p)?;
        let f = o.join(&tsv_name);
        std::fs::write(&f, &tsv).map_err(|e| format!("{}: {e}", f.display()))?;
        let verts = if g == Gfx::Gl {
            let v = t
                .shaders
                .iter()
                .filter(|(s, _)| s.stage == "vert")
                .map(|(s, _)| &s.key)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            format!(" ({v} vertex)")
        } else {
            String::new()
        };
        p.say(&format!(
            "{sdk}: {n} shaders{verts} -> {}",
            o.join(&lib_name).display()
        ));
    }
    std::fs::write(&key_file, &key).map_err(|e| format!("{}: {e}", key_file.display()))?;
    let _ = std::fs::remove_dir_all(&work);
    let failed = if t.failed.is_empty() {
        String::new()
    } else {
        format!(", {} sources do not translate", t.failed.len())
    };
    Ok(format!("{n} {label} shaders for the Mac{failed}"))
}

#[cfg(test)]
#[path = "layer_tests.rs"]
mod tests;
