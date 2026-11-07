//! Thread roles and the QoS class each role gets.
//!
//! The QoS class follows what a thread does, not its Switch priority. A title module supplies the table that says
//! which thread has which role. The table is matched by thread name, priority and entry address.

/// What a guest thread does for the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThreadRole {
    /// Builds and submits the frame.
    Render,
    /// Fills the audio buffers.
    Audio,
    /// The game's main thread.
    Main,
    /// Loads files and assets.
    Loading,
    /// Streams assets while the game runs.
    Streaming,
    /// A job or worker thread.
    Worker,
    /// No rule matched.
    Unknown,
}

impl ThreadRole {
    /// Every role, in the order of the statistics.
    pub const ALL: [ThreadRole; 7] = [
        ThreadRole::Render,
        ThreadRole::Audio,
        ThreadRole::Main,
        ThreadRole::Loading,
        ThreadRole::Streaming,
        ThreadRole::Worker,
        ThreadRole::Unknown,
    ];

    pub(crate) fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|r| *r == self).unwrap_or(6) as u8
    }

    pub(crate) fn from_u8(v: u8) -> ThreadRole {
        Self::ALL
            .get(usize::from(v))
            .copied()
            .unwrap_or(ThreadRole::Unknown)
    }

    /// A short name for logs.
    pub fn name(self) -> &'static str {
        match self {
            ThreadRole::Render => "render",
            ThreadRole::Audio => "audio",
            ThreadRole::Main => "main",
            ThreadRole::Loading => "loading",
            ThreadRole::Streaming => "streaming",
            ThreadRole::Worker => "worker",
            ThreadRole::Unknown => "unknown",
        }
    }
}

/// A macOS quality of service class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Qos {
    /// `QOS_CLASS_USER_INTERACTIVE`: performance cores, the frame depends on it.
    UserInteractive,
    /// `QOS_CLASS_USER_INITIATED`.
    UserInitiated,
    /// `QOS_CLASS_UTILITY`: may run on the efficiency cores.
    Utility,
    /// `QOS_CLASS_BACKGROUND`.
    Background,
}

impl Qos {
    /// The value of the `qos_class_t` constant.
    pub fn raw(self) -> u32 {
        match self {
            Qos::UserInteractive => 0x21,
            Qos::UserInitiated => 0x19,
            Qos::Utility => 0x11,
            Qos::Background => 0x09,
        }
    }

    /// The class for a `qos_class_t` value. `QOS_CLASS_DEFAULT` counts as user-initiated.
    pub fn from_raw(raw: u32) -> Option<Qos> {
        match raw {
            0x21 => Some(Qos::UserInteractive),
            0x19 | 0x15 => Some(Qos::UserInitiated),
            0x11 => Some(Qos::Utility),
            0x09 => Some(Qos::Background),
            _ => None,
        }
    }

    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Qos::UserInteractive => 1,
            Qos::UserInitiated => 2,
            Qos::Utility => 3,
            Qos::Background => 4,
        }
    }
}

/// The class a role runs at. Render, audio and the main thread are user-interactive. Loading and streaming are
/// utility, so they may use the efficiency cores. An unknown thread is user-interactive, which is what a thread
/// gets when nothing is known about it.
pub fn qos_for_role(role: ThreadRole) -> Qos {
    match role {
        ThreadRole::Render | ThreadRole::Audio | ThreadRole::Main | ThreadRole::Unknown => {
            Qos::UserInteractive
        }
        ThreadRole::Worker => Qos::UserInitiated,
        ThreadRole::Loading | ThreadRole::Streaming => Qos::Utility,
    }
}

/// How a rule matches a thread name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameMatch {
    /// Any name, including none.
    Any,
    /// The whole name.
    Exact(&'static str),
    /// The start of the name.
    Prefix(&'static str),
    /// Anywhere in the name.
    Contains(&'static str),
}

impl NameMatch {
    fn matches(self, name: &str) -> bool {
        match self {
            NameMatch::Any => true,
            NameMatch::Exact(s) => name == s,
            NameMatch::Prefix(s) => name.starts_with(s),
            NameMatch::Contains(s) => name.contains(s),
        }
    }
}

/// One rule of a role table. Every part that is set has to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleRule {
    /// The thread name.
    pub name: NameMatch,
    /// The Switch priority, from and to, both included.
    pub priority: Option<(i32, i32)>,
    /// The guest address of the thread's entry function.
    pub entry: Option<u64>,
    /// The role the thread gets.
    pub role: ThreadRole,
}

impl RoleRule {
    /// A rule that matches a name and gives a role.
    pub const fn named(name: NameMatch, role: ThreadRole) -> RoleRule {
        RoleRule {
            name,
            priority: None,
            entry: None,
            role,
        }
    }

    /// The same rule, limited to a priority range.
    pub const fn with_priority(mut self, from: i32, to: i32) -> RoleRule {
        self.priority = Some((from, to));
        self
    }

    /// The same rule, limited to one entry address.
    pub const fn with_entry(mut self, entry: u64) -> RoleRule {
        self.entry = Some(entry);
        self
    }

    fn matches(&self, info: &ThreadInfo<'_>) -> bool {
        self.name.matches(info.name)
            && self
                .priority
                .is_none_or(|(a, b)| (a..=b).contains(&info.priority))
            && self.entry.is_none_or(|e| e == info.entry)
    }
}

/// What a role table sees of a thread.
#[derive(Clone, Copy, Debug)]
pub struct ThreadInfo<'a> {
    /// The thread name, empty when the game has not set one.
    pub name: &'a str,
    /// The Switch priority.
    pub priority: i32,
    /// The guest entry address.
    pub entry: u64,
}

/// The rules a title gives, checked in order. The first match wins.
#[derive(Clone, Debug, Default)]
pub struct RoleTable {
    rules: Vec<RoleRule>,
}

impl RoleTable {
    /// A table from rules.
    pub fn new(rules: &[RoleRule]) -> RoleTable {
        RoleTable {
            rules: rules.to_vec(),
        }
    }

    /// The role of the first rule that matches, `None` when no rule does.
    pub fn classify(&self, info: &ThreadInfo<'_>) -> Option<ThreadRole> {
        self.rules.iter().find(|r| r.matches(info)).map(|r| r.role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &[RoleRule] = &[
        RoleRule::named(NameMatch::Prefix("Render"), ThreadRole::Render),
        RoleRule::named(NameMatch::Contains("Audio"), ThreadRole::Audio),
        RoleRule::named(NameMatch::Any, ThreadRole::Loading).with_priority(60, 63),
        RoleRule::named(NameMatch::Any, ThreadRole::Streaming).with_entry(0x7100_1234),
        RoleRule::named(NameMatch::Exact("Main"), ThreadRole::Main),
    ];

    fn info(name: &str, priority: i32, entry: u64) -> ThreadInfo<'_> {
        ThreadInfo {
            name,
            priority,
            entry,
        }
    }

    #[test]
    fn the_first_matching_rule_wins_by_name_priority_and_entry() {
        let t = RoleTable::new(RULES);
        assert_eq!(
            t.classify(&info("RenderThread", 40, 0)),
            Some(ThreadRole::Render)
        );
        assert_eq!(
            t.classify(&info("nn::AudioOut", 40, 0)),
            Some(ThreadRole::Audio)
        );
        assert_eq!(t.classify(&info("", 61, 0)), Some(ThreadRole::Loading));
        assert_eq!(
            t.classify(&info("", 40, 0x7100_1234)),
            Some(ThreadRole::Streaming)
        );
        assert_eq!(t.classify(&info("Main", 44, 0)), Some(ThreadRole::Main));
        assert_eq!(t.classify(&info("MainX", 44, 0)), None);
        assert_eq!(RoleTable::default().classify(&info("Main", 44, 0)), None);
    }

    #[test]
    fn roles_map_to_qos_by_what_the_thread_does() {
        use Qos::*;
        use ThreadRole::*;
        let expect = [
            (Render, UserInteractive),
            (Audio, UserInteractive),
            (Main, UserInteractive),
            (Worker, UserInitiated),
            (Loading, Utility),
            (Streaming, Utility),
            (Unknown, UserInteractive),
        ];
        for (role, qos) in expect {
            assert_eq!(qos_for_role(role), qos, "{}", role.name());
            assert_eq!(ThreadRole::from_u8(role.to_u8()), role);
        }
        assert_eq!(expect.len(), ThreadRole::ALL.len());
        for q in [UserInteractive, UserInitiated, Utility, Background] {
            assert_eq!(Qos::from_raw(q.raw()), Some(q));
        }
    }
}
