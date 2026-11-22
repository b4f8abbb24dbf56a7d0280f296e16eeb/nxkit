//! The engine families, then the graphics families, each behind its feature.
//!
//! Written by `tools/gen_title_registry.py` from the folders; do not edit.

use nxtitle::Family;

/// The engine families, then the graphics families, each behind its feature.
pub(crate) fn all() -> Vec<&'static dyn Family> {
    vec![
        #[cfg(feature = "engine-4a")]
        &nxengine_4a::ENGINE,
        #[cfg(feature = "engine-agl")]
        &nxengine_agl::ENGINE,
        #[cfg(feature = "engine-arcadix")]
        &nxengine_arcadix::ENGINE,
        #[cfg(feature = "engine-atlus")]
        &nxengine_atlus::ENGINE,
        #[cfg(feature = "engine-baseengine")]
        &nxengine_baseengine::ENGINE,
        #[cfg(feature = "engine-basso")]
        &nxengine_basso::ENGINE,
        #[cfg(feature = "engine-bezel")]
        &nxengine_bezel::ENGINE,
        #[cfg(feature = "engine-blizzard")]
        &nxengine_blizzard::ENGINE,
        #[cfg(feature = "engine-blizzard-d2r")]
        &nxengine_blizzard_d2r::ENGINE,
        #[cfg(feature = "engine-camelot")]
        &nxengine_camelot::ENGINE,
        #[cfg(feature = "engine-clover")]
        &nxengine_clover::ENGINE,
        #[cfg(feature = "engine-criterion")]
        &nxengine_criterion::ENGINE,
        #[cfg(feature = "engine-digital-eclipse")]
        &nxengine_digital_eclipse::ENGINE,
        #[cfg(feature = "engine-ea")]
        &nxengine_ea::ENGINE,
        #[cfg(feature = "engine-falcom")]
        &nxengine_falcom::ENGINE,
        #[cfg(feature = "engine-firaxis")]
        &nxengine_firaxis::ENGINE,
        #[cfg(feature = "engine-fna")]
        &nxengine_fna::ENGINE,
        #[cfg(feature = "engine-fromsoftware")]
        &nxengine_fromsoftware::ENGINE,
        #[cfg(feature = "engine-frostbite")]
        &nxengine_frostbite::ENGINE,
        #[cfg(feature = "engine-fusion")]
        &nxengine_fusion::ENGINE,
        #[cfg(feature = "engine-fuzz")]
        &nxengine_fuzz::ENGINE,
        #[cfg(feature = "engine-gamefreak")]
        &nxengine_gamefreak::ENGINE,
        #[cfg(feature = "engine-gamemaker")]
        &nxengine_gamemaker::ENGINE,
        #[cfg(feature = "engine-gfd")]
        &nxengine_gfd::ENGINE,
        #[cfg(feature = "engine-giants")]
        &nxengine_giants::ENGINE,
        #[cfg(feature = "engine-grezzo")]
        &nxengine_grezzo::ENGINE,
        #[cfg(feature = "engine-hagi")]
        &nxengine_hagi::ENGINE,
        #[cfg(feature = "engine-hal")]
        &nxengine_hal::ENGINE,
        #[cfg(feature = "engine-heaps")]
        &nxengine_heaps::ENGINE,
        #[cfg(feature = "engine-hedgehog")]
        &nxengine_hedgehog::ENGINE,
        #[cfg(feature = "engine-idtech4")]
        &nxengine_idtech4::ENGINE,
        #[cfg(feature = "engine-inti")]
        &nxengine_inti::ENGINE,
        #[cfg(feature = "engine-intsys")]
        &nxengine_intsys::ENGINE,
        #[cfg(feature = "engine-ktgl")]
        &nxengine_ktgl::ENGINE,
        #[cfg(feature = "engine-larian")]
        &nxengine_larian::ENGINE,
        #[cfg(feature = "engine-love")]
        &nxengine_love::ENGINE,
        #[cfg(feature = "engine-m2")]
        &nxengine_m2::ENGINE,
        #[cfg(feature = "engine-mercurysteam")]
        &nxengine_mercurysteam::ENGINE,
        #[cfg(feature = "engine-monogame")]
        &nxengine_monogame::ENGINE,
        #[cfg(feature = "engine-monolith")]
        &nxengine_monolith::ENGINE,
        #[cfg(feature = "engine-mt-framework")]
        &nxengine_mt_framework::ENGINE,
        #[cfg(feature = "engine-mvgl")]
        &nxengine_mvgl::ENGINE,
        #[cfg(feature = "engine-next-level")]
        &nxengine_next_level::ENGINE,
        #[cfg(feature = "engine-nicalis")]
        &nxengine_nicalis::ENGINE,
        #[cfg(feature = "engine-nintendo-2d")]
        &nxengine_nintendo_2d::ENGINE,
        #[cfg(feature = "engine-nintendo-wii")]
        &nxengine_nintendo_wii::ENGINE,
        #[cfg(feature = "engine-onyx")]
        &nxengine_onyx::ENGINE,
        #[cfg(feature = "engine-phx")]
        &nxengine_phx::ENGINE,
        #[cfg(feature = "engine-phyre")]
        &nxengine_phyre::ENGINE,
        #[cfg(feature = "engine-platinum")]
        &nxengine_platinum::ENGINE,
        #[cfg(feature = "engine-rage")]
        &nxengine_rage::ENGINE,
        #[cfg(feature = "engine-re-engine")]
        &nxengine_re_engine::ENGINE,
        #[cfg(feature = "engine-redengine")]
        &nxengine_redengine::ENGINE,
        #[cfg(feature = "engine-renderdragon")]
        &nxengine_renderdragon::ENGINE,
        #[cfg(feature = "engine-retro")]
        &nxengine_retro::ENGINE,
        #[cfg(feature = "engine-shinen")]
        &nxengine_shinen::ENGINE,
        #[cfg(feature = "engine-snes-online")]
        &nxengine_snes_online::ENGINE,
        #[cfg(feature = "engine-snowdrop")]
        &nxengine_snowdrop::ENGINE,
        #[cfg(feature = "engine-sora")]
        &nxengine_sora::ENGINE,
        #[cfg(feature = "engine-source")]
        &nxengine_source::ENGINE,
        #[cfg(feature = "engine-square-enix")]
        &nxengine_square_enix::ENGINE,
        #[cfg(feature = "engine-steamworld")]
        &nxengine_steamworld::ENGINE,
        #[cfg(feature = "engine-sumo")]
        &nxengine_sumo::ENGINE,
        #[cfg(feature = "engine-supergiant")]
        &nxengine_supergiant::ENGINE,
        #[cfg(feature = "engine-team-bondi")]
        &nxengine_team_bondi::ENGINE,
        #[cfg(feature = "engine-telltale")]
        &nxengine_telltale::ENGINE,
        #[cfg(feature = "engine-the-forge")]
        &nxengine_the_forge::ENGINE,
        #[cfg(feature = "engine-traveller")]
        &nxengine_traveller::ENGINE,
        #[cfg(feature = "engine-trinity")]
        &nxengine_trinity::ENGINE,
        #[cfg(feature = "engine-typemoon")]
        &nxengine_typemoon::ENGINE,
        #[cfg(feature = "engine-ubiart")]
        &nxengine_ubiart::ENGINE,
        #[cfg(feature = "engine-unity")]
        &nxengine_unity::ENGINE,
        #[cfg(feature = "engine-unreal")]
        &nxengine_unreal::ENGINE,
        #[cfg(feature = "engine-unreal3")]
        &nxengine_unreal3::ENGINE,
        #[cfg(feature = "engine-vanillaware")]
        &nxengine_vanillaware::ENGINE,
        #[cfg(feature = "engine-vrender")]
        &nxengine_vrender::ENGINE,
        #[cfg(feature = "engine-wube")]
        &nxengine_wube::ENGINE,
        #[cfg(feature = "graphics-nvn")]
        &nxgraphics_nvn::GRAPHICS,
        #[cfg(feature = "graphics-opengl")]
        &nxgraphics_opengl::GRAPHICS,
        #[cfg(feature = "graphics-vulkan")]
        &nxgraphics_vulkan::GRAPHICS,
    ]
}
