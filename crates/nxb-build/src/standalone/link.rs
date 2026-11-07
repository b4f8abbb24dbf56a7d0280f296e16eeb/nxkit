//! The link stage: the dylib with every module's segments at its fixed address (R48), and the files of the run folder
//! that describe the modules.

use super::bundle;
use super::macho::{self, Seg};
use super::manifest;
use super::modules::Built;
use std::path::Path;

/// The image's identity: the first 16 bytes of a hash of the build IDs in load order.
pub fn uuid(mods: &[(Built, u64)]) -> [u8; 16] {
    let mut h = nxb_model::Sha256::new();
    for (b, _) in mods {
        h.update(b.build_id.as_bytes());
        h.update(&[0]);
    }
    let d = h.finalize().0;
    let mut u = [0u8; 16];
    u.copy_from_slice(&d[..16]);
    u
}

/// The text of `nxgame.dylib`: every segment of every module, by address.
pub fn dylib(mods: &[(Built, u64)]) -> Result<Vec<u8>, String> {
    let mut segs: Vec<Seg> = Vec::new();
    for (b, base) in mods {
        for g in &b.shape.segments {
            let addr = base
                .checked_add_signed(g.offset)
                .ok_or_else(|| format!("module {}: segment {} leaves the address space", b.name, g.name))?;
            let data = b
                .image
                .get(g.file_off..g.file_off + g.file_len)
                .ok_or_else(|| {
                    format!(
                        "module {}: segment {} reads {:#x} bytes at {:#x} of a {:#x} byte image",
                        b.name,
                        g.name,
                        g.file_len,
                        g.file_off,
                        b.image.len()
                    )
                })?;
            segs.push(Seg {
                name: &g.name,
                addr,
                data,
                mem_size: g.mem_size,
                exec: g.exec,
            });
        }
    }
    segs.sort_by_key(|s| s.addr);
    macho::write(&segs, uuid(mods))
}

fn put(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes `modules/<name>.{bin,imports.json,exports.json,fixups.tsv,gates.tsv}` into the run folder.
pub fn module_files(run: &Path, mods: &[(Built, u64)]) -> Result<(), String> {
    let dir = run.join("modules");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (b, _) in mods {
        let at = |s: &str| dir.join(format!("{}.{s}", b.name));
        put(&at("bin"), &b.image)?;
        put(&at("imports.json"), manifest::imports_json(&b.symbols.imports).as_bytes())?;
        put(&at("exports.json"), manifest::symbols_json(&b.symbols.exports).as_bytes())?;
        put(&at("fixups.tsv"), b.fixups.as_bytes())?;
        put(&at("gates.tsv"), &b.gates)?;
    }
    Ok(())
}

/// Writes and signs `nxgame.dylib`. Returns its size.
pub fn write_dylib(run: &Path, mods: &[(Built, u64)]) -> Result<u64, String> {
    let bytes = dylib(mods)?;
    let path = run.join("nxgame.dylib");
    put(&path, &bytes)?;
    bundle::sign(&path)?;
    Ok(bytes.len() as u64)
}
