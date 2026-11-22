use super::*;

#[test]
fn every_default_is_a_value_and_every_name_is_once() {
    for (i, o) in OPTIONS.iter().enumerate() {
        assert!(o.accepts(o.default), "{}: default {:?}", o.env, o.default);
        assert!(o.env.starts_with(crate::env::PREFIX), "{}", o.env);
        assert!(
            ["Game", "Controls", "Performance"].contains(&o.section),
            "{}",
            o.env
        );
        assert!(
            !OPTIONS[..i].iter().any(|p| p.env == o.env),
            "{} twice",
            o.env
        );
    }
}

#[test]
fn a_value_is_checked_as_the_launcher_checks_it() {
    let op = option(OP_MODE).unwrap();
    assert!(op.accepts("handheld") && op.accepts("docked") && !op.accepts("console"));
    let up = option(PRESENT_UPSCALE).unwrap();
    assert!(up.accepts("off") && up.accepts("metalfx") && !up.accepts("1"));
    let full = option(FULLSCREEN).unwrap();
    assert!(full.accepts("1") && full.accepts("0") && !full.accepts("yes"));
    let r = Opt {
        kind: Kind::Range {
            min: 0.5,
            max: 2.0,
            step: 0.25,
        },
        ..*op
    };
    assert!(r.accepts("1") && r.accepts("2.0") && r.accepts("0.5"));
    for bad in ["", " 1", "0x1", "NaN", "inf", "2.5", "0.25", "1 "] {
        assert!(!r.accepts(bad), "{bad:?}");
    }
    assert_eq!(saved_key(OP_MODE), "launcher.NXKIT_OP_MODE");
    let lang = option(LANGUAGE).unwrap();
    assert!(
        lang.accepts("") && lang.accepts("de") && lang.accepts("zh-Hant") && !lang.accepts("de-DE")
    );
    assert_eq!(
        (op_mode_value("console"), op_mode_value("handheld")),
        ("docked", "handheld")
    );
}

#[test]
fn the_languages_follow_the_nacp_bits() {
    let de_en = game_languages(1 << 0 | 1 << 4);
    assert_eq!(
        de_en.iter().map(|l| l.0).collect::<Vec<_>>(),
        ["", "en-US", "de"]
    );
    assert_eq!(game_languages(0).len(), LANGUAGES.len());
    assert_eq!(game_languages(1 << 15).last().map(|l| l.0), Some("pt-BR"));
}

#[test]
fn the_frame_rates_follow_the_title() {
    let values = |t, max| {
        game_fps_targets(t, max)
            .iter()
            .map(|r| r.0)
            .collect::<Vec<_>>()
    };
    assert_eq!(values(60, 60), ["30", "60"]);
    assert_eq!(values(30, 120), ["30", "60", "120"]);
    assert_eq!(values(120, 60), ["30", "60", "120"], "the title's own rate");
    assert_eq!(values(30, 30), ["30"]);
    assert!(values(0, 120).is_empty(), "no target: the game's own limit");
}

#[test]
fn a_sub_setting_names_a_row_and_its_value() {
    for o in OPTIONS {
        if let Some((env, value)) = o.needs {
            assert!(
                option(env).is_some_and(|p| p.accepts(value)),
                "{}: needs {env}={value}",
                o.env
            );
        }
    }
}
