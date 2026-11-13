//! Generic input adapter over the shared package, NCZ, NUP and DLC extractors.
use super::{io, manifest, romfs};
use crate::{Args, events::Progress, graph::Build, inputs::Source};
use nxb_extract::version;
use std::path::{Path, PathBuf};

const LIMIT: u64 = 256 << 20;
fn err(e: nxb_extract::ExtractError) -> String {
    crate::exit::fail(e.exit_code(), e.to_string())
}

pub(super) struct Extracted {
    pub title_id: u64,
    pub name: String,
    pub version: u32,
    pub version_name: String,
    pub npdm: Vec<u8>,
    pub modules: Vec<(String, Vec<u8>)>,
    pub files: Vec<(String, Vec<u8>)>,
    pub directories: Vec<String>,
    pub aoc: Vec<manifest::Aoc>,
}

fn read(path: &Path, total: &mut u64) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path).map_err(|e| io(path, e))?.len();
    *total = total
        .checked_add(size)
        .filter(|n| *n <= LIMIT)
        .ok_or("unsupported: extracted game exceeds the generic adapter 256 MiB limit")?;
    std::fs::read(path).map_err(|e| io(path, e))
}

fn tree(
    root: &Path,
    prefix: &str,
    overlay: Option<&Path>,
    game: &mut Extracted,
    total: &mut u64,
) -> Result<(), String> {
    if !root.is_dir() {
        return Ok(());
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(rel) = pending.pop() {
        game.directories.push(if rel.as_os_str().is_empty() {
            prefix.into()
        } else {
            format!("{prefix}/{}", rel.display())
        });
        let dir = root.join(&rel);
        let mut entries = std::fs::read_dir(&dir)
            .map_err(|e| io(&dir, e))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io(&dir, e))?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e
                .file_name()
                .into_string()
                .map_err(|_| "extracted filenames must be UTF-8")?;
            nxb_extract::safe_path::rel("extracted file", &name, true).map_err(err)?;
            let rel = rel.join(name);
            let kind = e.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push(rel);
            } else if kind.is_file() {
                let path = overlay
                    .map(|o| o.join(&rel))
                    .filter(|p| p.is_file())
                    .unwrap_or_else(|| e.path());
                game.files
                    .push((format!("{prefix}/{}", rel.display()), read(&path, total)?));
            } else {
                return Err("extracted files must not contain symlinks or special files".into());
            }
        }
    }
    Ok(())
}

pub(super) fn extract(a: &Args, p: &Progress) -> Result<Extracted, String> {
    let src = crate::inputs::discover_generic(a)?;
    let version = src.updates.last().map_or(src.base_version, |u| u.0);
    let scratch =
        crate::host::TempDir::new_in(a.work_dir.as_deref().unwrap_or(&std::env::temp_dir()))?;
    let work = scratch.path();
    let package = format!("v{version}");
    let ex = work.join("extract").join(&package);
    let loose = ex.join("romfs");
    let b = Build {
        title: format!("{:016X}", src.app),
        base: src.base.clone(),
        updates: src.updates.last().cloned().into_iter().collect(),
        package,
        work: work.to_path_buf(),
        profile: (PathBuf::new(), Default::default()),
        shaders: PathBuf::new(),
        gpus: vec![],
        pipeline_lists: vec![],
        src,
    };
    for note in &b.src.notes {
        p.say(note);
    }
    if b.src.source == Source::Nup {
        let external_update = b.updates.last().filter(|(_, path)| path != &b.base);
        if external_update.is_some() && !b.src.has_base {
            return Err("unsupported: a code-only package update needs the original base game in the NUP or --base-package".into());
        }
        let result = nxb_extract::nup::unpack_nup(&nxb_extract::nup::UnpackRequest {
            nup: b.base.clone(),
            version: external_update.map(|_| 0),
            out: ex.clone(),
            romfs: loose.clone(),
            aoc: ex.join("aoc"),
            threads: a.threads,
            mem_limit: LIMIT,
            verify: true,
            split: None,
            progress: None,
        })
        .map_err(err)?;
        for note in result.notes {
            p.say(&note);
        }
        if let Some((version, path)) = external_update {
            nxb_extract::extract_code_only_update(
                &nxb_extract::Request {
                    keys: crate::extract_stage::keys(a)?,
                    base: path.clone(),
                    out_base: ex.clone(),
                    app: Some(b.src.app),
                    threads: a.threads,
                    mem_limit: LIMIT,
                    ..Default::default()
                },
                *version,
            )
            .map_err(err)?;
        }
    } else {
        let keys = crate::extract_stage::keys(a)?;
        let result = nxb_extract::extract(&nxb_extract::Request {
            keys,
            base: b.base.clone(),
            out_base: if b.updates.is_empty() {
                ex.clone()
            } else {
                work.join("extract/v0")
            },
            updates: b
                .updates
                .iter()
                .map(|(_, path)| (path.clone(), ex.clone()))
                .collect(),
            base_outputs: b.updates.is_empty(),
            app: Some(b.src.app),
            threads: a.threads,
            mem_limit: LIMIT,
            ..Default::default()
        })
        .map_err(|e| crate::extract_stage::damaged(p, &e))?;
        result.closing.wait();
        romfs::unpack(&ex.join("RomFS.img"), &loose, |_, _| {})?;
    }
    crate::extract_stage::dlc(a, &b, p)?;
    for (id, version, path) in &b.src.dlc {
        if b.src.source == Source::Nup && path == &b.base {
            continue;
        }
        let index = id - b.src.app - 0x1000;
        let out = ex.join(format!("aoc/{index}"));
        if nxb_extract::nup::is_nup(path) {
            nxb_extract::nup::unpack_dlc(path, *id, *version, &out, LIMIT).map_err(err)?;
        } else {
            romfs::unpack(&work.join(format!("aoc/{index}.img")), &out, |_, _| {})?;
        }
    }
    let overlay = crate::resource_patch::overlay_explicit(a, &b, &|m| p.say(m), &ex, &loose)?;
    let mut total = 0;
    let npdm = read(&ex.join("ExeFS/main.npdm"), &mut total)?;
    if nxb_extract::nup::program_id(&npdm, "main.npdm").map_err(err)? != b.src.app {
        return Err("NPDM program ID does not match the application metadata".into());
    }
    let mut game = Extracted {
        title_id: b.src.app,
        name: format!("{:016X}", b.src.app),
        version,
        version_name: String::new(),
        npdm,
        modules: vec![],
        files: vec![],
        directories: vec![],
        aoc: vec![],
    };
    let exefs = ex.join("ExeFS");
    let mut modules = std::fs::read_dir(&exefs)
        .map_err(|e| io(&exefs, e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| io(&exefs, e))?;
    modules.sort_by_key(|e| e.file_name());
    for e in modules {
        if e.file_name() == "main.npdm" {
            continue;
        }
        if !e.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("ExeFS contains a non-file".into());
        }
        let data = read(&e.path(), &mut total)?;
        if data.starts_with(b"NSO0") {
            game.modules.push((
                e.file_name()
                    .into_string()
                    .map_err(|_| "module name is not UTF-8")?,
                data,
            ));
        }
    }
    for name in ["control.nacp", "icon.jpg"] {
        let path = ex.join(name);
        if path.is_file() {
            let bytes = read(&path, &mut total)?;
            if name == "control.nacp" {
                let name = version::title_name(&bytes).map_err(err)?;
                if !name.is_empty() {
                    game.name = name;
                }
                game.version_name = version::display_version(&bytes);
            }
            game.files.push((name.into(), bytes));
        }
    }
    tree(&loose, "romfs", overlay.as_deref(), &mut game, &mut total)?;
    for (id, version, _) in &b.src.dlc {
        let index = u32::try_from(id - b.src.app - 0x1000).map_err(|e| e.to_string())?;
        let path = format!("aoc/{index}");
        tree(&ex.join(&path), &path, None, &mut game, &mut total)?;
        game.aoc.push(manifest::Aoc {
            index,
            title_id: *id,
            version: *version,
            path,
        });
    }
    Ok(game)
}

#[cfg(test)]
#[path = "extract_tests.rs"]
mod tests;
