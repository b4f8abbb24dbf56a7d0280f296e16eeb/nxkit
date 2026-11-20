//! The checks of the runtime crates (layers `R0` to `R4`).
//!
//! - `settings` (W27): no runtime crate reads an `NXKIT_*` variable from the process. A call of `env::var` or
//!   `env::var_os` (not the ones of `nxkit_abi::env`) or of `getenv` fails when its first argument is a string that
//!   starts with `NXKIT_` or is not a string at all. Variables of the system (`HOME`, `TZ`) are read where they are
//!   used. `nxkit_abi::env` itself, and the places in [`SETTINGS_EXEMPT`], are allowed.
//! - `statics` (W26): the process-wide statics (a `static` whose type names `OnceLock`, `OnceCell`, `LazyLock`,
//!   `Mutex`, `RwLock`, `Condvar` or an `Atomic*`) of each runtime crate are counted and compared with
//!   `xtask/statics.txt`. More fails; fewer fails until the file is lowered, so the number only goes down.
//!
//! Test code is left out, as in `modules`.

use crate::lex::Tok;
use crate::modgraph::graph;
use std::collections::BTreeMap;
use std::path::Path;

/// The baseline of `statics`, relative to the workspace root.
pub(crate) const STATICS_BASELINE: &str = "xtask/statics.txt";

/// Files that may read an `NXKIT_*` variable from the process, with the reason.
const SETTINGS_EXEMPT: &[(&str, &str)] = &[
    ("nxkit-abi/src/env.rs", "the snapshot itself"),
    (
        "nxkit-host/src/sys/allocspy.rs",
        "the allocator hook runs inside an allocation, so it uses libc getenv and no Rust collection",
    ),
];

/// The types that make a static process-wide state.
const STATE_TYPES: &[&str] = &[
    "OnceLock", "OnceCell", "LazyLock", "Mutex", "RwLock", "Condvar",
];

fn ident(t: Option<&(Tok, usize)>) -> Option<&str> {
    match t {
        Some((Tok::Ident(s), _)) => Some(s),
        _ => None,
    }
}

/// The places where the tokens of one file read a variable of the process that is a setting.
fn env_reads(toks: &[(Tok, usize)]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for i in 0..toks.len() {
        let name = ident(toks.get(i));
        let call = match name {
            Some("var" | "var_os") => {
                ident(toks.get(i.wrapping_sub(2))) == Some("env")
                    && matches!(toks.get(i.wrapping_sub(1)), Some((Tok::Path, _)))
                    && ident(toks.get(i.wrapping_sub(4))) != Some("nxkit_abi")
            }
            Some("getenv") => ident(toks.get(i.wrapping_sub(1))) != Some("fn"),
            _ => false,
        };
        if !call || !matches!(toks.get(i + 1), Some((Tok::Punct('('), _))) {
            continue;
        }
        let line = toks[i].1;
        match toks.get(i + 2) {
            Some((Tok::Str(s), _)) if !s.starts_with("NXKIT_") => {}
            Some((Tok::Str(s), _)) => out.push((line, s.clone())),
            _ => out.push((line, "a name that is not a literal".into())),
        }
    }
    out
}

/// The count of process-wide statics in the tokens of one file.
fn statics(toks: &[(Tok, usize)]) -> usize {
    let mut n = 0;
    for i in 0..toks.len() {
        if ident(toks.get(i)) != Some("static") {
            continue;
        }
        let mut k = i + 1;
        if ident(toks.get(k)) == Some("mut") {
            k += 1;
        }
        if ident(toks.get(k)).is_none() || !matches!(toks.get(k + 1), Some((Tok::Punct(':'), _))) {
            continue;
        }
        let ty = toks[k + 2..]
            .iter()
            .take_while(|(t, _)| !matches!(t, Tok::Punct('=' | ';')))
            .any(|(t, _)| {
                matches!(t, Tok::Ident(s) if STATE_TYPES.contains(&s.as_str()) || s.starts_with("Atomic"))
            });
        n += usize::from(ty);
    }
    n
}

/// The `settings` errors of the runtime crates (name, folder).
pub(crate) fn check_settings<'a>(
    crates: impl Iterator<Item = (&'a str, &'a Path)>,
    root: &Path,
) -> Vec<String> {
    let mut errs = Vec::new();
    for (_, dir) in crates {
        for (file, toks) in graph(&dir.join("src")).files {
            let rel = file.strip_prefix(root.join("crates")).unwrap_or(&file);
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            if SETTINGS_EXEMPT.iter().any(|(f, _)| *f == rel_s) {
                continue;
            }
            for (line, what) in env_reads(&toks) {
                errs.push(format!(
                    "crates/{rel_s}:{line}: reads {what} from the process; use the snapshot of `nxkit_abi::env` (W27)"
                ));
            }
        }
    }
    errs
}

/// The process-wide statics of each runtime crate (name, folder).
pub(crate) fn count_statics<'a>(
    crates: impl Iterator<Item = (&'a str, &'a Path)>,
) -> BTreeMap<String, usize> {
    crates
        .map(|(name, dir)| {
            let n = graph(&dir.join("src"))
                .files
                .iter()
                .map(|(_, t)| statics(t))
                .sum();
            (name.to_string(), n)
        })
        .collect()
}

/// Reads the baseline: `crate: count` per line, `#` starts a comment.
pub(crate) fn parse_statics(text: &str) -> BTreeMap<String, usize> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split('#').next()?.trim().split_once(':')?;
            Some((k.trim().to_string(), v.trim().parse().ok()?))
        })
        .collect()
}

/// Compares the counts with the baseline.
pub(crate) fn compare_statics(
    now: &BTreeMap<String, usize>,
    base: &BTreeMap<String, usize>,
) -> Vec<String> {
    let mut errs = Vec::new();
    for (k, &n) in now {
        let b = base.get(k).copied().unwrap_or(0);
        if n > b {
            errs.push(format!(
                "{k}: {n} process-wide statics, the baseline allows {b}. Own the state in a context, or give the reason \
                 at the static and raise {STATICS_BASELINE} in the same commit (W26)"
            ));
        } else if n < b {
            errs.push(format!(
                "{k}: {n} process-wide statics, fewer than {b}: lower {STATICS_BASELINE} to `{k}: {n}`"
            ));
        }
    }
    for k in base.keys().filter(|k| !now.contains_key(*k)) {
        errs.push(format!(
            "{k}: no such runtime crate, delete it from {STATICS_BASELINE}"
        ));
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::tokens;

    #[test]
    fn a_direct_read_of_a_setting_fails() {
        let src = "fn f() { std::env::var(\"NXKIT_A\"); env::var_os(name); nxkit_abi::env::var(\"NXKIT_B\"); \
                   std::env::var(\"HOME\"); getenv(c\"NXKIT_C\".as_ptr()); }\nextern \"C\" fn getenv(n: u64) {}";
        let reads: Vec<String> = env_reads(&tokens(src))
            .into_iter()
            .map(|(_, w)| w)
            .collect();
        assert_eq!(
            reads,
            ["NXKIT_A", "a name that is not a literal", "NXKIT_C"]
        );
    }

    #[test]
    fn process_wide_statics_are_counted() {
        let src = "static A: AtomicU64 = AtomicU64::new(0);\nstatic B: Mutex<Vec<u8>> = Mutex::new(Vec::new());\n\
                   static C: [u8; 3] = [0; 3];\nstatic mut D: Option<OnceLock<u8>> = None;\nfn f(x: &'static str) {}";
        assert_eq!(statics(&tokens(src)), 3);
    }

    #[test]
    fn the_static_count_only_goes_down() {
        let now: BTreeMap<String, usize> = [("k".to_string(), 3)].into_iter().collect();
        assert!(compare_statics(&now, &parse_statics("k: 3\n")).is_empty());
        assert!(compare_statics(&now, &parse_statics("k: 2\n"))[0].contains("allows 2"));
        assert!(compare_statics(&now, &parse_statics("k: 4 # old\n"))[0].contains("lower"));
    }
}
