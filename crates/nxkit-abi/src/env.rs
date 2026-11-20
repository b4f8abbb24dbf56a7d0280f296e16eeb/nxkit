//! The one source of the host's settings from the environment (W27).
//!
//! [`Env`] is a snapshot of every `NXKIT_*` variable. The host takes it once at start ([`Env::capture`]), installs it
//! ([`install`]) before it builds any part, and passes it to the parts that it builds. Code that has no context to
//! carry it (a guest export, a switch of a diagnostic deep in the encoder) reads the installed snapshot through
//! [`get`], [`var`] or [`var_os`]. No runtime crate reads an `NXKIT_*` variable from the process, and
//! `cargo run -p xtask -- settings` checks that. Variables of the system (`HOME`, `TZ`) are not settings and are read
//! where they are used.
//!
//! Nothing sets an `NXKIT_*` variable while the host runs, so a value read from the snapshot is the value that a read
//! of the process gives. [`var`] and [`var_os`] answer like `std::env::var` and `std::env::var_os`.

use std::collections::BTreeMap;
use std::env::VarError;
use std::ffi::OsString;
use std::sync::OnceLock;

/// The prefix of the host's variables.
pub const PREFIX: &str = "NXKIT_";

/// A snapshot of the `NXKIT_*` variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, OsString>,
}

impl Env {
    /// The `NXKIT_*` variables of this process, now.
    pub fn capture() -> Env {
        Env::from_pairs(std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v))))
    }

    /// A snapshot of the pairs whose name starts with [`PREFIX`]; the others are left out.
    pub fn from_pairs<K: Into<String>, V: Into<OsString>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Env {
        let vars = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .filter(|(k, _)| k.starts_with(PREFIX))
            .collect();
        Env { vars }
    }

    /// The value of `name` as `std::env::var` gives it: `NotPresent` when it is not set (or not an `NXKIT_*` name),
    /// `NotUnicode` when it is not UTF-8.
    pub fn var(&self, name: impl AsRef<str>) -> Result<String, VarError> {
        match self.vars.get(name.as_ref()) {
            None => Err(VarError::NotPresent),
            Some(v) => v.clone().into_string().map_err(VarError::NotUnicode),
        }
    }

    /// The value of `name` as `std::env::var_os` gives it.
    pub fn var_os(&self, name: impl AsRef<str>) -> Option<OsString> {
        self.vars.get(name.as_ref()).cloned()
    }

    /// The names that are set, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().map(String::as_str)
    }
}

// Process-wide because a guest export and a switch deep in the encoder have no context to carry the settings in: they
// read the snapshot that the host installed at start. It is written once and read only afterwards.
static INSTALLED: OnceLock<Env> = OnceLock::new();

/// Installs the snapshot of the process. The host calls it once at start, before it builds a part. The first call
/// wins; it returns false when a snapshot was already there (for example one that [`get`] took).
pub fn install(env: Env) -> bool {
    INSTALLED.set(env).is_ok()
}

/// The installed snapshot. Without [`install`] (a test, a tool that uses a crate alone) it is taken at the first call.
pub fn get() -> &'static Env {
    INSTALLED.get_or_init(Env::capture)
}

/// `std::env::var` over the installed snapshot.
pub fn var(name: impl AsRef<str>) -> Result<String, VarError> {
    get().var(name)
}

/// `std::env::var_os` over the installed snapshot.
pub fn var_os(name: impl AsRef<str>) -> Option<OsString> {
    get().var_os(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_like_the_process_environment() {
        let env = Env::from_pairs([("NXKIT_A", "1"), ("NXKIT_B", ""), ("HOME", "/x")]);
        assert_eq!(env.var("NXKIT_A"), Ok("1".to_string()));
        assert_eq!(env.var("NXKIT_B"), Ok(String::new()));
        assert_eq!(env.var("NXKIT_C"), Err(VarError::NotPresent));
        assert_eq!(
            env.var("HOME"),
            Err(VarError::NotPresent),
            "only the host's own names"
        );
        assert_eq!(env.var_os("NXKIT_A"), Some(OsString::from("1")));
        assert_eq!(env.names().collect::<Vec<_>>(), ["NXKIT_A", "NXKIT_B"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_value_that_is_not_utf8_is_not_unicode() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![0xff, 0xfe]);
        let env = Env::from_pairs([("NXKIT_X", bad.clone())]);
        assert_eq!(env.var("NXKIT_X"), Err(VarError::NotUnicode(bad.clone())));
        assert_eq!(env.var_os("NXKIT_X"), Some(bad));
    }
}
