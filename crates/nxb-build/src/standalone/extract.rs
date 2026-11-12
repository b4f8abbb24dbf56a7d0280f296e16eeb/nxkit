//! Single-application NSP/XCI extraction. Section hashes are checked by nxkit; signatures
//! and CNMT content hashes are not authenticated by this adapter.
use nxb_extract::{
    cache::BlockCache,
    keys::KeySet,
    meta,
    nca::{ContentType, Nca, read_hfs0, read_pfs0, xci_secure},
    storage::{FileStorage, Storage, SubStorage},
    version,
};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
/// One consistency-checked application, with at most one bundled update.
pub(super) struct Extracted {
    pub title_id: u64,
    pub name: String,
    pub version: u32,
    pub version_name: String,
    pub npdm: Vec<u8>,
    pub modules: Vec<(String, Vec<u8>)>,
    pub files: Vec<(String, Vec<u8>)>,
    pub directories: Vec<String>,
}

// Validate the complete ACI0 range before reading the program identity.
fn program_id(npdm: &[u8]) -> Result<u64, String> {
    if npdm.len() < 0x80 || npdm.get(..4) != Some(b"META") {
        return Err("main.npdm: missing META header".into());
    }
    let offset = u32::from_le_bytes(
        npdm[0x70..0x74]
            .try_into()
            .map_err(|_| "invalid integer or key field length")?,
    ) as usize;
    let size = u32::from_le_bytes(
        npdm[0x74..0x78]
            .try_into()
            .map_err(|_| "invalid integer or key field length")?,
    ) as usize;
    let end = offset.checked_add(size).ok_or("main.npdm: ACI0 overflow")?;
    let aci = npdm
        .get(offset..end)
        .ok_or("main.npdm: ACI0 outside file")?;
    if aci.len() < 0x18 || aci.get(..4) != Some(b"ACI0") {
        return Err("main.npdm: missing ACI0 header".into());
    }
    Ok(u64::from_le_bytes(
        aci[0x10..0x18]
            .try_into()
            .map_err(|_| "invalid integer or key field length")?,
    ))
}

pub(super) fn extract(input: &Path, keys: &Path) -> Result<Extracted, String> {
    let sources: Vec<Arc<dyn Storage>> = vec![Arc::new(FileStorage::open(input).map_err(err)?)];
    let mut key_bytes = std::fs::read(keys).map_err(|e| format!("read prod.keys: {e}"))?;
    let keys_text =
        std::str::from_utf8(&key_bytes).map_err(|_| "keys must be UTF-8 prod.keys bytes")?;
    let mut keys = KeySet::parse(keys_text);
    key_bytes.fill(0);
    if keys.header_key().is_none() {
        return Err("prod.keys has no valid header_key".into());
    }
    let mut containers = Vec::new();
    for raw in sources {
        let is_xci = raw.size() >= 0x138 && raw.read_vec(0x100, 4).map_err(err)? == b"HEAD";
        let (raw, entries) = if is_xci {
            let secure = xci_secure(&raw, "XCI").map_err(err)?;
            let entries = read_hfs0(secure.as_ref()).map_err(err)?;
            (secure, entries)
        } else {
            let entries = read_pfs0(raw.as_ref()).map_err(err)?;
            (raw, entries)
        };
        let mut names = BTreeSet::new();
        for e in &entries {
            let name = e.name.to_ascii_lowercase();
            if !names.insert(name.clone()) {
                return Err("duplicate container member".into());
            }
            if name.ends_with(".ncz") {
                return Err("compressed NCZ is not supported".into());
            }
            if name.ends_with(".tik") {
                let ticket = raw
                    .read_vec(e.offset, 0x2c0.min(usize::try_from(e.size).map_err(err)?))
                    .map_err(err)?;
                if ticket.len() < 0x2c0 || ticket[..4] != [4, 0, 1, 0] || ticket[0x281] != 0 {
                    return Err("only common RSA-2048/SHA-256 tickets are supported".into());
                }
                keys.add_title_key(
                    ticket[0x2a0..0x2b0]
                        .try_into()
                        .map_err(|_| "invalid integer or key field length")?,
                    ticket[0x180..0x190]
                        .try_into()
                        .map_err(|_| "invalid integer or key field length")?,
                );
            }
        }
        containers.push((raw, entries));
    }
    let mut ncas: Vec<Nca> = Vec::new();
    let cache = Arc::new(BlockCache::new(16));
    for (raw, entries) in containers {
        let mut content_ids = BTreeSet::new();
        for e in &entries {
            let name = e.name.to_ascii_lowercase();
            if name.ends_with(".nca") {
                let id = name.split('.').next().unwrap_or("");
                if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("NCA filenames must retain their 32-digit content IDs".into());
                }
                if !content_ids.insert(id.to_owned()) {
                    return Err("duplicate NCA content ID".into());
                }
                let storage =
                    Arc::new(SubStorage::new(raw.clone(), e.offset, e.size).map_err(err)?);
                let nca = Nca::open(&keys, id.into(), storage).map_err(err)?;
                for (_, _, fs) in nca.sections.iter().flatten() {
                    if fs.hash_type == 2
                        && u64::from_le_bytes(
                            fs.raw[0x48..0x50]
                                .try_into()
                                .map_err(|_| "invalid integer or key field length")?,
                        ) > 256 * 1024 * 1024
                    {
                        return Err("ExeFS or metadata exceeds the 256 MiB working limit".into());
                    }
                }
                ncas.push(nca);
            }
        }
    }
    let metas = meta::metas(&keys, &ncas, &cache).map_err(err)?;
    let applications: Vec<_> = metas
        .iter()
        .filter(|meta| meta.kind == meta::APPLICATION)
        .collect();
    let patches: Vec<_> = metas
        .iter()
        .filter(|meta| meta.kind == meta::PATCH)
        .collect();
    if applications.len() != 1
        || patches.len() > 1
        || applications.len() + patches.len() != metas.len()
    {
        return Err("expected one base application and at most one bundled update; update-only, DLC and multi-title containers are unsupported".into());
    }
    let app = applications[0];
    let patch = patches.first().copied();
    if patch.is_some_and(|p| p.title != app.title | 0x800 || p.version < app.version) {
        return Err("update does not match the base application".into());
    }
    let select_program = |meta: &meta::Cnmt| -> Result<&Nca, String> {
        let programs: Vec<_> = ncas
            .iter()
            .filter(|n| n.content_type == ContentType::Program && meta.contents.contains(&n.id))
            .collect();
        if programs.len() != 1 || programs[0].title_id != app.title {
            return Err("expected exactly one matching Program NCA in each CNMT".into());
        }
        Ok(programs[0])
    };
    let base = select_program(app)?;
    let update = patch.map(select_program).transpose()?;
    let program = update.unwrap_or(base);
    let effective = patch.unwrap_or(app);
    let exefs = version::read_exefs(&keys, program, &cache).map_err(err)?;
    let npdms: Vec<_> = exefs.iter().filter(|(n, _)| n == "main.npdm").collect();
    if npdms.len() != 1 {
        return Err("expected one main.npdm".into());
    }
    let npdm = &npdms[0].1;
    if program_id(npdm)? != app.title {
        return Err("NPDM program ID does not match Application CNMT and NCA".into());
    }
    let mut modules = Vec::new();
    let mut module_names = BTreeSet::new();
    for (name, data) in &exefs {
        if data.starts_with(b"NSO0") {
            if !module_names.insert(name) {
                return Err("duplicate NSO module".into());
            }
            modules.push((name.clone(), data.clone()));
        }
    }
    if !module_names.contains(&"main".to_string()) {
        return Err("ExeFS has no main NSO module".into());
    }
    let mut controls: Vec<_> = ncas
        .iter()
        .filter(|n| n.content_type == ContentType::Control && effective.contents.contains(&n.id))
        .collect();
    if controls.is_empty() && patch.is_some() {
        controls = ncas
            .iter()
            .filter(|n| n.content_type == ContentType::Control && app.contents.contains(&n.id))
            .collect();
    }
    if controls.len() > 1 {
        return Err("expected at most one matching Control NCA".into());
    }
    let control = controls
        .first()
        .map(|n| version::read_control(&keys, n, &cache))
        .transpose()
        .map_err(err)?;
    let mut files = Vec::new();
    let mut directories = Vec::new();
    let mut total = exefs.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
    let base_data = version::base_data(&keys, base, &cache).map_err(err)?;
    let data_update = update.filter(|n| n.sections[n.data_section()].is_some());
    if base_data.is_some() || data_update.is_some() {
        let rom = version::open_romfs(&keys, base_data, data_update, &cache).map_err(err)?;
        directories.push("romfs".into());
        for e in &rom.entries {
            let path = std::str::from_utf8(&e.path).map_err(|_| "RomFS filenames must be UTF-8")?;
            nxb_extract::safe_path::rel("RomFS", path, false).map_err(err)?;
            if path.contains('\\') {
                return Err("RomFS filename contains backslash".into());
            }
            if e.dir {
                directories.push(format!("romfs/{}", path.trim_start_matches('/')));
                continue;
            }
            total = total.checked_add(e.size).ok_or("extracted size overflow")?;
            if total > 256 * 1024 * 1024 {
                return Err("extracted game exceeds the native iOS adapter 256 MiB limit".into());
            }
            let data = rom
                .fs
                .read_vec(e.offset, usize::try_from(e.size).map_err(err)?)
                .map_err(err)?;
            files.push((format!("romfs/{}", path.trim_start_matches('/')), data));
        }
    }
    if total > 256 * 1024 * 1024 {
        return Err("extracted game exceeds the native iOS adapter 256 MiB limit".into());
    }
    let control_size = control.as_ref().map_or(0, |c| {
        c.nacp.len() + c.icon.as_ref().map_or(0, |(_, b)| b.len())
    });
    if total + control_size as u64 > 256 * 1024 * 1024 {
        return Err("extracted game exceeds the native iOS adapter 256 MiB limit".into());
    }
    let nacp = control.as_ref().map(|c| c.nacp.as_slice()).unwrap_or(&[]);
    if !nacp.is_empty() {
        files.push(("control.nacp".into(), nacp.to_vec()));
    }
    if let Some((_, icon)) = control.as_ref().and_then(|c| c.icon.as_ref()) {
        files.push(("icon.jpg".into(), icon.clone()));
    }
    let name = version::title_name(nacp).map_err(err)?;
    Ok(Extracted {
        title_id: app.title,
        name: if name.is_empty() {
            format!("{:016x}", app.title)
        } else {
            name
        },
        version: effective.version,
        version_name: version::display_version(nacp),
        npdm: npdm.clone(),
        modules,
        files,
        directories,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_npdm_identity_and_rejects_truncation() {
        let mut b = vec![0; 0x98];
        b[..4].copy_from_slice(b"META");
        b[0x70..0x74].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x74..0x78].copy_from_slice(&0x18u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"ACI0");
        b[0x90..0x98].copy_from_slice(&42u64.to_le_bytes());
        assert_eq!(program_id(&b).unwrap(), 42);
        assert!(program_id(&b[..0x97]).is_err());
    }
}
