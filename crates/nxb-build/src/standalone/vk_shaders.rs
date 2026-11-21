//! The shader stage of a game on Vulkan: its SPIR-V modules to one Metal library and the table the host's Vulkan layer
//! reads (D21, `nxkit_abi::vk_table`).
//!
//! The modules are the files of the run folder's RomFS that start with the SPIR-V magic. Each one is translated by
//! `nxb-spirv` into the function `vk_<key>` (and `vk_<key>_d` for a dual-source fragment shader) and compiled to AIR
//! with the Mac convention, on `--threads` workers. All AIR is linked into `<run>/shaders/vk_shaders.metallib`, and the
//! rows go into `<run>/shaders/vk_shaders.tsv`. A module that fails to translate or compile is a warning: the build goes
//! on without it, and the summary counts it. There is no pipeline archive.

use super::shaders::Done;
use crate::Args;
use crate::events::{Count, Progress};
use crate::graph::Build;
use nxb_shader::target::MAC;
use nxb_spirv::{Kind, Outcome, Shader, Stage};
use nxb_tool::{Io, MetalTools, ProcessRegistry, Resolver};
use nxkit_abi::vk_table::{self, Resource, Row};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn io(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

/// The SPIR-V files below `romfs`, by their RomFS path (`/` first), in path order. A file whose size cannot be a module
/// is not opened.
pub(crate) fn find(romfs: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(romfs.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for e in std::fs::read_dir(&dir).map_err(|e| io(&dir, e))? {
            let e = e.map_err(|e| io(&dir, e))?;
            let path = e.path();
            let name = format!("{rel}/{}", e.file_name().to_string_lossy());
            let meta = std::fs::metadata(&path).map_err(|x| io(&path, x))?;
            if meta.is_dir() {
                stack.push((path, name));
            } else if meta.len() >= 20 && meta.len().is_multiple_of(4) && starts_spirv(&path)? {
                out.push((name, path));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn starts_spirv(path: &Path) -> Result<bool, String> {
    use std::io::Read;
    let mut head = [0u8; 4];
    let mut f = std::fs::File::open(path).map_err(|e| io(path, e))?;
    f.read_exact(&mut head).map_err(|e| io(path, e))?;
    Ok(nxb_spirv::is_spirv(&head))
}

/// The table row of a translated module. A resource outside the table's limits is an error, as the host would refuse
/// the row.
pub(crate) fn row(key: vk_table::Key, s: &Shader, source: &str) -> Result<Row, String> {
    let resources: Vec<Resource> = s
        .resources
        .iter()
        .map(|r| Resource {
            kind: match r.kind {
                Kind::Uniform => vk_table::Kind::Uniform,
                Kind::Storage => vk_table::Kind::Storage,
                Kind::Texture => vk_table::Kind::Texture,
                Kind::Sampler => vk_table::Kind::Sampler,
                Kind::Combined => vk_table::Kind::Combined,
                Kind::Push => vk_table::Kind::Push,
            },
            set: r.set,
            binding: r.binding,
            slot: r.slot,
            count: r.count,
        })
        .collect();
    if let Some(r) = resources.iter().find(|r| !r.is_valid()) {
        return Err(format!(
            "the resource {r} is outside the limits of {}",
            vk_table::FILE
        ));
    }
    Ok(Row {
        key,
        stage: match s.stage {
            Stage::Vertex => vk_table::Stage::Vert,
            Stage::Fragment => vk_table::Stage::Frag,
        },
        resources,
        source: source.to_string(),
    })
}

/// What became of one module.
enum Made {
    /// Its row and the AIR of its functions.
    Done(Row, Vec<Vec<u8>>),
    /// A stage the host does not take.
    Skipped(&'static str),
    /// Why it is left out.
    Failed(String),
}

/// Translates and compiles one module.
fn one(tools: &MetalTools, key: vk_table::Key, bytes: &[u8], source: &str) -> Made {
    let entry = vk_table::function(&key);
    let s = match nxb_spirv::translate(bytes, &entry, &vk_table::dual_function(&key)) {
        Ok(Outcome::Shader(s)) => s,
        Ok(Outcome::Skipped(stage)) => return Made::Skipped(stage),
        Err(e) => return Made::Failed(format!("translate: {e}")),
    };
    let r = match row(key, &s, source) {
        Ok(r) => r,
        Err(e) => return Made::Failed(e),
    };
    let mut airs = Vec::new();
    for msl in std::iter::once(&s.msl).chain(s.msl_dual.as_ref()) {
        match tools.compile(MAC.compile_flags, None, msl.as_bytes(), Io::Pipes) {
            Ok((air, _)) => airs.push(air),
            Err(e) => return Made::Failed(format!("metal: {e}")),
        }
    }
    Made::Done(r, airs)
}

/// Makes the library and the table of a Vulkan game in `<run>/shaders`.
pub(crate) fn run(a: &Args, b: &Build, p: &Arc<Progress>, run: &Path) -> Result<Done, String> {
    let romfs = run.join("romfs");
    p.say("finding the SPIR-V modules in the RomFS");
    let files = find(&romfs)?;
    // a module that is in the RomFS twice is translated once, under its first path
    let mut modules: BTreeMap<vk_table::Key, (String, Vec<u8>)> = BTreeMap::new();
    for (name, path) in &files {
        let bytes = std::fs::read(path).map_err(|e| io(path, e))?;
        modules
            .entry(nxb_spirv::key_of(&bytes))
            .or_insert((name.clone(), bytes));
    }
    if modules.is_empty() {
        return Err(format!(
            "{}: no SPIR-V module in the RomFS",
            romfs.display()
        ));
    }
    let tmp = b.work.join("shader/vk/tmp");
    std::fs::create_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    let tools = MetalTools::for_target(
        &Resolver::from_env(),
        MAC.target,
        tmp,
        Arc::new(ProcessRegistry::new()),
    )?;
    let list: Vec<(&vk_table::Key, &(String, Vec<u8>))> = modules.iter().collect();
    let results: Vec<Mutex<Option<Made>>> = list.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let count = Count::default();
    count.set_total(list.len() as u64);
    p.counted(Some("translating and compiling"), "shaders", &count, || {
        std::thread::scope(|sc| {
            for _ in 0..a.threads.clamp(1, list.len()) {
                sc.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let (Some(&(key, (name, bytes))), Some(slot)) =
                            (list.get(i), results.get(i))
                        else {
                            break;
                        };
                        let r = one(&tools, *key, bytes, name);
                        if let Ok(mut g) = slot.lock() {
                            *g = Some(r);
                        }
                        count.add(1);
                    }
                });
            }
        });
    });
    let (mut rows, mut airs, mut skipped, mut failed) = (Vec::new(), Vec::new(), 0, 0);
    for (r, (_, (name, _))) in results.into_iter().zip(&list) {
        match r.into_inner().ok().flatten() {
            Some(Made::Done(row, a)) => {
                rows.push(row);
                airs.extend(a);
            }
            Some(Made::Skipped(stage)) => {
                skipped += 1;
                p.say(&format!(
                    "nxb: warning: {name}: a {stage} shader, which the host does not take: left out"
                ));
            }
            Some(Made::Failed(e)) => {
                failed += 1;
                p.say(&format!("nxb: warning: {name}: {e}: left out"));
            }
            None => {
                failed += 1;
                p.say(&format!("nxb: warning: {name}: no result: left out"));
            }
        }
    }
    if rows.is_empty() {
        return Err(format!(
            "none of the {} SPIR-V modules of the RomFS became a Metal function",
            list.len()
        ));
    }
    p.say(&format!(
        "linking {} functions into {}",
        airs.len(),
        vk_table::LIBRARY
    ));
    let (lib, _) = tools
        .link_many(&airs)
        .map_err(|e| format!("metallib: {e}"))?;
    let dst = run.join("shaders");
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).map_err(|e| io(&dst, e))?;
    let put = |n: &str, d: &[u8]| std::fs::write(dst.join(n), d).map_err(|e| io(&dst.join(n), e));
    put(vk_table::LIBRARY, &lib)?;
    put(
        vk_table::FILE,
        vk_table::write(&rows, &nxb_spirv::translator_id()).as_bytes(),
    )?;
    Ok(Done {
        summary: format!(
            "{} Vulkan shaders, {} functions, {} failed, {} skipped, {:.1} MB library, no archive (Vulkan)",
            rows.len(),
            airs.len(),
            failed,
            skipped,
            lib.len() as f64 / 1_048_576.0
        ),
    })
}

#[cfg(test)]
#[path = "vk_shaders_tests.rs"]
mod tests;
