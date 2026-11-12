//! The one list of families and title modules.
//!
//! Each module is behind its own cargo feature, named as its crate without the prefix (`agl` for `nxfamily-agl`,
//! `smbw` for `nxgame-smbw`), so a build can leave any of them out. This is the only crate that depends on family and
//! title crates. The core depends on this crate and on `nxtitle`.

use nxtitle::{Catalog, Family, Title};

/// The catalog of every module this build holds.
pub fn catalog() -> Result<Catalog, String> {
    let families: Vec<&'static dyn Family> = vec![
        #[cfg(feature = "agl")]
        &nxfamily_agl::AGL,
    ];
    let titles: Vec<&'static dyn Title> = vec![
        #[cfg(feature = "oot")]
        &nxgame_oot::OOT,
        #[cfg(feature = "botw")]
        &nxgame_botw::BOTW,
        #[cfg(feature = "acnh")]
        &nxgame_acnh::ACNH,
        #[cfg(feature = "mtf")]
        &nxgame_mtf::MTF,
        #[cfg(feature = "smbw")]
        &nxgame_smbw::SMBW,
        #[cfg(feature = "smo")]
        &nxgame_smo::SMO,
        #[cfg(feature = "pkbd")]
        &nxgame_pkbd::PKBD,
        #[cfg(feature = "aaic")]
        &nxgame_aaic::AAIC,
        #[cfg(feature = "mpr")]
        &nxgame_mpr::MPR,
        #[cfg(feature = "silksong")]
        &nxgame_silksong::SILKSONG,
        #[cfg(feature = "snbz")]
        &nxgame_snbz::SNBZ,
        #[cfg(feature = "mk8d")]
        &nxgame_mk8d::MK8D,
        #[cfg(feature = "swheist")]
        &nxgame_swheist::SWHEIST,
        #[cfg(feature = "ryza")]
        &nxgame_ryza::RYZA,
        #[cfg(feature = "mp4")]
        &nxgame_mp4::MP4,
        #[cfg(feature = "diablo3")]
        &nxgame_diablo3::DIABLO3,
        #[cfg(feature = "pkarceus")]
        &nxgame_pkarceus::PKARCEUS,
        #[cfg(feature = "ragingloop")]
        &nxgame_ragingloop::RAGINGLOOP,
        #[cfg(feature = "animalwell")]
        &nxgame_animalwell::ANIMALWELL,
        #[cfg(feature = "okami")]
        &nxgame_okami::OKAMI,
        #[cfg(feature = "sm3dwbf")]
        &nxgame_sm3dwbf::SM3DWBF,
        #[cfg(feature = "nfshp")]
        &nxgame_nfshp::NFSHP,
        #[cfg(feature = "ghosttrick")]
        &nxgame_ghosttrick::GHOSTTRICK,
        #[cfg(feature = "hwaoc")]
        &nxgame_hwaoc::HWAOC,
        #[cfg(feature = "celeste")]
        &nxgame_celeste::CELESTE,
        #[cfg(feature = "baba")]
        &nxgame_baba::BABA,
        #[cfg(feature = "sshd")]
        &nxgame_sshd::SSHD,
        #[cfg(feature = "cotl")]
        &nxgame_cotl::COTL,
        #[cfg(feature = "aw12")]
        &nxgame_aw12::AW12,
        #[cfg(feature = "mrsoh")]
        &nxgame_mrsoh::MRSOH,
        #[cfg(feature = "smbbr")]
        &nxgame_smbbr::SMBBR,
        #[cfg(feature = "obradinn")]
        &nxgame_obradinn::OBRADINN,
        #[cfg(feature = "trvdx")]
        &nxgame_trvdx::TRVDX,
        #[cfg(feature = "dbspz")]
        &nxgame_dbspz::DBSPZ,
        #[cfg(feature = "tgaac")]
        &nxgame_tgaac::TGAAC,
        #[cfg(feature = "bastion")]
        &nxgame_bastion::BASTION,
        #[cfg(feature = "fftic")]
        &nxgame_fftic::FFTIC,
        #[cfg(feature = "dq3hd")]
        &nxgame_dq3hd::DQ3HD,
        #[cfg(feature = "sentinels")]
        &nxgame_sentinels::SENTINELS,
        #[cfg(feature = "blasphemous2")]
        &nxgame_blasphemous2::BLASPHEMOUS2,
        #[cfg(feature = "dqb2")]
        &nxgame_dqb2::DQB2,
        #[cfg(feature = "subnautica")]
        &nxgame_subnautica::SUBNAUTICA,
        #[cfg(feature = "sentinelsjp")]
        &nxgame_sentinelsjp::SENTINELSJP,
        #[cfg(feature = "bayo3")]
        &nxgame_bayo3::BAYO3,
        #[cfg(feature = "dsr")]
        &nxgame_dsr::DSR,
        #[cfg(feature = "kirbyfl")]
        &nxgame_kirbyfl::KIRBYFL,
        #[cfg(feature = "mt2")]
        &nxgame_mt2::MT2,
        #[cfg(feature = "octo0")]
        &nxgame_octo0::OCTO0,
        #[cfg(feature = "unicornasia")]
        &nxgame_unicornasia::UNICORNASIA,
        #[cfg(feature = "fe3h")]
        &nxgame_fe3h::FE3H,
        #[cfg(feature = "nier")]
        &nxgame_nier::NIER,
        #[cfg(feature = "octo")]
        &nxgame_octo::OCTO,
        #[cfg(feature = "p5r")]
        &nxgame_p5r::P5R,
        #[cfg(feature = "yoshicw")]
        &nxgame_yoshicw::YOSHICW,
        #[cfg(feature = "ffres")]
        &nxgame_ffres::FFRES,
        #[cfg(feature = "oribf")]
        &nxgame_oribf::ORIBF,
        #[cfg(feature = "bba")]
        &nxgame_bba::BBA,
        #[cfg(feature = "p4g")]
        &nxgame_p4g::P4G,
        #[cfg(feature = "hollowknight")]
        &nxgame_hollowknight::HOLLOWKNIGHT,
        #[cfg(feature = "srwy")]
        &nxgame_srwy::SRWY,
        #[cfg(feature = "deadcells")]
        &nxgame_deadcells::DEADCELLS,
        #[cfg(feature = "mrkb")]
        &nxgame_mrkb::MRKB,
        #[cfg(feature = "unicorn")]
        &nxgame_unicorn::UNICORN,
        #[cfg(feature = "blasphemous")]
        &nxgame_blasphemous::BLASPHEMOUS,
        #[cfg(feature = "smt5v")]
        &nxgame_smt5v::SMT5V,
        #[cfg(feature = "ssbu")]
        &nxgame_ssbu::SSBU,
        #[cfg(feature = "zla")]
        &nxgame_zla::ZLA,
        #[cfg(feature = "dq11s")]
        &nxgame_dq11s::DQ11S,
        #[cfg(feature = "mlbs")]
        &nxgame_mlbs::MLBS,
        #[cfg(feature = "mps")]
        &nxgame_mps::MPS,
        #[cfg(feature = "aisni")]
        &nxgame_aisni::AISNI,
        #[cfg(feature = "d2r")]
        &nxgame_d2r::D2R,
        #[cfg(feature = "astral")]
        &nxgame_astral::ASTRAL,
        #[cfg(feature = "xc3")]
        &nxgame_xc3::XC3,
        #[cfg(feature = "bayo")]
        &nxgame_bayo::BAYO,
        #[cfg(feature = "rdr")]
        &nxgame_rdr::RDR,
        #[cfg(feature = "bayo2")]
        &nxgame_bayo2::BAYO2,
        #[cfg(feature = "peach")]
        &nxgame_peach::PEACH,
        #[cfg(feature = "portal")]
        &nxgame_portal::PORTAL,
        #[cfg(feature = "kirbysa")]
        &nxgame_kirbysa::KIRBYSA,
        #[cfg(feature = "ballxpit")]
        &nxgame_ballxpit::BALLXPIT,
        #[cfg(feature = "ais")]
        &nxgame_ais::AIS,
        #[cfg(feature = "swheist2")]
        &nxgame_swheist2::SWHEIST2,
        #[cfg(feature = "steinsgate")]
        &nxgame_steinsgate::STEINSGATE,
        #[cfg(feature = "seaofstars")]
        &nxgame_seaofstars::SEAOFSTARS,
        #[cfg(feature = "zeow")]
        &nxgame_zeow::ZEOW,
        #[cfg(feature = "sentinelsasia")]
        &nxgame_sentinelsasia::SENTINELSASIA,
        #[cfg(feature = "rpop")]
        &nxgame_rpop::RPOP,
        #[cfg(feature = "pkshield")]
        &nxgame_pkshield::PKSHIELD,
        #[cfg(feature = "oriwotw")]
        &nxgame_oriwotw::ORIWOTW,
        #[cfg(feature = "pkviolet")]
        &nxgame_pkviolet::PKVIOLET,
        #[cfg(feature = "sonicss")]
        &nxgame_sonicss::SONICSS,
        #[cfg(feature = "tsr")]
        &nxgame_tsr::TSR,
        #[cfg(feature = "swdig")]
        &nxgame_swdig::SWDIG,
        #[cfg(feature = "dread")]
        &nxgame_dread::DREAD,
        #[cfg(feature = "smpj")]
        &nxgame_smpj::SMPJ,
        #[cfg(feature = "davediver")]
        &nxgame_davediver::DAVEDIVER,
        #[cfg(feature = "isaac")]
        &nxgame_isaac::ISAAC,
        #[cfg(feature = "toad")]
        &nxgame_toad::TOAD,
        #[cfg(feature = "dkcrhd")]
        &nxgame_dkcrhd::DKCRHD,
        #[cfg(feature = "hades2")]
        &nxgame_hades2::HADES2,
        #[cfg(feature = "octo2")]
        &nxgame_octo2::OCTO2,
        #[cfg(feature = "pmok")]
        &nxgame_pmok::PMOK,
        #[cfg(feature = "pkscarlet")]
        &nxgame_pkscarlet::PKSCARLET,
        #[cfg(feature = "cuphead")]
        &nxgame_cuphead::CUPHEAD,
        #[cfg(feature = "feengage")]
        &nxgame_feengage::FEENGAGE,
        #[cfg(feature = "mcd2")]
        &nxgame_mcd2::MCD2,
        #[cfg(feature = "absolum")]
        &nxgame_absolum::ABSOLUM,
        #[cfg(feature = "dq7r")]
        &nxgame_dq7r::DQ7R,
        #[cfg(feature = "portal2")]
        &nxgame_portal2::PORTAL2,
        #[cfg(feature = "pksword")]
        &nxgame_pksword::PKSWORD,
        #[cfg(feature = "batmanak")]
        &nxgame_batmanak::BATMANAK,
        #[cfg(feature = "hwde")]
        &nxgame_hwde::HWDE,
        #[cfg(feature = "mhrise")]
        &nxgame_mhrise::MHRISE,
        #[cfg(feature = "pikmin4")]
        &nxgame_pikmin4::PIKMIN4,
        #[cfg(feature = "ffresasia")]
        &nxgame_ffresasia::FFRESASIA,
        #[cfg(feature = "nierasia")]
        &nxgame_nierasia::NIERASIA,
        #[cfg(feature = "mvdk")]
        &nxgame_mvdk::MVDK,
        #[cfg(feature = "cvbc")]
        &nxgame_cvbc::CVBC,
        #[cfg(feature = "smrpg")]
        &nxgame_smrpg::SMRPG,
        #[cfg(feature = "ffx")]
        &nxgame_ffx::FFX,
        #[cfg(feature = "catherine")]
        &nxgame_catherine::CATHERINE,
        #[cfg(feature = "dkctf")]
        &nxgame_dkctf::DKCTF,
        #[cfg(feature = "rotk8")]
        &nxgame_rotk8::ROTK8,
        #[cfg(feature = "fc27")]
        &nxgame_fc27::FC27,
        #[cfg(feature = "swdig2")]
        &nxgame_swdig2::SWDIG2,
        #[cfg(feature = "tristrat")]
        &nxgame_tristrat::TRISTRAT,
        #[cfg(feature = "ffcc")]
        &nxgame_ffcc::FFCC,
        #[cfg(feature = "silverpines")]
        &nxgame_silverpines::SILVERPINES,
        #[cfg(feature = "dq12hd")]
        &nxgame_dq12hd::DQ12HD,
        #[cfg(feature = "rhgroove")]
        &nxgame_rhgroove::RHGROOVE,
        #[cfg(feature = "tunic")]
        &nxgame_tunic::TUNIC,
        #[cfg(feature = "lm3")]
        &nxgame_lm3::LM3,
        #[cfg(feature = "parabox")]
        &nxgame_parabox::PARABOX,
        #[cfg(feature = "stardew")]
        &nxgame_stardew::STARDEW,
        #[cfg(feature = "xc2")]
        &nxgame_xc2::XC2,
        #[cfg(feature = "sor4")]
        &nxgame_sor4::SOR4,
        #[cfg(feature = "pmttyd")]
        &nxgame_pmttyd::PMTTYD,
        #[cfg(feature = "rotk14")]
        &nxgame_rotk14::ROTK14,
        #[cfg(feature = "totk")]
        &nxgame_totk::TOTK,
        #[cfg(feature = "pkza")]
        &nxgame_pkza::PKZA,
        #[cfg(feature = "bmta")]
        &nxgame_bmta::BMTA,
        #[cfg(feature = "xcde")]
        &nxgame_xcde::XCDE,
    ];
    Catalog::new(families, titles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_builds() {
        let c = catalog().unwrap();
        // every title names the family `agl` or none
        assert!(c.titles().iter().all(|t| t.family().is_none_or(|f| f == "agl")));
        let agl = c.titles().iter().filter(|t| t.family() == Some("agl")).count();
        assert_eq!(c.missing_families().len(), if cfg!(feature = "agl") { 0 } else { agl });
    }

    #[test]
    fn every_title_builds_standalone() {
        use nxtitle::TitleFacts;
        let c = catalog().unwrap();
        for t in c.titles() {
            let r = c.resolve(&TitleFacts::new(t.id()));
            let st = r.standalone();
            assert!(st.is_some() && r.title_rules().is_some(), "{}", t.name());
            assert!(r.nxkit_services().contains(&nxtitle::Service::Kernel), "{}", t.name());
            assert!(t.profile().is_some(), "{}", t.name());
        }
    }

    #[cfg(all(feature = "agl", feature = "oot"))]
    #[test]
    fn oot_resolves_to_agl() {
        use nxtitle::{FamilyFrom, TitleFacts, TitleId};
        let c = catalog().unwrap();
        let r = c.resolve(&TitleFacts::new(TitleId(0x0400_4860_24DE_C000)));
        assert_eq!(
            r.title.map(|t| t.name()),
            Some("The Legend of Zelda: Ocarina of Time")
        );
        assert_eq!(r.family.map(|f| f.id()), Some("agl"));
        assert_eq!(r.family_from, FamilyFrom::Declared);
        assert_eq!(r.needs().len(), 6);
    }

    #[cfg(all(feature = "agl", feature = "botw"))]
    #[test]
    fn botw_resolves_to_agl_without_the_oot_keys() {
        use nxtitle::{FamilyFrom, TitleFacts, TitleId};
        let c = catalog().unwrap();
        let r = c.resolve(&TitleFacts::new(TitleId(0x0100_7EF0_0011_E000)));
        assert_eq!(r.family.map(|f| f.id()), Some("agl"));
        assert_eq!(r.family_from, FamilyFrom::Declared);
        let keys: Vec<_> = r.needs().iter().map(|n| n.value.capability).collect();
        assert!(!keys.contains(&"render_cond") && !keys.contains(&"tex_flush_upload"));
        assert_eq!(keys.len(), 5);
        assert_eq!(r.nxkit_services(), nxtitle::Service::ALL);
        assert!(r.standalone().is_some());
    }

    #[cfg(all(feature = "agl", feature = "smbw"))]
    #[test]
    fn smbw_resolves_to_agl_and_builds_standalone() {
        use nxtitle::{FamilyFrom, TitleFacts, TitleId};
        let c = catalog().unwrap();
        let r = c.resolve(&TitleFacts::new(TitleId(0x0100_1510_0B51_4000)));
        assert_eq!(r.title.map(|t| t.name()), Some("Super Mario Bros. Wonder"));
        assert_eq!(r.family.map(|f| f.id()), Some("agl"));
        assert_eq!(r.family_from, FamilyFrom::Declared);
        assert!(r.needs().is_empty());
        assert_eq!(r.nxkit_services(), nxtitle::Service::ALL);
        assert!(r.standalone().is_some() && r.title_rules().is_some());
    }
}
