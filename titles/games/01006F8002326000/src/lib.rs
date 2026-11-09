//! The title module of Animal Crossing: New Horizons for Nintendo Switch (`01006F8002326000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN, update 3.0.3 (v2228224), SDK 20.5.4. This module
//! holds the facts of the game that the system services need. The graphics needs of the game (`render_cond` and the
//! memory layout) are in the engine tree `dm/games/01006F8002326000/game.tsv` and are not in this module yet.

use nxtitle::{BuildHooks, Evidence, Fact, RuntimeHooks, Title, TitleId};

/// The title module.
pub struct Acnh;

/// The one instance the registry lists.
pub static ACNH: Acnh = Acnh;

const SF_SERVED: Evidence = Evidence::Recorded {
    source: "engine tree dm/host/hle_sf.c (sf_served and its header comment) at f25fa956",
    read: "2026-11-07",
};

/// The services whose clients the game links into its own `main` (`nn::sf::hipc`). `olsc:u` is the cloud save data
/// backup. The `caps:*` services are the capture services that the Switch 2 camera path reaches.
const SF_SERVICES: &[Fact<&str>] = &[
    Fact::new("olsc:u", SF_SERVED),
    Fact::new("caps:su", SF_SERVED),
    Fact::new("caps:u", SF_SERVED),
    Fact::new("caps:a", SF_SERVED),
    Fact::new("caps:c", SF_SERVED),
    Fact::new("caps:sc", SF_SERVED),
    Fact::new("caps:ss", SF_SERVED),
];

impl BuildHooks for Acnh {}

impl RuntimeHooks for Acnh {
    fn sf_services(&self) -> &'static [Fact<&'static str>] {
        SF_SERVICES
    }
}

impl Title for Acnh {
    fn id(&self) -> TitleId {
        TitleId(0x0100_6F80_0232_6000)
    }

    fn name(&self) -> &'static str {
        "Animal Crossing: New Horizons"
    }

    fn family(&self) -> Option<&'static str> {
        Some("agl")
    }

    fn profile(&self) -> Option<&'static str> {
        Some(include_str!("../profile.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxb_profile::Profile;

    #[test]
    fn profile_matches_the_module() {
        let p = Profile::from_toml(ACNH.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(ACNH.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(ACNH.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn every_served_service_has_a_record() {
        assert!(
            ACNH.sf_services()
                .iter()
                .all(|s| s.evidence.is_checked() && s.value.len() <= 8)
        );
    }
}
