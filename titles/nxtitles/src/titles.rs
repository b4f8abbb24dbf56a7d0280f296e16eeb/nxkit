//! The title modules, each behind its feature.
//!
//! `tools/gen_title_registry.py` writes this file from the folders. Do not edit it.

use nxtitle::Title;

/// The title modules, each behind its feature.
pub(crate) fn all() -> Vec<&'static dyn Title> {
    vec![
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
        #[cfg(feature = "hades")]
        &nxgame_hades::HADES,
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
        #[cfg(feature = "yoshimb")]
        &nxgame_yoshimb::YOSHIMB,
        #[cfg(feature = "splatraiders")]
        &nxgame_splatraiders::SPLATRAIDERS,
        #[cfg(feature = "hwaoi")]
        &nxgame_hwaoi::HWAOI,
        #[cfg(feature = "dkbananza")]
        &nxgame_dkbananza::DKBANANZA,
        #[cfg(feature = "kirbyar")]
        &nxgame_kirbyar::KIRBYAR,
        #[cfg(feature = "fefw")]
        &nxgame_fefw::FEFW,
        #[cfg(feature = "nssr")]
        &nxgame_nssr::NSSR,
        #[cfg(feature = "pkpokopia")]
        &nxgame_pkpokopia::PKPOKOPIA,
        #[cfg(feature = "mkw")]
        &nxgame_mkw::MKW,
        #[cfg(feature = "pragmata")]
        &nxgame_pragmata::PRAGMATA,
        #[cfg(feature = "starfox")]
        &nxgame_starfox::STARFOX,
        #[cfg(feature = "digimoncs")]
        &nxgame_digimoncs::DIGIMONCS,
        #[cfg(feature = "fifa23")]
        &nxgame_fifa23::FIFA23,
        #[cfg(feature = "welldweller")]
        &nxgame_welldweller::WELLDWELLER,
        #[cfg(feature = "slaythespire")]
        &nxgame_slaythespire::SLAYTHESPIRE,
        #[cfg(feature = "dos2")]
        &nxgame_dos2::DOS2,
        #[cfg(feature = "doom3")]
        &nxgame_doom3::DOOM3,
        #[cfg(feature = "mmlc")]
        &nxgame_mmlc::MMLC,
        #[cfg(feature = "factorio")]
        &nxgame_factorio::FACTORIO,
        #[cfg(feature = "legosw")]
        &nxgame_legosw::LEGOSW,
        #[cfg(feature = "civ6")]
        &nxgame_civ6::CIV6,
        #[cfg(feature = "xcx")]
        &nxgame_xcx::XCX,
        #[cfg(feature = "bscotm2")]
        &nxgame_bscotm2::BSCOTM2,
        #[cfg(feature = "bscotm")]
        &nxgame_bscotm::BSCOTM,
        #[cfg(feature = "re1")]
        &nxgame_re1::RE1,
        #[cfg(feature = "dbzk")]
        &nxgame_dbzk::DBZK,
        #[cfg(feature = "tsukihime")]
        &nxgame_tsukihime::TSUKIHIME,
        #[cfg(feature = "raymanl")]
        &nxgame_raymanl::RAYMANL,
        #[cfg(feature = "borderlands")]
        &nxgame_borderlands::BORDERLANDS,
        #[cfg(feature = "fateha")]
        &nxgame_fateha::FATEHA,
        #[cfg(feature = "cozyland")]
        &nxgame_cozyland::COZYLAND,
        #[cfg(feature = "trailssky2")]
        &nxgame_trailssky2::TRAILSSKY2,
        #[cfg(feature = "p5s")]
        &nxgame_p5s::P5S,
        #[cfg(feature = "lanoire")]
        &nxgame_lanoire::LANOIRE,
        #[cfg(feature = "p5t")]
        &nxgame_p5t::P5T,
        #[cfg(feature = "ff8")]
        &nxgame_ff8::FF8,
        #[cfg(feature = "snesnc")]
        &nxgame_snesnc::SNESNC,
        #[cfg(feature = "spfbw")]
        &nxgame_spfbw::SPFBW,
        #[cfg(feature = "unmetal")]
        &nxgame_unmetal::UNMETAL,
        #[cfg(feature = "spsot")]
        &nxgame_spsot::SPSOT,
        #[cfg(feature = "smg")]
        &nxgame_smg::SMG,
        #[cfg(feature = "witcher3")]
        &nxgame_witcher3::WITCHER3,
        #[cfg(feature = "kirbydb")]
        &nxgame_kirbydb::KIRBYDB,
        #[cfg(feature = "ff6")]
        &nxgame_ff6::FF6,
        #[cfg(feature = "ff5")]
        &nxgame_ff5::FF5,
        #[cfg(feature = "bsrotn")]
        &nxgame_bsrotn::BSROTN,
        #[cfg(feature = "civ7")]
        &nxgame_civ7::CIV7,
        #[cfg(feature = "chainedechoes")]
        &nxgame_chainedechoes::CHAINEDECHOES,
        #[cfg(feature = "mgsr")]
        &nxgame_mgsr::MGSR,
        #[cfg(feature = "whsurvivors")]
        &nxgame_whsurvivors::WHSURVIVORS,
        #[cfg(feature = "twelvemin")]
        &nxgame_twelvemin::TWELVEMIN,
        #[cfg(feature = "balatro")]
        &nxgame_balatro::BALATRO,
        #[cfg(feature = "fs26")]
        &nxgame_fs26::FS26,
        #[cfg(feature = "metro2033")]
        &nxgame_metro2033::METRO2033,
        #[cfg(feature = "sammax3")]
        &nxgame_sammax3::SAMMAX3,
        #[cfg(feature = "minecraft")]
        &nxgame_minecraft::MINECRAFT,
        #[cfg(feature = "rezero")]
        &nxgame_rezero::REZERO,
        #[cfg(feature = "burnout")]
        &nxgame_burnout::BURNOUT,
        #[cfg(feature = "ff12")]
        &nxgame_ff12::FF12,
        #[cfg(feature = "overcooked")]
        &nxgame_overcooked::OVERCOOKED,
        #[cfg(feature = "smg2")]
        &nxgame_smg2::SMG2,
        #[cfg(feature = "smbw2")]
        &nxgame_smbw2::SMBW2,
        #[cfg(feature = "orbitals")]
        &nxgame_orbitals::ORBITALS,
        #[cfg(feature = "fastfusion")]
        &nxgame_fastfusion::FASTFUSION,
        #[cfg(feature = "re4")]
        &nxgame_re4::RE4,
        #[cfg(feature = "welcometour")]
        &nxgame_welcometour::WELCOMETOUR,
        #[cfg(feature = "sonicfd")]
        &nxgame_sonicfd::SONICFD,
    ]
}
