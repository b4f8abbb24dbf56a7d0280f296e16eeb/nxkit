//! `nxb build`: the app of a title that has a title module, with `nxkit-host` as its program (H6, R21, R48 to R50).
//!
//! The stages, in order: unpack the packages (base, update, DLC) or the NUP container, put the RomFS into the run
//! folder, rewrite the modules, write `game.json`, link and sign `nxgame.dylib`, make the shaders and the pipeline
//! archive, and close the bundle. A NUP gives the RomFS as a folder in the extract's one pass (with the shader split),
//! and the romfs stage links its files into the app: no `RomFS.img`, no second copy. `--auto-analyze` uses the generic path.

mod analysis;
mod auto_shaders;
mod bundle;
mod elf;
mod extract;
mod files;
pub(crate) mod fonts;
mod generic;
mod icon;
mod image;
mod launcher;
mod layout;
mod link;
mod macho;
mod manifest;
mod modules;
mod npdm;
mod nvn;
mod nxnative;
#[cfg(test)]
mod nxnative_tests;
mod pipelist;
#[cfg(test)]
mod replace_tests;
mod romfs;
mod shaders;
mod version;
mod vk_shaders;

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;
use crate::inputs::{Inputs, Source};
use nxtitle::Standalone;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use files::{extract, link, unpack_once};

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
    std::fs::metadata(p)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            format!(
                "{}:{}:{}",
                p.display(),
                m.len(),
                t.map_or(0, |d| d.as_secs())
            )
        })
        .unwrap_or_default()
}

/// What the build knows about the title before it starts.
struct Game {
    id: u64,
    name: &'static str,
    st: Standalone,
    rules: &'static str,
    services: Vec<&'static str>,
    /// The graphics API the game renders with (`BuildHooks::graphics_api`).
    graphics: nxtitle::GraphicsApi,
    shared_font_decode: bool,
    sf_services: Vec<&'static str>,
    /// The frame limits of the family and the title (`RuntimeHooks::limits`).
    limits: Vec<nxtitle::LimitRule>,
    /// The program arguments that set the render size (`RuntimeHooks::render_args`).
    render_args: Option<nxtitle::RenderArgs>,
    /// The temporal upscale facts, when the engine and the title both answer (`temporal_view`, `temporal_draws`).
    temporal_upscale: Option<(nxtitle::TemporalView, nxtitle::TemporalDraws)>,
    /// The title needs the command buffer to ignore a bind of one null range at a stream of 16 or more.
    vertex_stream_null_bind: bool,
}

/// The title module of the input's game, `None` when the game has no module or has no data for this build.
fn module(src: &Inputs) -> Result<Option<Game>, String> {
    let id = src.app;
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
        services: r
            .nxkit_services()
            .into_iter()
            .map(nxtitle::Service::id)
            .collect(),
        graphics: r.graphics_api(),
        shared_font_decode: r.shared_font_decode().is_some_and(|f| f.value),
        sf_services: r.sf_services().into_iter().map(|f| f.value).collect(),
        limits: r.limits().into_iter().map(|f| f.value).collect(),
        render_args: r.render_args().map(|f| f.value),
        temporal_upscale: r
            .temporal_view()
            .zip(r.temporal_draws())
            .map(|(v, d)| (v.value, d.value)),
        vertex_stream_null_bind: r.needs().iter().any(|n| {
            n.value.capability == "vertex_stream_null_bind" && n.value.setting == "ignore"
        }),
    }))
}

/// The folder the work folder and the output default into: the input folder, or the one holding the input file.
fn home(a: &Args) -> PathBuf {
    if a.nsp_dir.is_file() {
        a.nsp_dir
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
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

/// Refuses to replace something that is not an app a build made: an app of nxkit, or one of the old engine
/// (nxnative), which a launcher such as NX2Apple built at the same place before the game moved to nxkit. The old
/// engine keeps the save outside the app (`~/Library/Application Support/nxnative/<title id>/save` or the
/// library folder's `save/<title id>`), so replacing its app loses no save: the stage `bundle` copies it into the new
/// app (`nxnative.rs`).
fn replaceable(out: &Path) -> Result<(), String> {
    let nxkit = out.join("Contents/MacOS/nxkit-host").is_file();
    if !out.exists() || nxkit || nxnative::is_app(out) {
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

fn mib(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.0)
}

/// What a dry run prints.
fn describe(t: &Game, b: &Build, host: &Path, out: &Path) -> String {
    let mut s = format!("nxb build: {} [{:016X}] v{}\n", t.name, t.id, t.st.version);
    s.push_str(&format!("  base:      {}\n", b.base.display()));
    for (v, path) in &b.updates {
        s.push_str(&format!("  update:    v{v} {}\n", path.display()));
    }
    for (tid, v, path) in &b.src.dlc {
        s.push_str(&format!(
            "  dlc:       {tid:016X} v{v} {}\n",
            path.display()
        ));
    }
    s.push_str(&format!("  work:      {}\n", b.work.display()));
    s.push_str(&format!("  host:      {}\n", host.display()));
    s.push_str(&format!("  app:       {}\n", out.display()));
    for l in &b.pipeline_lists {
        s.push_str(&format!("  pipelines: {}\n", l.display()));
    }
    s.push_str("  stages:    extract, romfs, rewrite, manifest, link, shaders, bundle\n");
    s
}

/// Builds the app of a title that has a title module.
pub(crate) fn run(a: &Args, p: &Say) -> Result<(), String> {
    if a.auto_analyze {
        return generic::run(a, p);
    }
    let src = crate::inputs::discover(a)?;
    let Some(t) = module(&src)? else {
        return Err(crate::exit::fail(
            4,
            format!(
                "unsupported: the game {:016X} has no title module. nxb builds only games that have one, for nxkit",
                src.app
            ),
        ));
    };
    let version = t.st.version;
    let updates = version::updates_for(t.name, version, src.has_base, &src.updates)?;
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
        base: src.base.clone(),
        updates,
        package: package.clone(),
        work: work.clone(),
        profile: (PathBuf::new(), Default::default()),
        shaders: work
            .join("shader/cache")
            .join(format!("b-{}", nxb_shader::chain::standalone_version())),
        gpus: Vec::new(),
        split: t.graphics == nxtitle::GraphicsApi::Nvn,
        pipeline_lists: pipelist::find(
            a.pipeline_list.as_deref(),
            &out,
            &format!("{:016X}", t.id),
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            std::env::var_os("NXKIT_PIPELINE_CACHE_DIR")
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .as_deref(),
        ),
        src,
    };
    if a.dry_run {
        print!("{}", describe(&t, &b, &host, &out));
        return Ok(());
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
    let what = match (nup, b.split) {
        (true, true) => "NUP and shader split",
        (true, false) => "NUP",
        (false, true) => "packages and shader split",
        (false, false) => "packages",
    };
    stage(p, "extract", what, || {
        Ok(((), extract(a, &b, p, version, t.st.romfs_marker)?))
    })?;
    let ver_dir = work.join("extract").join(format!("v{version}"));
    let exefs = ver_dir.join("ExeFS");
    let img = ver_dir.join("RomFS.img");
    let aoc = stage(p, "romfs", "RomFS", || {
        // title resource patches: only the patched files are new, in a folder beside the extract
        let patched =
            crate::resource_patch::overlay(a, &b, &|m| p.say(m), &ver_dir, &ver_dir.join("romfs"))?;
        // a NUP's RomFS is a folder already: its files are linked into the app, not copied
        let (files, bytes) = if nup {
            link(
                p,
                a.threads,
                &ver_dir.join("romfs"),
                patched.as_deref(),
                &run_dir.join("romfs"),
                "linking RomFS",
            )?
        } else {
            // the image is unpacked once beside the extract and linked into each app, like a NUP's folder
            let loose = ver_dir.join("romfs");
            unpack_once(p, &img, &loose, "unpacking RomFS")?;
            link(
                p,
                a.threads,
                &loose,
                patched.as_deref(),
                &run_dir.join("romfs"),
                "linking RomFS",
            )?
        };
        let marker = run_dir
            .join("romfs")
            .join(t.st.romfs_marker.trim_start_matches('/'));
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
                link(
                    p,
                    a.threads,
                    &ver_dir.join(&rel),
                    None,
                    &run_dir.join(&rel),
                    "linking DLC",
                )?;
            } else {
                let from = work.join("aoc").join(format!("{index}.img"));
                let loose = work.join("aoc").join(index.to_string());
                unpack_once(p, &from, &loose, "unpacking DLC")?;
                link(
                    p,
                    a.threads,
                    &loose,
                    None,
                    &run_dir.join(&rel),
                    "linking DLC",
                )?;
            }
            aoc.push(manifest::Aoc {
                index,
                title_id: *tid,
                version: *v,
                path: rel,
            });
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
        .map_err(|e| {
            if e.starts_with("unsupported") {
                crate::exit::fail(4, e)
            } else {
                e
            }
        })?;
        let names: Vec<&str> = built.iter().map(|m| m.name.as_str()).collect();
        let note = names.join(", ");
        Ok((built, note))
    })?;
    let shapes: Vec<layout::Shape> = built.iter().map(|m| m.shape.clone()).collect();
    let mods: Vec<(modules::Built, u64)> = built.into_iter().zip(layout::bases(&shapes)).collect();
    stage(p, "manifest", "game.json", || {
        if !t.services.contains(&"kernel") {
            return Err(format!(
                "the title module of {:016X} does not list the kernel service, which the host needs",
                t.id
            ));
        }
        let npdm_bytes =
            std::fs::read(exefs.join("main.npdm")).map_err(|e| io(&exefs.join("main.npdm"), e))?;
        let main_thread = npdm::main_thread(&npdm_bytes)?;
        let m = manifest::Manifest {
            title_id: t.id,
            name: t.name,
            version,
            version_name: shown_version(&t),
            modules: &mods,
            services: t.services.clone(),
            graphics: t.graphics.id(),
            // an empty list in the title module: the names the modules hold. A game on another API has none.
            nvn_functions: match manifest::nvn_list(t.st.nvn_functions.value) {
                _ if t.graphics != nxtitle::GraphicsApi::Nvn => Vec::new(),
                l if l.is_empty() => nvn::names(&mods),
                l => l,
            },
            patches: true,
            main_thread,
            aoc: &aoc,
            settings: t.st.settings.value,
            shared_font_decode: t.shared_font_decode,
            sf_services: t.sf_services.clone(),
            limits: t.limits.clone(),
            render_args: t.render_args,
            temporal_upscale: t.temporal_upscale,
            vertex_stream_null_bind: t.vertex_stream_null_bind,
        };
        let put = |n: &str, d: &[u8]| {
            std::fs::write(run_dir.join(n), d).map_err(|e| io(&run_dir.join(n), e))
        };
        put("game.json", manifest::game_json(&m).as_bytes())?;
        let nacp = std::fs::read(ver_dir.join("control.nacp")).ok();
        let options =
            launcher::launcher_json(t.id, &m.settings, nacp.as_deref(), t.render_args.is_some());
        put(nxkit_abi::launch::FILE, options.as_bytes())?;
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
        Ok((
            (),
            format!(
                "{} modules, {} NVN functions, {} services",
                mods.len(),
                m.nvn_functions.len(),
                m.services.len()
            ),
        ))
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
    // a game on Vulkan has no NVN shaders and no pipeline archive
    let vulkan = t.graphics == nxtitle::GraphicsApi::Vulkan;
    if !vulkan {
        b.gpus = gpus(a, &work, &b.profile.0)?;
    }
    if !b.gpus.is_empty() {
        for l in &b.pipeline_lists {
            p.say(&format!("pipeline list: {}", l.display()));
        }
    }
    stage(p, "shaders", "shaders and pipeline archive", || {
        let d = if vulkan {
            vk_shaders::run(a, &b, p, &run_dir)?
        } else {
            shaders::run(a, &b, p, &run_dir)?
        };
        Ok(((), d.summary))
    })?;
    stage(p, "bundle", "app", || {
        // The host keeps the save under Application Support and moves a save that an older build left in the run
        // folder there at its first start (R67). Such a save moves into the new app before the old one goes, and a
        // failed move stops the build with the old app and its save in place.
        let old_save = out.join("Contents/Resources/nxkit/save");
        if old_save.is_dir() {
            let new_save = run_dir.join("save");
            if new_save.exists() {
                return Err(format!(
                    "{}: the new app already has a save folder",
                    new_save.display()
                ));
            }
            std::fs::rename(&old_save, &new_save).map_err(|e| io(&new_save, e))?;
            p.say(&format!(
                "save: kept from the old app ({})",
                new_save.display()
            ));
        } else if nxnative::is_app(&out) && !run_dir.join("save").exists() {
            // the old engine's save is outside its app: a copy goes into the new app, the old one stays. The host
            // moves the copy to its own save folder at the first start, unless a save is there already (R67).
            if let Some(home) = std::env::var_os("HOME")
                && let Some(m) = nxnative::carry(Path::new(&home), &out, t.id, &run_dir)?
            {
                p.say(&m);
            }
        }
        // the lists of the recorded pipelines go with the save: the new app starts with them
        let carried = pipelist::carry(
            &b.pipeline_lists,
            &out.join("Contents/Resources/nxkit"),
            &run_dir,
        );
        if carried > 0 {
            p.say(&format!("pipeline lists: {carried} kept in the new app"));
        }
        match fonts::write(&run_dir.join("font")) {
            Ok(s) => p.say(&s),
            Err(e) => p.say(&format!(
                "warning: no shared fonts, button icons draw as \"?\": {e}"
            )),
        }
        // the cover of the game; an update's control data may have none, then the base's
        let cover = [ver_dir.join("icon.jpg"), work.join("extract/v0/icon.jpg")]
            .into_iter()
            .find(|c| c.is_file());
        match cover {
            Some(c) => match icon::write(
                &c,
                &stage_dir.join("Contents/Resources"),
                &work.join("icon"),
            ) {
                Ok(s) => p.say(&s),
                Err(e) => p.say(&format!("warning: no app icon: {e}")),
            },
            None => p.say("warning: no app icon, the control data has no icon.jpg"),
        }
        let _ = std::fs::remove_dir_all(&out);
        std::fs::rename(&stage_dir, &out).map_err(|e| io(&out, e))?;
        Ok(((), out.display().to_string()))
    })?;
    p.say(&format!("app: {}", out.display()));
    Ok(())
}

fn plist(a: &Args, t: &Game) -> String {
    let id = a
        .bundle_id
        .clone()
        .unwrap_or_else(|| bundle::default_bundle_id(t.id));
    let shown = shown_version(t);
    bundle::info_plist(
        &bundle::file_name(a.app_name.as_deref().unwrap_or(t.name)),
        &id,
        &shown,
    )
}

/// The version as the game shows it: the display version of the title module, else the number of the update.
fn shown_version(t: &Game) -> String {
    t.st.settings
        .value
        .display_versions
        .first()
        .map_or_else(|| t.st.version.to_string(), |v| v.1.to_string())
}
