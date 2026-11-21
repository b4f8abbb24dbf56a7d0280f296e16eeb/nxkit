//! Which way a save goes, and the player's choice before the start. Pure, as `ss_decide` and `read_choice` of
//! `save_sync.c`.

use crate::format::is_hash;

/// Which way the save goes at the first mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// Neither replaces the other.
    Keep,
    /// The cloud's save comes in.
    Pull,
    /// This device's save goes out.
    Push,
    /// Both changed and a front end asks the player.
    Ask,
}

/// Which way, from the three hashes ("" for none: no save here, no sync yet, nothing in the cloud). The second value
/// is true when both changed. Then `Ask` when a front end asks the player (`ask`), else the newer one by the times (the
/// same time: this device's).
pub fn decide(
    local: &str,
    base: &str,
    remote: &str,
    local_saved: i64,
    remote_saved: i64,
    ask: bool,
) -> (Way, bool) {
    if remote.is_empty() {
        return (
            if local.is_empty() {
                Way::Keep
            } else {
                Way::Push
            },
            false,
        );
    }
    if local == remote {
        return (Way::Keep, false);
    }
    if local.is_empty() {
        return (Way::Pull, false);
    }
    if !base.is_empty() && local == base {
        return (Way::Pull, false);
    }
    if !base.is_empty() && remote == base {
        return (Way::Push, false);
    }
    if ask {
        return (Way::Ask, true);
    }
    (
        if remote_saved > local_saved {
            Way::Pull
        } else {
            Way::Push
        },
        true,
    )
}

/// What the player chose before the start, as a front end passes it in `NXKIT_SAVE_SYNC_CHOICE` (the values of
/// `DM_SAVE_SYNC_CHOICE`). Any value says the front end asks the player when both changed. Unset, the newer save wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    /// Unset or not a choice: the newer save wins.
    #[default]
    None,
    /// `ask`: no choice yet. Both changed: neither replaces the other.
    Ask,
    /// `cloud:<hash>`: the cloud's save with that hash comes in.
    Cloud(String),
    /// `local:<hash>`, or `local:none` with an empty hash: this device's save replaces the cloud's with that hash.
    Local(String),
    /// `offline`: the cloud could not be read before the start. The mount does not wait for it.
    Offline,
}

impl Choice {
    /// The choice of a value. A value that is not one gives `None` and the reason.
    pub fn parse(v: &str) -> (Choice, Option<String>) {
        match v {
            "" => return (Choice::None, None),
            "ask" => return (Choice::Ask, None),
            "offline" => return (Choice::Offline, None),
            "local:none" => return (Choice::Local(String::new()), None),
            _ => {}
        }
        let pick = |h: &str, f: fn(String) -> Choice| is_hash(h).then(|| f(h.to_owned()));
        let c = match v.split_once(':') {
            Some(("cloud", h)) => pick(h, Choice::Cloud),
            Some(("local", h)) => pick(h, Choice::Local),
            _ => None,
        };
        match c {
            Some(c) => (c, None),
            None => (
                Choice::None,
                Some(format!(
                    "{v} is not a choice (ask, cloud:<hash>, local:<hash>, local:none, offline): ignored, the newer save \
                     wins"
                )),
            ),
        }
    }

    /// Whether a front end asks the player.
    pub(crate) fn asks(&self) -> bool {
        *self != Choice::None
    }
}

/// The way after the player's choice. A choice holds only for the cloud save it was made on (`now`, the hash of the
/// cloud's save or "" for none): when that changed, the way stays `way`, so a save that changed on both sides is
/// replaced on neither. Gives the way and whether the choice was used.
pub(crate) fn apply(choice: &Choice, way: Way, now: &str) -> (Way, bool) {
    if way == Way::Keep {
        return (way, false);
    }
    match choice {
        Choice::Cloud(h) if h == now => (Way::Pull, true),
        Choice::Local(h) if h == now => (Way::Push, true),
        _ => (way, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_way_follows_save_sync_c() {
        assert_eq!(decide("", "", "", 0, 0, false), (Way::Keep, false));
        assert_eq!(decide("L", "", "", 0, 0, false), (Way::Push, false));
        assert_eq!(decide("", "", "R", 0, 0, false), (Way::Pull, false));
        assert_eq!(decide("X", "B", "X", 0, 0, false), (Way::Keep, false));
        assert_eq!(decide("B", "B", "R", 9, 1, false), (Way::Pull, false));
        assert_eq!(decide("L", "B", "B", 1, 9, false), (Way::Push, false));
        assert_eq!(decide("L", "B", "R", 10, 20, false), (Way::Pull, true));
        assert_eq!(decide("L", "B", "R", 20, 10, false), (Way::Push, true));
        assert_eq!(decide("L", "", "R", 20, 10, false), (Way::Push, true));
        assert_eq!(decide("L", "B", "R", 10, 10, false), (Way::Push, true));
        assert_eq!(decide("L", "B", "R", 10, 20, true), (Way::Ask, true));
        assert_eq!(decide("L", "", "R", 20, 10, true), (Way::Ask, true));
        assert_eq!(decide("B", "B", "R", 9, 1, true), (Way::Pull, false));
    }

    #[test]
    fn a_choice_is_read_as_dm_save_sync_choice() {
        let h = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            Choice::parse(&format!("cloud:{h}")).0,
            Choice::Cloud(h.into())
        );
        assert_eq!(
            Choice::parse(&format!("local:{h}")).0,
            Choice::Local(h.into())
        );
        assert_eq!(Choice::parse("local:none").0, Choice::Local(String::new()));
        assert_eq!(Choice::parse("ask").0, Choice::Ask);
        assert_eq!(Choice::parse("offline").0, Choice::Offline);
        assert_eq!(Choice::parse(""), (Choice::None, None));
        for bad in [
            "cloud:none",
            "cloud:0123",
            "local:0123456789ABCDEF0123456789abcdef",
            "yes",
        ] {
            let (c, why) = Choice::parse(bad);
            assert!(c == Choice::None && why.is_some(), "{bad}");
        }
    }

    #[test]
    fn a_choice_holds_only_for_the_save_it_was_made_on() {
        let c = Choice::Cloud("a".into());
        assert_eq!(apply(&c, Way::Ask, "a"), (Way::Pull, true));
        assert_eq!(apply(&c, Way::Ask, "b"), (Way::Ask, false));
        assert_eq!(
            apply(&Choice::Local(String::new()), Way::Ask, ""),
            (Way::Push, true)
        );
        assert_eq!(apply(&c, Way::Keep, "a"), (Way::Keep, false));
    }
}
