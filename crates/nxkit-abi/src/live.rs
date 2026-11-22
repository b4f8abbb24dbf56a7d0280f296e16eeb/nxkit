//! Settings that change while the host runs (W35).
//!
//! Most settings are fixed at the start (W27). A diagnostic switch is different. It changes what the host prints or
//! looks at, not what the host builds, so it can change while the game runs. A person or an agent then turns it on in
//! the scene that shows the problem, without a new start of the game.
//!
//! - [`Live`]: the live part of the settings of one crate. A read gives a `&'static` value with one read lock. A
//!   change puts a new value in its place. The old value stays in memory, so a reader that holds it keeps a
//!   consistent value. The host changes a value only on a command of a person, so the cost is small.
//! - [`Memo`]: a value that code derives from live settings (a category check on a hot path), kept until the next
//!   change of any live setting. It takes the place of a `OnceLock<bool>`.
//! - [`generation`]: the count of the changes of all live settings. [`Memo`] and the rate limits compare with it, so a
//!   change gives every rate limit its budget again.
//! - [`Section`] and [`Board`]: each crate names its live keys and how it applies them. The host collects the sections
//!   on one board, which checks a change of a key and applies it to every crate that reads the key.

use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, PoisonError, RwLock};

use crate::env::{Env, PREFIX};

// Process-wide (W33 (2)): every crate's live settings and every memo of them compare with this one count.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// The count of the changes of all live settings, from 1.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// The live part of the settings of one crate. A crate keeps one in a `static` and fills it from the snapshot at the
/// start and from the board after a change.
pub struct Live<T: 'static> {
    value: RwLock<Option<&'static T>>,
    defaults: OnceLock<T>,
}

impl<T: Default + Send + Sync + 'static> Live<T> {
    /// An empty cell. It gives the defaults until the first [`Live::set`].
    pub const fn new() -> Self {
        Live {
            value: RwLock::new(None),
            defaults: OnceLock::new(),
        }
    }

    /// The value now (the defaults before the first set).
    pub fn get(&'static self) -> &'static T {
        let now = *self.value.read().unwrap_or_else(PoisonError::into_inner);
        now.unwrap_or_else(|| self.defaults.get_or_init(T::default))
    }

    /// Puts `value` in place of the value now and counts a change. The old value stays in memory (see the module).
    pub fn set(&self, value: T) {
        let value: &'static T = Box::leak(Box::new(value));
        *self.value.write().unwrap_or_else(PoisonError::into_inner) = Some(value);
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

impl<T: Default + Send + Sync + 'static> Default for Live<T> {
    fn default() -> Self {
        Live::new()
    }
}

/// A value of up to 32 bits that code derives from live settings, kept until the next change of any of them.
pub struct Memo(AtomicU64);

impl Memo {
    /// An empty memo. The first read derives the value.
    pub const fn new() -> Self {
        Memo(AtomicU64::new(0))
    }

    /// The value, derived again with `derive` after a change of a live setting.
    pub fn get(&self, derive: impl FnOnce() -> u32) -> u32 {
        // The tag of a generation is never 0, so the empty memo never matches.
        let tag = generation() % u64::from(u32::MAX) + 1;
        let v = self.0.load(Ordering::Relaxed);
        if v >> 32 == tag {
            return v as u32;
        }
        // The tag was read before the value was derived: a change during the derive makes the next read derive again.
        let n = derive();
        self.0.store(tag << 32 | u64::from(n), Ordering::Relaxed);
        n
    }

    /// [`Memo::get`] for a switch.
    pub fn flag(&self, derive: impl FnOnce() -> bool) -> bool {
        self.get(|| u32::from(derive())) != 0
    }
}

impl Default for Memo {
    fn default() -> Self {
        Memo::new()
    }
}

/// One live key of a crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveKey {
    /// The name, `NXKIT_*`.
    pub name: &'static str,
    /// One line on what the key does, for the `keys` command of the host.
    pub help: &'static str,
}

/// The live keys of one crate and how it applies a change of them.
#[derive(Clone, Copy, Debug)]
pub struct Section {
    /// The crate.
    pub owner: &'static str,
    /// The keys that the crate reads again after a change.
    pub keys: &'static [LiveKey],
    /// Parses the live settings of the crate from `env` and puts them in its [`Live`] cell.
    pub apply: fn(&Env),
}

/// Why the board refused a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveError {
    /// The name does not start with `NXKIT_`.
    NotASetting(String),
    /// The key is a setting of the start (W27): a change needs a new start of the game.
    StartOnly(String),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::NotASetting(n) => write!(f, "{n} is not an {PREFIX}* setting"),
            LiveError::StartOnly(n) => write!(
                f,
                "{n} is read at the start only: it needs a new start of the game"
            ),
        }
    }
}

impl std::error::Error for LiveError {}

/// One row of [`Board::rows`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The key.
    pub name: &'static str,
    /// The value now, `None` when it is not set.
    pub value: Option<String>,
    /// The help line of the first crate that names the key.
    pub help: &'static str,
    /// The crates that read the key.
    pub owners: Vec<&'static str>,
}

/// The live keys of every crate and their values now. The host owns one board and changes keys only through it.
#[derive(Debug)]
pub struct Board {
    env: Env,
    sections: Vec<Section>,
}

impl Board {
    /// A board over the snapshot of the start. It applies every section once, so the live settings of every crate
    /// come from the same snapshot as its settings of the start. The host makes it with the settings slots, before
    /// it builds any part.
    pub fn new(env: Env, sections: Vec<Section>) -> Self {
        for s in &sections {
            (s.apply)(&env);
        }
        Board { env, sections }
    }

    /// Sets `name` to `value` (`None` removes it) and applies the change to every crate that reads the key. Returns
    /// the crates.
    pub fn set(&mut self, name: &str, value: Option<&str>) -> Result<Vec<&'static str>, LiveError> {
        if !name.starts_with(PREFIX) {
            return Err(LiveError::NotASetting(name.to_owned()));
        }
        let owners: Vec<Section> = self
            .sections
            .iter()
            .filter(|s| s.keys.iter().any(|k| k.name == name))
            .copied()
            .collect();
        if owners.is_empty() {
            return Err(LiveError::StartOnly(name.to_owned()));
        }
        self.env.put(name, value.map(OsString::from));
        for s in &owners {
            (s.apply)(&self.env);
        }
        Ok(owners.iter().map(|s| s.owner).collect())
    }

    /// The value of `name` now, as the crates see it.
    pub fn value(&self, name: &str) -> Option<String> {
        self.env.var(name).ok()
    }

    /// Every live key once, by name, with its value now.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = Vec::new();
        for s in &self.sections {
            for k in s.keys {
                match rows.iter_mut().find(|r| r.name == k.name) {
                    Some(r) => r.owners.push(s.owner),
                    None => rows.push(Row {
                        name: k.name,
                        value: self.value(k.name),
                        help: k.help,
                        owners: vec![s.owner],
                    }),
                }
            }
        }
        rows.sort_by_key(|r| r.name);
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Debug, PartialEq)]
    struct Diag {
        on: bool,
    }

    static CELL: Live<Diag> = Live::new();

    fn apply(env: &Env) {
        CELL.set(Diag {
            on: env.var("NXKIT_T_ON").is_ok_and(|v| v == "1"),
        });
    }

    const KEYS: &[LiveKey] = &[LiveKey {
        name: "NXKIT_T_ON",
        help: "a test switch",
    }];

    #[test]
    fn a_board_applies_a_live_key_and_refuses_the_others() {
        assert!(!CELL.get().on, "the defaults before a set");
        let section = Section {
            owner: "t",
            keys: KEYS,
            apply,
        };
        let mut board = Board::new(Env::default(), vec![section]);
        let memo = Memo::new();
        assert!(!memo.flag(|| CELL.get().on));
        assert_eq!(board.set("NXKIT_T_ON", Some("1")), Ok(vec!["t"]));
        assert!(CELL.get().on);
        assert!(
            memo.flag(|| CELL.get().on),
            "a memo derives again after a change"
        );
        assert_eq!(board.value("NXKIT_T_ON").as_deref(), Some("1"));
        assert_eq!(board.set("NXKIT_T_ON", None), Ok(vec!["t"]));
        assert!(!CELL.get().on && !memo.flag(|| CELL.get().on));
        assert_eq!(
            board.set("NXKIT_PRIVATE", Some("1")),
            Err(LiveError::StartOnly("NXKIT_PRIVATE".into()))
        );
        assert_eq!(
            board.set("HOME", Some("/")),
            Err(LiveError::NotASetting("HOME".into()))
        );
        let rows = board.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].name, rows[0].value.as_deref()),
            ("NXKIT_T_ON", None)
        );
    }

    #[test]
    fn a_memo_keeps_its_value_until_a_change() {
        let memo = Memo::new();
        assert_eq!(memo.get(|| 7), 7);
        // Another test can count a change at the same time: then the memo derives again, which is correct too.
        let before = generation();
        let again = memo.get(|| 9);
        if generation() == before {
            assert_eq!(again, 7, "no change, no new derive");
        }
    }
}
