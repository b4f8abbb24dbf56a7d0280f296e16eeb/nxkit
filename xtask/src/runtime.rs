//! The checks of the runtime crates (layers `R0` to `R4`).
//!
//! - `settings` (W27): an `NXKIT_*` key is read only in the settings module of its crate (a file `settings.rs`, the
//!   host's `hostenv.rs`), from the snapshot `nxkit_abi::env::Env`. Anywhere else in a runtime crate a call of `var`
//!   or `var_os` (a function or a method) or of `getenv` with a string that starts with `NXKIT_`, and a format string
//!   that builds such a name, fail. A read of the process environment (`env::var`, `env::var_os`, `getenv`) with such
//!   a name or with a name that is not a literal fails everywhere, settings modules included. Variables of the system
//!   (`HOME`, `TZ`) are read where they are used. The places in [`SETTINGS_EXEMPT`] are allowed.
//! - `statics` (W26): the process-wide statics (a `static` whose type names `OnceLock`, `OnceCell`, `LazyLock`,
//!   `Mutex`, `RwLock`, `Condvar`, `Slot`, `Live`, `Memo` or an `Atomic*`) of each runtime crate are counted and compared with
//!   `xtask/statics.txt`. More fails. Fewer fails until the file is lowered, so the number only goes down.
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

/// The settings modules: the one place of a crate that reads `NXKIT_*` keys from the snapshot.
fn settings_module(rel: &str) -> bool {
    rel.ends_with("/settings.rs")
        || rel == "nxkit-host/src/hostenv.rs"
        || rel == "nxkit-abi/src/env.rs"
}

/// The places where the tokens of one file read an `NXKIT_*` key by name, from the snapshot or anywhere else: a
/// `var`/`var_os`/`getenv` call with such a string, or a format string that builds such a name.
fn key_reads(toks: &[(Tok, usize)]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for i in 0..toks.len() {
        match &toks[i].0 {
            Tok::Ident(n) if n == "var" || n == "var_os" || n == "getenv" => {
                if matches!(toks.get(i + 1), Some((Tok::Punct('('), _)))
                    && let Some((Tok::Str(s), _)) = toks.get(i + 2)
                    && s.starts_with("NXKIT_")
                {
                    out.push((toks[i].1, s.clone()));
                }
            }
            Tok::Str(s) if s.starts_with("NXKIT_") && s.contains('{') => {
                out.push((toks[i].1, format!("a name built from {s:?}")));
            }
            _ => {}
        }
    }
    out
}

/// The types that make a static process-wide state.
const STATE_TYPES: &[&str] = &[
    "OnceLock", "OnceCell", "LazyLock", "Mutex", "RwLock", "Condvar", "Slot", "Live", "Memo",
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
            if settings_module(&rel_s) {
                continue;
            }
            for (line, what) in key_reads(&toks) {
                errs.push(format!(
                    "crates/{rel_s}:{line}: reads the key {what} outside the settings module of the crate; add a typed \
                     field to its settings (W27)"
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
    fn a_key_read_outside_the_settings_fails() {
        let src = "fn f(env: &Env) { env.var(\"NXKIT_A\"); s.var(x); format!(\"NXKIT_{}_B\", n); \
                   eprintln!(\"set NXKIT_C=1\"); }";
        let reads: Vec<String> = key_reads(&tokens(src))
            .into_iter()
            .map(|(_, w)| w)
            .collect();
        assert_eq!(reads, ["NXKIT_A", "a name built from \"NXKIT_{}_B\""]);
        assert!(
            settings_module("nxkit-gpu/src/settings.rs")
                && !settings_module("nxkit-gpu/src/diag.rs")
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
