//! `nxb build` for a title that has a title module: the run folder and the app without nxnative (H6, R21, R48 to R50).
//!
//! The stages, in order: unpack the packages (base, update, DLC) or the NUP container, put the RomFS into the run
//! folder, rewrite the modules, write `game.json`, link and sign `nxgame.dylib`, make the shaders and the pipeline
//! archive, and close the bundle. A NUP gives the RomFS as a folder in the extract's one pass (with the shader split),
//! and the romfs stage links its files into the app: no `RomFS.img`, no second copy. A title without a module still builds through nxnative, and this path leaves it alone ([`run`] answers
//! `false`).

mod bundle;
mod checks;
mod elf;
mod layout;
mod link;
mod macho;
mod manifest;
mod modules;
mod npdm;
mod nvn;
mod romfs;
mod shaders;

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;
use crate::inputs::{Inputs, Source};
use nxtitle::{Standalone, Title as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Say = Arc<Progress>;

fn io(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

/// Runs one stage: its name, a step line, and a closure that returns the value and the summary line.
fn stage<T>(
    p: &Say,
    name: &str,
    step: &str,
    f: impl FnOnce() -> Result<(T, String), String>,
) -> Result<T, String> {
    p.stage(name, step);
    match f() {
        Ok((v, s)) => {
            p.end(&s, true);
            Ok(v)
        }
        Err(e) => {
            p.end(&e, false);
            Err(e)
        }
    }
}

fn identity(p: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p)
        .map(|m| format!("{}:{}:{}", p.display(), m.len(), m.mtime()))
        .unwrap_or_default()
}

/// What the build knows about the title before it starts.
struct Game {
    id: u64,
    name: &'static str,
    st: Standalone,
    rules: &'static str,
    services: Vec<&'static str>,
    shared_font_decode: bool,
    sf_services: Vec<&'static str>,
    /// The title needs the command buffer to ignore a bind of one null range at a stream of 16 or more.
    vertex_stream_null_bind: bool,
}

/// The title module of the input's game, `None` when the game has no module or has no data for this build.
fn module(src: &Inputs, program: u64) -> Result<Option<Game>, String> {
    let id = src.app + program;
    let catalog = nxtitles::catalog()?;
    let r = catalog.resolve(&nxtitle::TitleFacts::new(nxtitle::TitleId(id)));
    let (Some(st), Some(t)) = (r.standalone(), r.title) else {
        return Ok(None);
    };
    let rules = r.title_rules().ok_or_else(|| {
        format!("the title module of {id:016X} has build data and no rewrite rules")
    })?;
    Ok(Some(Game {
        id,
        name: t.name(),
        st,
        rules,
        services: r.nxkit_services().into_iter().map(nxtitle::Service::id).collect(),
        shared_font_decode: r.shared_font_decode().is_some_and(|f| f.value),
        sf_services: r.sf_services().into_iter().map(|f| f.value).collect(),
        vertex_stream_null_bind: r
            .needs()
            .iter()
            .any(|n| n.value.capability == "vertex_stream_null_bind" && n.value.setting == "ignore"),
    }))
}

/// The folder the work folder and the output default into: the input folder, or the one holding the input file.
fn home(a: &Args) -> PathBuf {
    if a.nsp_dir.is_file() {
        a.nsp_dir.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    } else {
        a.nsp_dir.clone()
    }
}

/// The GPU architectures of the pipeline archive: `--gpu`, or this Mac's when the game has shader pairs.
fn gpus(a: &Args, work: &Path, profile: &Path) -> Result<Vec<String>, String> {
    Ok(match a.gpu.as_deref().map(str::trim) {
        Some("none") => Vec::new(),
        Some(g) if !g.is_empty() => g.split(',').map(nxb_archive::plan::arch_name).collect(),
        _ if profile.join("shader_pairs.tsv").is_file() => vec![crate::host::gpu::mac_arch(work)?],
        _ => Vec::new(),
    })
}

/// Refuses to replace something that is not an app a build made.
fn replaceable(out: &Path) -> Result<(), String> {
    if !out.exists() || out.join("Contents/MacOS/nxkit-host").is_file() {
        return Ok(());
    }
    Err(crate::exit::fail(
        2,
        format!(
            "{}: exists and is not an app this build made. -o names the app to write: remove the folder or choose another name",
            out.display()
        ),
    ))
}

/// The extract and the split, reused while the packages are as they were. Packages give `RomFS.img`, a NUP gives the
/// RomFS as a folder (`romfs/`) and no image.
fn extract(a: &Args, b: &Build, p: &Say, version: u32, marker: &str) -> Result<String, String> {
    let dir = b.work.join("extract").join(format!("v{version}"));
    let nup = b.src.source == Source::Nup;
    let key_file = b.work.join("extract.key");
    let key = std::iter::once(identity(&b.base))
        .chain(b.updates.iter().map(|u| identity(&u.1)))
        .collect::<Vec<_>>()
        .join(";");
    let romfs = if nup {
        dir.join("romfs").join(marker.trim_start_matches('/')).is_file()
    } else {
        dir.join("RomFS.img").is_file()
    };
    let have = romfs
        && dir.join("ExeFS/main.npdm").is_file()
        && std::fs::read_dir(b.work.join("shader/pkg")).is_ok_and(|mut d| d.next().is_some());
    let reused = have && std::fs::read_to_string(&key_file).is_ok_and(|k| k == key);
    let mut text = if reused {
        "reused".to_string()
    } else {
        let _ = std::fs::remove_file(&key_file);
        let (text, unsupported) = if nup {
            crate::extract_stage::nup_loose(a, b, p, version)?
        } else {
            let (images, why) = crate::extract_stage::packages(a, b, p)?;
            (format!("{images} images"), why)
        };
        if let Some(why) = unsupported {
            return Err(crate::exit::fail(
                4,
                format!("unsupported: the shader split needs the game's own decoder: {why}"),
            ));
        }
        std::fs::write(&key_file, &key).map_err(|e| io(&key_file, e))?;
        text
    };
    if let Some(d) = crate::extract_stage::dlc(a, b, p)? {
        text.push_str(&format!(", {d}"));
    }
    shaders::take_tables(&b.work, &b.profile.0)?;
    Ok(text)
}

/// Unpacks a RomFS image into `dest`, reporting the bytes.
fn unpack(p: &Say, img: &Path, dest: &Path, what: &str) -> Result<(usize, u64), String> {
    let _ = std::fs::remove_dir_all(dest);
    p.set(Update { step: Some(what), ..Default::default() });
    let count = crate::events::Count::default();
    p.counted(None, "B", &count, || {
        romfs::unpack(img, dest, |done, total| {
            count.done.store(done, std::sync::atomic::Ordering::Relaxed);
            count.set_total(total);
        })
    })
}

/// Unpacks the image `img` into the folder `dest` unless `dest` already holds this image: a key file beside the
/// folder records the image's size and change time. A new image is unpacked into a temporary folder that then takes
/// the place of the old one, so an interrupted unpack is never reused. The folder's files are linked into apps and
/// never written in place.
fn unpack_once(p: &Say, img: &Path, dest: &Path, what: &str) -> Result<(), String> {
    let meta = std::fs::metadata(img).map_err(|e| format!("{}: {e}", img.display()))?;
    let changed = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = format!("{} {changed}\n", meta.len());
    let key_file = {
        let mut n = dest.as_os_str().to_owned();
        n.push(".key");
        PathBuf::from(n)
    };
    if dest.is_dir() && std::fs::read_to_string(&key_file).is_ok_and(|k| k == key) {
        p.say(&format!("{what}: reused"));
        return Ok(());
    }
    let tmp = {
        let mut n = dest.as_os_str().to_owned();
        n.push(".tmp");
        PathBuf::from(n)
    };
    let _ = std::fs::remove_file(&key_file);
    unpack(p, img, &tmp, what)?;
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    std::fs::write(&key_file, key).map_err(|e| format!("{}: {e}", key_file.display()))?;
    Ok(())
}

/// Links the files of the folder `from` into `dest`, reporting the bytes.
/// Files of `overlay` (patched ones) are taken from there.
fn link(
    p: &Say,
    threads: usize,
    from: &Path,
    overlay: Option<&Path>,
    dest: &Path,
    what: &str,
) -> Result<(usize, u64), String> {
    p.set(Update { step: Some(what), ..Default::default() });
    let count = crate::events::Count::default();
    p.counted(None, "B", &count, || {
        romfs::link_tree(from, overlay, dest, threads, |done, total| {
            // the workers report out of order
            count.done.fetch_max(done, std::sync::atomic::Ordering::Relaxed);
            count.set_total(total);
        })
    })
}

fn mib(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.0)
}

/// What a dry run prints.
fn describe(t: &Game, b: &Build, host: &Path, out: &Path) -> String {
    let mut s = format!(
        "nxb build (no nxnative): {} [{:016X}] v{}\n",
        t.name, t.id, t.st.version
    );
    s.push_str(&format!("  base:      {}\n", b.base.display()));
    for (v, path) in &b.updates {
        s.push_str(&format!("  update:    v{v} {}\n", path.display()));
    }
    for (tid, v, path) in &b.src.dlc {
        s.push_str(&format!("  dlc:       {tid:016X} v{v} {}\n", path.display()));
    }
    s.push_str(&format!("  work:      {}\n", b.work.display()));
    s.push_str(&format!("  host:      {}\n", host.display()));
    s.push_str(&format!("  app:       {}\n", out.display()));
    s.push_str("  stages:    extract, romfs, rewrite, manifest, link, shaders, bundle\n");
    s
}

/// Builds the app of a title that has a title module. `Ok(false)`: the game has no module, the engine path builds it.
pub fn run(a: &Args, p: &Say) -> Result<bool, String> {
    let src = crate::inputs::discover(a)?;
    let program = u64::from(a.program.unwrap_or(0));
    let Some(t) = module(&src, program)? else {
        return Ok(false);
    };
    checks::refuse(a)?;
    if src.source == Source::Dump {
        return Err(crate::exit::fail(
            4,
            "unsupported: a build without nxnative reads packages (NSP or XCI) or a NUP container, not a dump folder",
        ));
    }
    if std::env::var_os("NXB_ENGINE").is_some() {
        p.say("NXB_ENGINE is set and is not used: this title builds without nxnative");
    }
    let version = t.st.version;
    let updates: Vec<(u32, PathBuf)> = match version {
        0 => Vec::new(),
        v => vec![
            src.updates
                .iter()
                .find(|u| u.0 == v)
                .cloned()
                .ok_or_else(|| {
                    let have: Vec<String> = src.updates.iter().map(|u| format!("v{}", u.0)).collect();
                    crate::exit::fail(
                        4,
                        format!(
                            "{}: the title module is made for v{v} and the input has no update with that version (found: {})",
                            t.name,
                            if have.is_empty() { "none".into() } else { have.join(", ") }
                        ),
                    )
                })?,
        ],
    };
    if src.updates.len() > updates.len() {
        p.say(&format!(
            "the title module is made for v{version}: the other updates in the input are not used"
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("the path of nxb: {e}"))?;
    let host = bundle::host(std::env::var("NXB_NXKIT_HOST").ok().as_deref(), &exe)?;
    let app_name = a.app_name.clone().unwrap_or_else(|| t.name.to_string());
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| home(a).join(format!("{}.app", bundle::file_name(&app_name))));
    let work = a
        .work_dir
        .clone()
        .unwrap_or_else(|| home(a).join(format!("{:016X}_build", t.id)));
    let package = format!("v{version}");
    let mut b = Build {
        title: format!("{:016X}", t.id),
        program,
        mac_run: false,
        base: src.base.clone(),
        updates,
        package: package.clone(),
        split_versions: vec![package],
        work: work.clone(),
        profile: (PathBuf::new(), Default::default()),
        shaders: work
            .join("shader/cache")
            .join(format!("b-{}", nxb_shader::chain::standalone_version())),
        out: out.clone(),
        gpus: Vec::new(),
        engine: PathBuf::new(),
        nodes: Vec::new(),
        src,
        prov: false,
        gfx: std::sync::OnceLock::new(),
    };
    if a.dry_run {
        print!("{}", describe(&t, &b, &host, &out));
        return Ok(true);
    }
    replaceable(&out)?;
    b.profile = shaders::profile(&work, &t.st, t.id, t.name)?;
    let stage_dir = {
        let mut n = out.clone().into_os_string();
        n.push(".partial");
        PathBuf::from(n)
    };
    let _ = std::fs::remove_dir_all(&stage_dir);
    let run_dir = bundle::skeleton(&stage_dir, &host, &plist(a, &t))?;
    p.plan(7);
    let nup = b.src.source == Source::Nup;
    let what = if nup { "NUP and shader split" } else { "packages and shader split" };
    stage(p, "extract", what, || {
        Ok(((), extract(a, &b, p, version, t.st.romfs_marker)?))
    })?;
    let ver_dir = work.join("extract").join(format!("v{version}"));
    let exefs = ver_dir.join("ExeFS");
    let img = ver_dir.join("RomFS.img");
    let aoc = stage(p, "romfs", "RomFS", || {
        // title resource patches: only the patched files are new, in a folder beside the extract
        let patched = crate::resource_patch::overlay(a, &b, &|m| p.say(m), &ver_dir, &ver_dir.join("romfs"))?;
        // a NUP's RomFS is a folder already: its files are linked into the app, not copied
        let (files, bytes) = if nup {
            link(p, a.threads, &ver_dir.join("romfs"), patched.as_deref(), &run_dir.join("romfs"), "linking RomFS")?
        } else {
            // the image is unpacked once beside the extract and linked into each app, like a NUP's folder
            let loose = ver_dir.join("romfs");
            unpack_once(p, &img, &loose, "unpacking RomFS")?;
            link(p, a.threads, &loose, patched.as_deref(), &run_dir.join("romfs"), "linking RomFS")?
        };
        let marker = run_dir.join("romfs").join(t.st.romfs_marker.trim_start_matches('/'));
        if !marker.exists() {
            return Err(format!(
                "{}: the RomFS has no {}: it is not the game's files",
                if nup { b.base.display() } else { img.display() },
                t.st.romfs_marker
            ));
        }
        let mut aoc = Vec::new();
        for (tid, v, _) in &b.src.dlc {
            let index = u32::try_from(tid.wrapping_sub(b.src.app).wrapping_sub(0x1000))
                .map_err(|_| format!("DLC {tid:016X}: its index does not fit 32 bits"))?;
            let rel = format!("aoc/{index}");
            if nup {
                link(p, a.threads, &ver_dir.join(&rel), None, &run_dir.join(&rel), "linking DLC")?;
            } else {
                let from = work.join("aoc").join(format!("{index}.img"));
                let loose = work.join("aoc").join(index.to_string());
                unpack_once(p, &from, &loose, "unpacking DLC")?;
                link(p, a.threads, &loose, None, &run_dir.join(&rel), "linking DLC")?;
            }
            aoc.push(manifest::Aoc { index, title_id: *tid, version: *v, path: rel });
        }
        let note = format!("{files} files, {}, {} DLC", mib(bytes), aoc.len());
        Ok((aoc, note))
    })?;
    let built = stage(p, "rewrite", "modules", || {
        let built = modules::rewrite_all(
            &exefs,
            &t.st,
            &format!("{:016X}", t.id),
            t.rules,
            |l| p.say(l),
            |done, total, name| {
                p.set(Update {
                    done: Some(done),
                    total: Some(total),
                    unit: Some("modules"),
                    note: Some(name),
                    ..Default::default()
                })
            },
        )
        .map_err(|e| if e.starts_with("unsupported") { crate::exit::fail(4, e) } else { e })?;
        let names: Vec<&str> = built.iter().map(|m| m.name.as_str()).collect();
        let note = names.join(", ");
        Ok((built, note))
    })?;
    let shapes: Vec<layout::Shape> = built.iter().map(|m| m.shape.clone()).collect();
    let mods: Vec<(modules::Built, u64)> = built.into_iter().zip(layout::bases(&shapes)).collect();
    stage(p, "manifest", "game.json", || {
        if !t.services.contains(&"kernel") {
            return Err(format!("the title module of {:016X} does not list the kernel service, which the host needs", t.id));
        }
        let npdm_bytes = std::fs::read(exefs.join("main.npdm")).map_err(|e| io(&exefs.join("main.npdm"), e))?;
        let main_thread = npdm::main_thread(&npdm_bytes)?;
        let m = manifest::Manifest {
            title_id: t.id,
            name: t.name,
            version,
            version_name: shown_version(&t),
            modules: &mods,
            services: t.services.clone(),
            // an empty list in the title module: the names the modules hold
            nvn_functions: match manifest::nvn_list(t.st.nvn_functions.value) {
                l if l.is_empty() => nvn::names(&mods),
                l => l,
            },
            patches: true,
            main_thread,
            aoc: &aoc,
            settings: t.st.settings.value,
            shared_font_decode: t.shared_font_decode,
            sf_services: t.sf_services.clone(),
            vertex_stream_null_bind: t.vertex_stream_null_bind,
        };
        let put = |n: &str, d: &[u8]| std::fs::write(run_dir.join(n), d).map_err(|e| io(&run_dir.join(n), e));
        put("game.json", manifest::game_json(&m).as_bytes())?;
        put("patches.toml", t.rules.as_bytes())?;
        put("main.npdm", &npdm_bytes)?;
        link::module_files(&run_dir, &mods, |done, total| {
            p.set(Update {
                done: Some(done),
                total: Some(total),
                unit: Some("modules"),
                note: Some("writing module files"),
                ..Default::default()
            })
        })?;
        Ok(((), format!("{} modules, {} NVN functions, {} services", mods.len(), m.nvn_functions.len(), m.services.len())))
    })?;
    stage(p, "link", "nxgame.dylib", || {
        let n = link::write_dylib(&run_dir, &mods, |done, what| {
            p.set(Update {
                done: Some(done),
                total: Some(3),
                unit: Some("steps"),
                note: Some(what),
                ..Default::default()
            })
        })?;
        Ok(((), format!("{}, signed ad hoc", mib(n))))
    })?;
    b.gpus = gpus(a, &work, &b.profile.0)?;
    stage(p, "shaders", "shaders and pipeline archive", || {
        let d = shaders::run(a, &b, p, &run_dir)?;
        Ok(((), d.summary))
    })?;
    stage(p, "bundle", "app", || {
        let _ = std::fs::remove_dir_all(&out);
        std::fs::rename(&stage_dir, &out).map_err(|e| io(&out, e))?;
        Ok(((), out.display().to_string()))
    })?;
    p.say(&format!("app: {}", out.display()));
    Ok(true)
}

fn plist(a: &Args, t: &Game) -> String {
    let id = a.bundle_id.clone().unwrap_or_else(|| bundle::default_bundle_id(t.id));
    let shown = shown_version(t);
    bundle::info_plist(&bundle::file_name(a.app_name.as_deref().unwrap_or(t.name)), &id, &shown)
}

/// The version as the game shows it: the display version of the title module, else the number of the update.
fn shown_version(t: &Game) -> String {
    t.st.settings.value.display_versions.first().map_or_else(|| t.st.version.to_string(), |v| v.1.to_string())
}
