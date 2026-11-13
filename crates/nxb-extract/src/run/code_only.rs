//! A code-only package update over an already unpacked NUP. Loose RomFS files cannot supply BKTR base offsets.

use std::collections::BTreeSet;

use super::{Arc, AtomicU64, BlockCache, ExtractError, FileStorage, KeySet, Nca, Ordering};
use super::{Path, PathBuf, Request, Result, Storage, open_package, small_outputs};
use crate::meta::{PATCH, metas};
use crate::nca::ContentType;

static STAGING_ID: AtomicU64 = AtomicU64::new(0);

struct Staging(PathBuf);

impl Staging {
    fn new(out: &Path) -> Result<Self> {
        std::fs::create_dir_all(out)
            .map_err(|e| ExtractError::io(format!("create {}", out.display()), e))?;
        for _ in 0..100 {
            let id = STAGING_ID.fetch_add(1, Ordering::Relaxed);
            let path = out.join(format!(".code-only-{}-{id}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(ExtractError::io(format!("create {}", path.display()), e));
                }
            }
        }
        Err(ExtractError::io(
            format!("create code-only staging directory in {}", out.display()),
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "100 staging directory names already exist",
            ),
        ))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn selected<'a>(
    ks: &KeySet,
    ncas: &'a [Nca],
    cache: &Arc<BlockCache>,
    app: u64,
    version: u32,
) -> Result<(&'a Nca, Option<&'a Nca>)> {
    let matching: Vec<_> = metas(ks, ncas, cache)?
        .into_iter()
        .filter(|m| m.kind == PATCH && m.title == (app | 0x800))
        .collect();
    if matching.len() != 1 || matching[0].version != version {
        return Err(ExtractError::format(format!(
            "code-only update needs exactly one Patch content meta of title {:016X} and version {version}",
            app | 0x800
        )));
    }
    let mut ids = BTreeSet::new();
    let mut listed = Vec::new();
    for id in &matching[0].contents {
        if !ids.insert(id) {
            return Err(ExtractError::format(format!(
                "Patch content meta lists NCA {id} more than once"
            )));
        }
        let members: Vec<_> = ncas.iter().filter(|n| n.id == *id).collect();
        if members.len() != 1 {
            return Err(ExtractError::format(format!(
                "Patch content meta needs exactly one NCA {id}; missing, renamed or duplicate content"
            )));
        }
        listed.push(members[0]);
    }
    let programs: Vec<_> = listed
        .iter()
        .copied()
        .filter(|n| n.content_type == ContentType::Program)
        .collect();
    if programs
        .iter()
        .any(|n| n.sections[n.data_section()].is_some())
    {
        return Err(ExtractError::Unsupported(
            "an encrypted update with a Program data section requires the original base NCA package; loose NUP RomFS files cannot supply BKTR base offsets".into(),
        ));
    }
    if programs.len() != 1 || programs[0].title_id != app {
        return Err(ExtractError::format(format!(
            "code-only update needs exactly one listed Program NCA of application {app:016X}"
        )));
    }
    let controls: Vec<_> = listed
        .into_iter()
        .filter(|n| n.content_type == ContentType::Control)
        .collect();
    if controls.len() > 1 || controls.first().is_some_and(|n| n.title_id != app) {
        return Err(ExtractError::format(format!(
            "code-only update needs at most one listed Control NCA of application {app:016X}"
        )));
    }
    Ok((programs[0], controls.first().copied()))
}

fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ExtractError::io(format!("remove {}", path.display()), e)),
    }
}

fn install(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to)
        .map_err(|e| ExtractError::io(format!("rename {} to {}", from.display(), to.display()), e))
}

/// Replaces an unpacked NUP's code with one exact, code-only encrypted update.
///
/// Uses `req.keys`, `req.base` (the update package), `req.app` (required), `req.out_base` and `req.cache_chunks`.
/// The output must be a caller-owned work folder, never the source NUP. Its RomFS and other files are preserved.
/// A Program data section requires the original base NCA and returns [`ExtractError::Unsupported`].
/// Package membership, decrypted code, control data and the NPDM ID are checked before replacing any base files.
/// The whole ExeFS is replaced. Control and icon are preserved only when the update has no Control NCA.
pub fn extract_code_only_update(req: &Request, version: u32) -> Result<()> {
    let app = req
        .app
        .filter(|app| *app != 0 && app & 0xfff == 0)
        .ok_or_else(|| ExtractError::format("code-only update needs an explicit application ID"))?;
    if req.program.is_some_and(|program| program != app) {
        return Err(ExtractError::Unsupported(
            "code-only updates of another application program".into(),
        ));
    }
    let text = std::fs::read_to_string(&req.keys)
        .map_err(|e| ExtractError::io(format!("read {}", req.keys.display()), e))?;
    let mut ks = KeySet::parse(&text);
    drop(text);
    if let Some(spill) = std::env::var_os("NXB_TMP").map(PathBuf::from).or_else(|| {
        req.out_base
            .ancestors()
            .find(|p| p.is_dir())
            .map(Path::to_path_buf)
    }) {
        crate::ncz::set_spill_dir(&spill);
    }
    let pkg: Arc<dyn Storage> = Arc::new(FileStorage::open(&req.base)?);
    let ncas = open_package(&mut ks, pkg, &req.base.display().to_string())?;
    let cache = Arc::new(BlockCache::new(req.cache_chunks));
    let (prog, ctrl) = selected(&ks, &ncas, &cache, app, version)?;
    let staged = Staging::new(&req.out_base)?;
    small_outputs(&ks, prog, prog, ctrl, &staged.0, &cache, &mut String::new())?;
    let npdm_path = staged.0.join("ExeFS/main.npdm");
    let npdm = std::fs::read(&npdm_path)
        .map_err(|e| ExtractError::io(format!("read {}", npdm_path.display()), e))?;
    if crate::nup::program_id(&npdm, "update ExeFS/main.npdm")? != app {
        return Err(ExtractError::format(format!(
            "update ExeFS/main.npdm does not match application {app:016X}"
        )));
    }
    if !staged.0.join("ExeFS/main").is_file() {
        return Err(ExtractError::format("code-only update has no ExeFS/main"));
    }
    let exefs = req.out_base.join("ExeFS");
    match std::fs::remove_dir_all(&exefs) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ExtractError::io(format!("remove {}", exefs.display()), e)),
    }
    install(&staged.0.join("ExeFS"), &exefs)?;
    if ctrl.is_some() {
        install(
            &staged.0.join("control.nacp"),
            &req.out_base.join("control.nacp"),
        )?;
        let icon = req.out_base.join("icon.jpg");
        if staged.0.join("icon.jpg").is_file() {
            install(&staged.0.join("icon.jpg"), &icon)?;
        } else {
            remove_file(&icon)?;
        }
    }
    Ok(())
}
