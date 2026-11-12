//! The title module of Animal Crossing: New Horizons for Nintendo Switch (`01006F8002326000`).
//!
//! The game is update 3.0.3 (v2228224) on SDK 20.5.4, graphics NVN. The engine tree names its engine: Nintendo EPD
//! (sead / agl / gsys). What every game of that engine shares is in the family `agl`. The modules are `main`, `subsdk0`
//! and `sdk`. This module holds what the engine tree knows of the game.
//!
//! The game builds without the engine ([`BuildHooks::standalone`]): the module holds the build IDs of v2228224, the
//! shader split settings, the romfs marker and the run settings of the engine's `game.tsv`. The NVN function list is
//! not a hand list: the build reads the names from the modules' own strings (an empty list asks for that). All nine
//! services run on nxkit.
//!
//! The rewrite rules are data in `rules/01006F8002326000.toml`: the build row of v2228224 and the engine's SDK-wide
//! rules for the `sdk` module (12 implementations of the node area of 62 MB and of the 2 MB alignment of large
//! regions).
//!
//! Left out: the frame step stubs of `main` (`FRAME_STEP`): a stub is a list of steps, not a byte cut, and no rule of
//! this module describes it.
//!
//! The form of the shared font is left to the core default (the TTF itself): on SDK 20 the engine serves the BFTTF form
//! when `main` imports `ScalableFontEngineHelper::Decode`, and nobody checked the imports of this game.
//!
//! The game links the clients of some services into its own `main` (`nn::sf::hipc`), and the host answers them with
//! zeroed output (`sf_services`).

use nxtitle::{
    BuildHooks, Evidence, Fact, Memory, Runtime, RuntimeHooks, Service, Settings, Standalone, Title, TitleId,
};

/// The title module.
pub struct Acnh;

/// The one instance the registry lists.
pub static ACNH: Acnh = Acnh;

const GAME_TSV: Evidence = Evidence::Recorded {
    source: "engine tree dm/games/01006F8002326000/game.tsv and the defaults of dm/games/game.py at 034e91a8",
    read: "2026-11-12",
};

/// The run settings: the values of `game.tsv`, and for a key it does not set the defaults of `game.py`. The display
/// version is the one of the build row of v2228224.
const SETTINGS: Fact<Settings> = Fact::new(
    Settings {
        memory: Memory {
            reserve: 0x1_0000_0000,
            alias: 0x1000_0000,
            stack: 0x1000_0000,
            app_pool: 0xCD50_0000,
            used: 0x7A0_0000,
            system_resource: 0,
            ..Settings::DEFAULT.memory
        },
        display_versions: &[("ff1d1c05670db6021c85b624a710b963", "3.0.3")],
        screen_aspect: true,
        pad_layout: "full",
        ..Settings::DEFAULT
    },
    GAME_TSV,
);

/// No list here: the build reads the NVN function names from the modules.
const NVN_FUNCTIONS: Fact<&'static str> = Fact::new(
    "",
    Evidence::Unverified {
        note: "the build lists the NVN function names in the read-only data of the modules: what the game can fetch \
               through the NVN loader. A run that lists the fetched names checks the list.",
    },
);

impl BuildHooks for Acnh {
    fn standalone(&self) -> Option<Standalone> {
        Some(Standalone {
            version: 2_228_224,
            modules: &[
                ("main", "ff1d1c05670db6021c85b624a710b963"),
                ("subsdk0", "6c6ceace7be94107a8bd440bc2d60704"),
                ("sdk", "a226048605d3597f90cb1067d141cd36"),
            ],
            nvn_functions: NVN_FUNCTIONS,
            shader_groups: "/Shader/=shader /System/=system /Effect/=effect /Model/=model /Layout/=layout \
                            /Swkbd/=swkbd",
            shader_groups_only: false,
            shader_note: "Animal Crossing: New Horizons shaders",
            shader_facts: None,
            romfs_marker: "/Movie/OpeningMovie.webm",
            settings: SETTINGS,
        })
    }

    fn rewrite_rules(&self) -> Option<&'static str> {
        Some(include_str!("../rules/01006F8002326000.toml"))
    }
}

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

/// The game builds without the engine, so nxkit runs every service.
const NXKIT: Fact<Runtime> = Fact::new(
    Runtime::Nxkit,
    Evidence::Unverified {
        note: "the game has no engine path in a standalone build. A run of the game on the Mac checks each service.",
    },
);

impl RuntimeHooks for Acnh {
    fn runtime(&self, _service: Service) -> Option<Fact<Runtime>> {
        Some(NXKIT)
    }

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

    /// The engine tree names the engine: Nintendo EPD (sead / agl / gsys).
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
    use nxb_rewrite::patches::PatchTable;

    #[test]
    fn profile_matches_the_module() {
        let p = Profile::from_toml(ACNH.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(ACNH.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(ACNH.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn standalone_data_is_complete() {
        let s = ACNH.standalone().unwrap();
        assert_eq!(s.version, 2_228_224);
        assert_eq!(s.modules.len(), 3);
        assert!(s.modules.iter().all(|(_, id)| id.len() == 32));
        assert!(s.nvn_functions.value.is_empty());
        assert!(s.romfs_marker.is_empty() || s.romfs_marker.starts_with('/'));
        assert!(s.settings.evidence.is_checked());
        assert_eq!(s.settings.value.display_versions[0].0, s.modules[0].1);
    }

    #[test]
    fn the_rules_hold_the_build_row() {
        let text = ACNH.rewrite_rules().unwrap();
        let t = PatchTable::from_toml(&[("t.toml".to_owned(), text.to_owned())]).unwrap();
        let s = ACNH.standalone().unwrap();
        let row = &t.gbuild["01006F8002326000"][0];
        assert_eq!(row[1], format!("v{}", s.version));
        for (name, id) in s.modules {
            assert!(row.contains(&format!("{name}={id}")), "{name}");
        }
        assert_eq!(t.cuts.values().map(Vec::len).sum::<usize>(), 0);
        assert_eq!(t.caps.values().map(Vec::len).sum::<usize>(), 0);
        let rules = |m: &str| t.rules.get("01006F8002326000").and_then(|r| r.get(m)).map_or(0, Vec::len);
        assert_eq!(rules("sdk"), 12);
        assert_eq!(rules("main") + rules("subsdk0") + rules("subsdk1"), 0);
        assert!(t.frame.is_empty());
        assert_eq!(text.matches("evidence = \"engine tree ").count(), text.matches("[[rule]]").count());
    }

    #[test]
    fn nxkit_runs_every_service() {
        let on: Vec<Service> = Service::ALL
            .into_iter()
            .filter(|s| ACNH.runtime(*s).map(|f| f.value) == Some(Runtime::Nxkit))
            .collect();
        assert_eq!(on, Service::ALL);
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
