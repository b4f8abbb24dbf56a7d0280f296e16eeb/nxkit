//! The chain version V of the shader library (`b-<V>/`) and the digest of a game profile's inputs (`b-<V>/inputs.tsv`).

use std::fs;
use std::path::Path;

/// The profile tables of the inputs digest.
const DEFAULT_TABLES: &[&str] = &[
    "shader_facts.tsv",
    "shader_map.tsv",
    "shader_map.tsv.conflicts.tsv",
    "shader_pairs.tsv",
    "pso_pairs.tsv",
    "bf_want.tsv",
    "geo_mesh.tsv",
    "dual_want.tsv",
];

/// The translation options at their defaults (`XLATE_DEBUG=1`, `XLATE_REDUCED=1`): the only ones the host loads.
const OPTIONS: &str = "debug=True reduced=True";

/// The chain's own revision. Raise it with every change of the translation that
/// the crate version does not carry.
const STANDALONE_REVISION: &str = "nxkit-chain-1";

/// V of the shader step: the shader step is this workspace's code, so the version is a hash of
/// `STANDALONE_REVISION`, this crate's version and the translation options. A changed translator that keeps the crate version needs a new revision.
pub fn standalone_version() -> String {
    let mut h = nxb_model::Sha256::new();
    h.update(STANDALONE_REVISION.as_bytes());
    h.update(&[0]);
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(&[0]);
    h.update(OPTIONS.as_bytes());
    nxb_model::to_hex(&h.finalize().0)[..12].to_string()
}

/// The profile's `nvn_*` / `shader_*` rows of `game.tsv`, tab-joined.
pub fn profile_rows(game_tsv: &str) -> String {
    let mut s = String::new();
    for line in game_tsv.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut cells = line.split('\t');
        let key = cells.next().unwrap_or("").trim();
        if key.starts_with("nvn_") || key.starts_with("shader_") {
            s.push_str(key);
            for v in cells {
                s.push('\t');
                s.push_str(v.trim());
            }
            s.push('\n');
        }
    }
    s
}

/// The digest of the profile's tables and its `nvn_*` / `shader_*` rows.
pub fn inputs(profile: &Path) -> Result<String, String> {
    let t: Vec<String> = DEFAULT_TABLES.iter().map(|s| (*s).to_string()).collect();
    inputs_with(&t, profile)
}

fn inputs_with(tables: &[String], profile: &Path) -> Result<String, String> {
    let mut h = nxb_model::Sha256::new();
    for t in tables {
        let body = fs::read(profile.join(t)).unwrap_or_else(|_| b"missing".to_vec());
        h.update(format!("game/{t}").as_bytes());
        h.update(&[0]);
        h.update(&body);
        h.update(&[0]);
    }
    let g = profile.join("game.tsv");
    let text = fs::read_to_string(&g).map_err(|e| format!("{}: {e}", g.display()))?;
    h.update(profile_rows(&text).as_bytes());
    Ok(nxb_model::to_hex(&h.finalize().0)[..12].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_take_build_keys_only() {
        let t = "# c\nname\tTitleA\nshader_versions\t v0 \tv1\nnvn_x\t1\n\nother\t2\n";
        assert_eq!(profile_rows(t), "shader_versions\tv0\tv1\nnvn_x\t1\n");
    }

    #[test]
    fn the_standalone_version_is_twelve_hex_digits_and_stable() {
        let v = standalone_version();
        assert_eq!(v.len(), 12);
        assert!(v.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(v, standalone_version());
    }

    #[test]
    fn missing_profile_is_an_error() {
        assert!(inputs(Path::new("/nonexistent-profile")).is_err());
    }
}
