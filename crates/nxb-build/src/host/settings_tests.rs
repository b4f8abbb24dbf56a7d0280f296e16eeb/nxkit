//! Tests of the profile header and the app binary settings: a synthetic engine, and the oracle over the reference
//! scripts' outputs for every profile (`NXB_ORACLE_ENGINE`, `NXB_ORACLE_SETTINGS` = a manifest written by running
//! `game.py header` and `settings.py`, rows `header|settings <gid> <platform> <gfxapi> <DM_LANG> <patched> <control>
//! <expected file or ->`).

use super::header::{header_one, upscaler_keys};
use super::profile::{Game, Profiles};
use super::settings::{languages, render};
use std::path::{Path, PathBuf};

const TSV: &str = "# test\nDM_FPS_TARGET\tchoice\t30=30 fps,60=60 fps\tDM_GAME_FPS_TARGET\tDM_GAME_FPS_TARGET!=0\tGame\tFrame rate\t\tlevel=basic
DM_RENDER_SCALE\trange\t0.5:1.0:0.05\t1.0\tfixups:RENDER_SCALE\tGame\tRender scale\t\tlevel=basic
DM_LANG\tchoice\tlanguages\tbuild:DM_LANG\tlanguages>1\tGame\tGame language\t\tlevel=basic
DM_FULLSCREEN\tchoice\t0=Window,1=Fullscreen\t0\tplatform:mac\tGame\tDisplay mode
DM_RUMBLE\ttoggle\t-\t1\talways\tControls\tGame rumble
DM_RUMBLE_GAIN\trange\t0:3:0.1\t1.0\talways\tControls\tRumble strength\t\tneeds=DM_RUMBLE=1
DM_PSO_MTL4\ttoggle\t-\tDM_GAME_PSO_MTL4|1\tgfxapi:nvn\tPerformance\tMetal 4 pipelines\tKeeps an archive.
DM_FPS_HOLD\ttoggle\ton/off\tDM_GAME_FPS_HOLD\tDM_GAME_FPS_TARGET!=0\tPerformance\tHold frames
";

const GAME: &str = "name\tA \"Game\"\ntitle_id\t0100000000001000\nprogram_id\t0100000000001000\nmodules\tmain sdk\nromfs_marker\tdata/x\nmem_reserve\t0x100000000\nmem_alias\t0x80000000\nmem_stack\t0x100000\napp_pool\t0xC0000000\nfps_target\t30\nrender_base\t1920x1080 1280x720\nbuild\tv0\t1.0.0\tmain=aa\tsdk=bb\n";

fn engine() -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nxb-settings-{}-{n}", std::process::id()));
    let g = root.join("dm/games/0100000000001000");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::write(root.join("dm/games/settings.tsv"), TSV).unwrap();
    std::fs::write(g.join("game.tsv"), GAME).unwrap();
    root
}

#[test]
fn synthetic_settings_and_header() {
    let e = engine();
    let p = Profiles::new(&e, &|_| None);
    let g = p.load("0100000000001000").unwrap();
    let h = header_one(&g, &p.here).unwrap();
    // both fixtures are the reference scripts' output for this synthetic engine (game.py header, settings.py)
    assert_eq!(h, include_str!("settings_test.h"));

    let fx = e.join("patched");
    std::fs::create_dir_all(&fx).unwrap();
    std::fs::write(fx.join("main.fixups.tsv"), "# RENDER_SCALE\t1\n").unwrap();
    let env = |k: &str| match k {
        "SETTINGS_PLATFORM" => Some("mac".to_string()),
        "GFXAPI" => Some("nvn".to_string()),
        _ => None,
    };
    let none = e.join("none.nacp");
    let json = render(&e, "0100000000001000", &fx, &none, &env).unwrap();
    let want = include_str!("settings_test.json");
    assert_eq!(json, want);
    // a broken table is an error, never a partial file
    std::fs::write(
        e.join("dm/games/settings.tsv"),
        "A\tchoice\tx=y\t0\talways\tNope\tL\n",
    )
    .unwrap();
    assert!(render(&e, "0100000000001000", &fx, &none, &env).is_err());
    std::fs::remove_dir_all(&e).unwrap();
}

#[test]
fn nacp_languages() {
    let d = std::env::temp_dir().join(format!("nxb-nacp-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut b = vec![0u8; 0x4000];
    b[0x302C..0x3030].copy_from_slice(&0b101u32.to_le_bytes());
    std::fs::write(d.join("control.nacp"), &b).unwrap();
    assert_eq!(languages(&d.join("control.nacp")), ["en-US", "ja"]);
    std::fs::write(d.join("short.nacp"), &b[..0x302F]).unwrap();
    assert!(languages(&d.join("short.nacp")).is_empty());
    assert!(languages(&d.join("missing.nacp")).is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn every_profile_equals_the_reference_scripts() {
    let (Some(engine), Some(manifest)) = (
        std::env::var_os("NXB_ORACLE_ENGINE"),
        std::env::var_os("NXB_ORACLE_SETTINGS"),
    ) else {
        return;
    };
    let engine = Path::new(&engine);
    let text = std::fs::read_to_string(&manifest).unwrap();
    let mut same = 0;
    for line in text.lines() {
        let c: Vec<&str> = line.split('\t').collect();
        let (kind, gid, plat, gfx, lang, patched, control, want) =
            (c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]);
        let p = Profiles::new(engine, &|_| None);
        if kind == "header" {
            let g = p.load(gid).unwrap();
            let got = super::header_multi::header(&g, &p);
            if want == "-" {
                assert!(got.is_err(), "{gid}: the reference refused the header");
            } else {
                assert_eq!(
                    got.unwrap(),
                    std::fs::read_to_string(want).unwrap(),
                    "{gid}"
                );
            }
        } else {
            let env = |k: &str| match k {
                "SETTINGS_PLATFORM" => Some(plat.to_string()),
                "GFXAPI" => Some(gfx.to_string()),
                "DM_LANG" => Some(lang.to_string()),
                "DM_SHOW_FPS" => Some("YES".to_string()),
                _ => None,
            };
            let got = render(engine, gid, Path::new(patched), Path::new(control), &env);
            if want == "-" {
                assert!(got.is_err(), "{gid} {plat}: the reference refused it");
            } else {
                assert_eq!(
                    got.unwrap(),
                    std::fs::read_to_string(want).unwrap(),
                    "{gid} {plat}"
                );
            }
        }
        same += 1;
    }
    eprintln!("{same} outputs identical");
    assert!(same > 0);
}

#[test]
fn language_choice_as_the_nsp_script() {
    use super::settings::{game_language, language_code};
    assert_eq!(language_code(" Japanese "), Some("ja"));
    assert_eq!(language_code("zh_hans"), Some("zh-Hans"));
    assert_eq!(language_code("english"), Some("en-US"));
    assert_eq!(language_code("zh-CN"), Some("zh-Hans"));
    assert_eq!(language_code("klingon"), None);
    assert_eq!(game_language("en-US", false, &[]).unwrap(), "en-US");
    assert_eq!(
        game_language("en-US", false, &["ja", "en-GB"]).unwrap(),
        "en-GB"
    );
    assert_eq!(game_language("en-US", false, &["ja"]).unwrap(), "ja");
    assert!(game_language("fr", true, &["ja"]).is_err());
}

#[test]
fn upscaler_keys_cover_the_three_ndsr_packs() {
    let e = engine();
    let g = Game::open(&e.join("dm/games/0100000000001000")).unwrap();
    assert!(upscaler_keys(&g).unwrap().is_empty());
    let row = |key: &str, stage: &str, pack: &str| {
        format!("x\tx\t{stage}\t{key}\tL/agl_resource.sarc/{pack}#NDSR@0x10\n")
    };
    let map = [
        row("00000000000000c3", "compute", "agl_ndsr_vr_light.pck"),
        row("00000000000000b2", "compute", "agl_ndsr_vr.pck"),
        row("00000000000000a1", "compute", "agl_ndsr.pck"),
        row("00000000000000a0", "pixel", "agl_ndsr.pck"),
        row("00000000000000d4", "compute", "other.pck"),
    ]
    .concat();
    std::fs::write(g.table("shader_map.tsv"), map).unwrap();
    assert_eq!(
        upscaler_keys(&g).unwrap(),
        ["00000000000000a1", "00000000000000b2", "00000000000000c3"]
    );
}
