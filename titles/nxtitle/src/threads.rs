//! The roles of a game's threads, as title facts.
//!
//! A title says which guest thread does what (render, audio, loading) by name, priority and entry address. The words
//! are the title API's own, so a title module needs no runtime crate. The kernel of the host (`nxkit-hos`) has the
//! matching table that sets the QoS class of a role; the host maps these rules onto it.

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
    /// Whether `name` matches.
    pub fn matches(self, name: &str) -> bool {
        match self {
            NameMatch::Any => true,
            NameMatch::Exact(s) => name == s,
            NameMatch::Prefix(s) => name.starts_with(s),
            NameMatch::Contains(s) => name.contains(s),
        }
    }
}

/// One thread rule. Every part that is set has to match.
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

    /// Whether the rule matches a thread with this name, priority and entry address.
    pub fn matches(&self, name: &str, priority: i32, entry: u64) -> bool {
        self.name.matches(name)
            && self
                .priority
                .is_none_or(|(a, b)| (a..=b).contains(&priority))
            && self.entry.is_none_or(|e| e == entry)
    }
}

/// The role of the first rule that matches, `None` when no rule does.
pub fn classify(rules: &[RoleRule], name: &str, priority: i32, entry: u64) -> Option<ThreadRole> {
    rules
        .iter()
        .find(|r| r.matches(name, priority, entry))
        .map(|r| r.role)
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

    #[test]
    fn first_match_wins() {
        assert_eq!(
            classify(RULES, "RenderThread", 61, 0),
            Some(ThreadRole::Render)
        );
        assert_eq!(classify(RULES, "MyAudio", 10, 0), Some(ThreadRole::Audio));
        assert_eq!(classify(RULES, "x", 62, 0), Some(ThreadRole::Loading));
        assert_eq!(
            classify(RULES, "x", 1, 0x7100_1234),
            Some(ThreadRole::Streaming)
        );
        assert_eq!(classify(RULES, "Main", 1, 0), Some(ThreadRole::Main));
        assert_eq!(classify(RULES, "Mainly", 1, 0), None);
    }
}
