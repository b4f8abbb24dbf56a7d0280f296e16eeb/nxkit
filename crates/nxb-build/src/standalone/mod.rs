//! `nxb build` for a title that has a title module: the run folder and the app without nxnative (H6, R21, R48 to R50).
//!
//! The stages, in order: unpack the packages (base, update, DLC), put the RomFS into the run folder, rewrite the
//! modules, write `game.json`, link and sign `nxgame.dylib`, make the shaders and the pipeline archive, and close the
//! bundle. A title without a module still builds through nxnative, and this path leaves it alone ([`run`] answers
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

/// The extract and the split, reused while the packages are as they were.
fn extract(a: &Args, b: &Build, p: &Say, version: u32) -> Result<String, String> {
    let dir = b.work.join("extract").join(format!("v{version}"));
    let key_file = b.work.join("extract.key");
    let key = std::iter::once(identity(&b.base))
        .chain(b.updates.iter().map(|u| identity(&u.1)))
        .collect::<Vec<_>>()
        .join(";");
    let have = dir.join("RomFS.img").is_file()
        && dir.join("ExeFS/main.npdm").is_file()
        && std::fs::read_dir(b.work.join("shader/pkg")).is_ok_and(|mut d| d.next().is_some());
    let reused = have && std::fs::read_to_string(&key_file).is_ok_and(|k| k == key);
    let mut text = if reused {
        "reused".to_string()
    } else {
        let _ = std::fs::remove_file(&key_file);
        let (images, unsupported) = crate::extract_stage::packages(a, b, p)?;
        if let Some(why) = unsupported {
            return Err(crate::exit::fail(
                4,
                format!("unsupported: the shader split needs the game's own decoder: {why}"),
            ));
        }
        std::fs::write(&key_file, &key).map_err(|e| io(&key_file, e))?;
        format!("{images} images")
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
    romfs::unpack(img, dest, |done, total| {
        p.set(Update {
            step: Some(what),
            done: Some(done),
            total: Some(total),
            unit: Some("B"),
            ..Default::default()
        });
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
    if src.source != Source::Packages {
        return Err(crate::exit::fail(
            4,
            "unsupported: a build without nxnative reads packages (NSP or XCI)",
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
    stage(p, "extract", "packages and shader split", || {
        Ok(((), extract(a, &b, p, version)?))
    })?;
    let exefs = work.join("extract").join(format!("v{version}/ExeFS"));
    let img = work.join("extract").join(format!("v{version}/RomFS.img"));
    let aoc = stage(p, "romfs", "RomFS", || {
        let (files, bytes) = unpack(p, &img, &run_dir.join("romfs"), "unpacking RomFS")?;
        let marker = run_dir.join("romfs").join(t.st.romfs_marker.trim_start_matches('/'));
        if !marker.exists() {
            return Err(format!(
                "{}: the RomFS has no {}: it is not the game's files",
                img.display(),
                t.st.romfs_marker
            ));
        }
        let mut aoc = Vec::new();
        for (tid, v, _) in &b.src.dlc {
            let index = u32::try_from(tid.wrapping_sub(b.src.app).wrapping_sub(0x1000))
                .map_err(|_| format!("DLC {tid:016X}: its index does not fit 32 bits"))?;
            let from = work.join("aoc").join(format!("{index}.img"));
            let rel = format!("aoc/{index}");
            unpack(p, &from, &run_dir.join(&rel), "unpacking DLC")?;
            aoc.push(manifest::Aoc { index, title_id: *tid, version: *v, path: rel });
        }
        let note = format!("{files} files, {}, {} DLC", mib(bytes), aoc.len());
        Ok((aoc, note))
    })?;
    let built = stage(p, "rewrite", "modules", || {
        let built = modules::rewrite_all(&exefs, &t.st, &format!("{:016X}", t.id), t.rules, |l| p.say(l)).map_err(
            |e| if e.starts_with("unsupported") { crate::exit::fail(4, e) } else { e },
        )?;
        let names: Vec<&str> = built.iter().map(|m| m.name.as_str()).collect();
        let note = names.join(", ");
        Ok((built, note))
    })?;
    let shapes: Vec<layout::Shape> = built.iter().map(|m| m.shape.clone()).collect();
    let mods: Vec<(modules::Built, u64)> = built.into_iter().zip(layout::bases(&shapes)).collect();
    stage(p, "manifest", "game.json", || {
        let npdm_bytes = std::fs::read(exefs.join("main.npdm")).map_err(|e| io(&exefs.join("main.npdm"), e))?;
        let main_thread = npdm::main_thread(&npdm_bytes)?;
        let m = manifest::Manifest {
            title_id: t.id,
            name: t.name,
            version,
            modules: &mods,
            services: t.services.clone(),
            nvn_functions: manifest::nvn_list(t.st.nvn_functions.value),
            patches: true,
            main_thread,
            aoc: &aoc,
            settings: t.st.settings.value,
        };
        let put = |n: &str, d: &[u8]| std::fs::write(run_dir.join(n), d).map_err(|e| io(&run_dir.join(n), e));
        put("game.json", manifest::game_json(&m).as_bytes())?;
        put("patches.toml", t.rules.as_bytes())?;
        put("main.npdm", &npdm_bytes)?;
        link::module_files(&run_dir, &mods)?;
        Ok(((), format!("{} modules, {} NVN functions, {} services", mods.len(), m.nvn_functions.len(), m.services.len())))
    })?;
    stage(p, "link", "nxgame.dylib", || {
        let n = link::write_dylib(&run_dir, &mods)?;
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
    let shown = t
        .st
        .settings
        .value
        .display_versions
        .first()
        .map_or_else(|| t.st.version.to_string(), |v| v.1.to_string());
    bundle::info_plist(&bundle::file_name(a.app_name.as_deref().unwrap_or(t.name)), &id, &shown)
}
