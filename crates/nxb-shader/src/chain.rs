//! `dmsh_chain.py`: the chain version V of the shader library (`b-<V>/`) and the digest of a game profile's inputs
//! (`b-<V>/inputs.tsv`). Both are read from the engine's files, so a changed engine gives another V as in Python.

use std::fs;
use std::path::Path;

/// The quoted strings of one Python statement starting at `start` (up to the line where its brackets balance).
fn statement_strings(text: &str, start: usize) -> Vec<String> {
    let (mut out, mut depth, mut quote) = (Vec::new(), 0i32, None::<char>);
    let mut cur = String::new();
    for c in text[start..].chars() {
        match quote {
            Some(q) if c == q => {
                out.push(std::mem::take(&mut cur));
                quote = None;
            }
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                '[' | '(' => depth += 1,
                ']' | ')' => depth -= 1,
                _ => {}
            },
        }
        if quote.is_none() && depth <= 0 && c == ']' {
            break;
        }
    }
    out
}

/// The value of a list assignment `NAME = [...]` plus every `NAME += [...]` of the Python source, as the strings in
/// order. A `NAME += ['<prefix>' + f for f in (...)]` statement gives `<prefix><item>` per item.
fn list_of(text: &str, name: &str) -> Result<Vec<String>, String> {
    let mut all = Vec::new();
    let mut off = 0;
    for line in text.split_inclusive('\n') {
        for op in [" = [", " += ["] {
            let head = format!("{name}{op}");
            if line.starts_with(&head) {
                let st = statement_strings(text, off + head.len() - 1);
                if line.contains(" for f in (") {
                    let (prefix, items) = st.split_first().ok_or("empty prefix statement")?;
                    all.extend(items.iter().map(|f| format!("{prefix}{f}")));
                } else {
                    all.extend(st);
                }
            }
        }
        off += line.len();
    }
    if all.is_empty() {
        return Err(format!("dmsh_chain.py: no list {name} found"));
    }
    Ok(all)
}

fn chain_source(engine: &Path) -> Result<String, String> {
    let p = engine.join("dm/gfx/tools/dmsh_chain.py");
    fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
}

/// The files V covers, relative to `dm/gfx`, as the engine's `dmsh_chain.FILES` names them, same order.
pub fn files(engine: &Path) -> Result<Vec<String>, String> {
    list_of(&chain_source(engine)?, "FILES")
}

/// The profile tables of the inputs digest of the engine this was written against, for callers without an engine
/// folder (`inputs`). `inputs_of` reads the engine's own list.
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

/// The profile tables of the inputs digest, as the engine's `dmsh_chain.INPUT_TABLES` names them, same order.
pub fn input_tables(engine: &Path) -> Result<Vec<String>, String> {
    list_of(&chain_source(engine)?, "INPUT_TABLES")
}

/// The translation options at their defaults (`XLATE_DEBUG=1`, `XLATE_REDUCED=1`): the only ones the host loads.
const OPTIONS: &str = "debug=True reduced=True";

/// `dmsh_chain.version()`: V for the engine at `engine` (the folder holding `dm/` and `third_party/`).
pub fn version(engine: &Path) -> Result<String, String> {
    let gfx = engine.join("dm/gfx");
    let mut h = nxb_model::Sha256::new();
    let src = chain_source(engine)?;
    if !src.contains("'debug=%s reduced=%s'") {
        return Err("dmsh_chain.py: the options string changed, update nxb_shader::chain".into());
    }
    for f in &files(engine)? {
        let p = gfx.join(f);
        let body = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        h.update(f.as_bytes());
        h.update(&[0]);
        h.update(&body);
        h.update(&[0]);
    }
    h.update(OPTIONS.as_bytes());
    Ok(nxb_model::to_hex(&h.finalize().0)[..12].to_string())
}

/// The chain's own revision for a build without an engine folder. Raise it with every change of the translation that
/// the crate version does not carry.
const STANDALONE_REVISION: &str = "nxkit-chain-1";

/// V for a build without the engine (R49): the shader step is this workspace's code, so the version is a hash of
/// `STANDALONE_REVISION`, this crate's version and the translation options. The engine's `dm/gfx` files do not
/// enter it, as the build does not run them. A changed translator that keeps the crate version needs a new revision.
pub fn standalone_version() -> String {
    let mut h = nxb_model::Sha256::new();
    h.update(STANDALONE_REVISION.as_bytes());
    h.update(&[0]);
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(&[0]);
    h.update(OPTIONS.as_bytes());
    nxb_model::to_hex(&h.finalize().0)[..12].to_string()
}

/// `dmsh_chain.profile_rows`: the profile's `nvn_*` / `shader_*` rows of `game.tsv`, tab-joined.
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

/// `dmsh_chain.inputs` with the table list of the engine at `engine`.
pub fn inputs_of(engine: &Path, profile: &Path) -> Result<String, String> {
    inputs_with(&input_tables(engine)?, profile)
}

/// `dmsh_chain.inputs` with the table list of the engine this was written against.
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
    fn lists_are_read_from_the_python_source() {
        let d = std::env::temp_dir().join(format!("nxb-chain-{}", std::process::id()));
        let t = d.join("dm/gfx/tools");
        fs::create_dir_all(&t).unwrap();
        fs::write(
            t.join("dmsh_chain.py"),
            "FILES = ['a.py', 'b/c.h']  # x\n# FILES += ['no']\nFILES += ['../tp/' + f for f in (\n    'x.cs', 'y/z.cs')]\nINPUT_TABLES = ['t.tsv']\n",
        )
        .unwrap();
        assert_eq!(
            files(&d).unwrap(),
            ["a.py", "b/c.h", "../tp/x.cs", "../tp/y/z.cs"]
        );
        assert_eq!(input_tables(&d).unwrap(), ["t.tsv"]);
        fs::remove_dir_all(&d).unwrap();
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
        assert!(version(Path::new("/nonexistent-engine")).is_err());
    }
}
