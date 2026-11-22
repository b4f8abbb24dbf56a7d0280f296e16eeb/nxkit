//! The one source of the host's settings from the environment (W27).
//!
//! [`Env`] is a snapshot of every `NXKIT_*` variable. The host takes it once at start ([`Env::capture`]). Each runtime
//! crate has a typed settings struct with the keys, defaults and parsing of that crate (`from_env(&Env)`). The host
//! builds every one of them from the one snapshot and hands it over explicitly: in a constructor, a `Config`, or the
//! crate's [`Slot`] for code with no context. No runtime crate reads an `NXKIT_*` variable by name outside its
//! settings module, and `cargo run -p xtask -- settings` checks that. Variables of the system (`HOME`, `TZ`) are not
//! settings and are read where they are used.
//!
//! Nothing sets an `NXKIT_*` variable of the process while the host runs, so a value of the snapshot is the value
//! that a read of the process gives, or a saved choice of the player for a variable that the process does not set
//! ([`Env::fill`]). The live keys are the one exception (W35). The live board ([`crate::live::Board`]) keeps a copy of
//! the snapshot and changes the live keys in that copy only. [`Env::var`] and [`Env::var_os`] answer like
//! `std::env::var` and `std::env::var_os`.

use std::collections::BTreeMap;
use std::env::VarError;
use std::ffi::OsString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

    /// A snapshot of the pairs whose name starts with [`PREFIX`]. The snapshot leaves out the others.
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

    /// Sets `name` to `value` when the snapshot has no value for it, for a saved choice of the player
    /// ([`crate::launch`]). Returns whether it was set. A name without [`PREFIX`] is left out.
    pub fn fill(&mut self, name: &str, value: impl Into<OsString>) -> bool {
        if !name.starts_with(PREFIX) || self.vars.contains_key(name) {
            return false;
        }
        self.vars.insert(name.to_owned(), value.into());
        true
    }

    /// Sets `name` to `value`, or removes it for `None`. Only the live board ([`crate::live::Board`]) changes a
    /// snapshot after the start, and only its live keys. A name without [`PREFIX`] is left out.
    pub(crate) fn put(&mut self, name: &str, value: Option<OsString>) {
        if !name.starts_with(PREFIX) {
            return;
        }
        match value {
            Some(v) => self.vars.insert(name.to_owned(), v),
            None => self.vars.remove(name),
        };
    }

    /// The names that are set, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.vars.keys().map(String::as_str)
    }
}

/// Why a [`Slot`] refused its settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotError {
    /// The slot has its settings already: a second install is a wiring bug of the host.
    AlreadyInstalled(&'static str),
    /// Code read the slot before the host installed it and got the defaults: the order of the start is wrong.
    ReadBeforeInstall(&'static str),
}

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SlotError::AlreadyInstalled(n) => {
                write!(f, "the settings of {n} are installed already")
            }
            SlotError::ReadBeforeInstall(n) => {
                write!(
                    f,
                    "the settings of {n} were read before the host installed them"
                )
            }
        }
    }
}

impl std::error::Error for SlotError {}

/// The typed settings of one runtime crate, for the code of the crate that has no context to carry them (a guest
/// export, a switch deep in the encoder). The host builds the settings from the snapshot ([`Env`]) and installs them
/// once, before it builds any part ([`Slot::install`]).
///
/// [`Slot::get`] never reads the process environment. Before the install it gives the defaults and marks the slot, so
/// that a later install fails with [`SlotError::ReadBeforeInstall`]: in the host that is a hard error at the start. A
/// unit test or a tool that installs nothing gets the defaults.
pub struct Slot<T: 'static> {
    name: &'static str,
    value: OnceLock<T>,
    defaults: OnceLock<T>,
    early: AtomicBool,
}

impl<T: Default> Slot<T> {
    /// An empty slot for the settings of `name` (the crate).
    pub const fn new(name: &'static str) -> Self {
        Slot {
            name,
            value: OnceLock::new(),
            defaults: OnceLock::new(),
            early: AtomicBool::new(false),
        }
    }

    /// Installs the settings. Fails when the slot has settings already or was read before.
    pub fn install(&self, settings: T) -> Result<(), SlotError> {
        if self.early.load(Ordering::Acquire) {
            return Err(SlotError::ReadBeforeInstall(self.name));
        }
        self.value
            .set(settings)
            .map_err(|_| SlotError::AlreadyInstalled(self.name))
    }

    /// The installed settings, or the defaults when nothing is installed (unit tests and tools).
    pub fn get(&self) -> &T {
        if let Some(v) = self.value.get() {
            return v;
        }
        self.early.store(true, Ordering::Release);
        self.defaults.get_or_init(T::default)
    }
}

/// A size in bytes as a setting gives it: a number, a `0x` number, or a number with `M` (MiB) or `G` (GiB).
pub fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim();
    let (n, unit) = match v.char_indices().last()? {
        (i, 'M' | 'm') => (&v[..i], 1 << 20),
        (i, 'G' | 'g') => (&v[..i], 1 << 30),
        _ => (v, 1),
    };
    let n = match n.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok()?,
        None => n.parse().ok()?,
    };
    n.checked_mul(unit)
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

    #[test]
    fn a_fill_keeps_what_the_process_set() {
        let mut env = Env::from_pairs([("NXKIT_A", "1"), ("NXKIT_B", "")]);
        assert!(!env.fill("NXKIT_A", "2") && !env.fill("NXKIT_B", "2") && !env.fill("HOME", "/"));
        assert!(env.fill("NXKIT_C", "3"));
        assert_eq!(
            (env.var("NXKIT_A"), env.var("NXKIT_C")),
            (Ok("1".into()), Ok("3".into()))
        );
        assert_eq!(env.var("HOME"), Err(VarError::NotPresent));
    }

    #[test]
    fn a_slot_refuses_a_second_install_and_a_read_before_it() {
        static A: Slot<u32> = Slot::new("a");
        assert_eq!(A.install(5), Ok(()));
        assert_eq!(*A.get(), 5);
        assert_eq!(A.install(6), Err(SlotError::AlreadyInstalled("a")));
        static B: Slot<u32> = Slot::new("b");
        assert_eq!(*B.get(), 0, "the defaults before an install");
        assert_eq!(B.install(7), Err(SlotError::ReadBeforeInstall("b")));
        assert_eq!(*B.get(), 0);
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

    #[test]
    fn reads_sizes() {
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("0x1000"), Some(4096));
        assert_eq!(parse_size("512M"), Some(512 << 20));
        assert_eq!(parse_size(" 6G "), Some(6 << 30));
        assert_eq!(parse_size("6T"), None);
        assert_eq!(parse_size(""), None);
    }
}
