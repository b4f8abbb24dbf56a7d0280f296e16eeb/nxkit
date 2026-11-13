//! Synthetic metadata selection regressions.
use super::*;

use crate::{extract_fixture as common, nup_fixture as nup};

const APP: u64 = 0x0100_ABCD_0000_0000;

fn package(path: &str, role: GenericRole, metas: &[(&str, u64, u32)]) -> GenericPackage {
    (
        PathBuf::from(path),
        role,
        metas
            .iter()
            .map(|(k, t, v)| (k.to_string(), *t, *v))
            .collect(),
    )
}

fn base() -> GenericPackage {
    package(
        "misleading [0100999900000000][v999].nsp",
        GenericRole::Input,
        &[("Application", APP, 0)],
    )
}

#[test]
fn external_nup_dlc_records_validate_and_select_versions() {
    let dir = common::tmp("generic-nup-dlc-records");
    let path = dir.join("dlc.nup");
    let record = |title, version| nup::Pkg {
        title,
        version,
        kind: 2,
        flags: 0,
        exefs: None,
        romfs: Some(vec![nup::Node::File("data", b"DLC".to_vec(), false)]),
        meta: vec![],
    };
    std::fs::write(
        &path,
        nup::build(&[record(APP + 0x1001, 1), record(APP + 0x1001, 3)], false),
    )
    .unwrap();
    let args = Args {
        aoc: vec![path.clone()],
        ..Args::default()
    };
    let mut packages = vec![base()];
    packages.extend(external_dlc(&args, None).unwrap());
    let inputs = select_generic(packages, None).unwrap();
    assert_eq!(inputs.dlc, vec![(APP + 0x1001, 3, path.clone())]);
    std::fs::write(&path, nup::build(&[record(APP + 0x3001, 1)], false)).unwrap();
    assert!(merge_nup_dlc(APP, vec![], external_dlc(&args, None).unwrap()).is_err());
    let mut no_dlc = record(APP, 0);
    no_dlc.kind = 0;
    std::fs::write(&path, nup::build(&[no_dlc], false)).unwrap();
    assert!(external_dlc(&args, None).is_err());
    common::cleanup(&dir);
}

#[test]
fn nup_external_dlc_selects_latest_and_deterministic_ties() {
    let embedded = vec![(APP + 0x1001, 3, PathBuf::from("base.nup"))];
    let external = vec![
        package(
            "z.nsz",
            GenericRole::Dlc,
            &[("AddOnContent", APP + 0x1001, 4)],
        ),
        package(
            "a.nsp",
            GenericRole::Dlc,
            &[("AddOnContent", APP + 0x1001, 4)],
        ),
        package(
            "second.xcz",
            GenericRole::Dlc,
            &[("AddOnContent", APP + 0x1002, 1)],
        ),
    ];
    let first = merge_nup_dlc(APP, embedded.clone(), external.clone()).unwrap();
    let mut reversed = external;
    reversed.reverse();
    assert_eq!(first, merge_nup_dlc(APP, embedded, reversed).unwrap());
    assert_eq!(
        first,
        vec![
            (APP + 0x1001, 4, PathBuf::from("a.nsp")),
            (APP + 0x1002, 1, PathBuf::from("second.xcz")),
        ]
    );
    assert_eq!(
        merge_nup_dlc(
            APP,
            vec![(APP + 0x1001, 5, PathBuf::from("base.nup"))],
            vec![package(
                "old.nsp",
                GenericRole::Dlc,
                &[("AddOnContent", APP + 0x1001, 4)]
            ),]
        )
        .unwrap(),
        vec![(APP + 0x1001, 5, PathBuf::from("base.nup"))]
    );
}

#[test]
fn nup_external_dlc_requires_matching_family_and_kind() {
    for metas in [
        vec![],
        vec![("Patch", APP + 0x800, 1)],
        vec![("AddOnContent", APP + 0x3001, 1)],
        vec![
            ("AddOnContent", APP + 0x1001, 1),
            ("Application", APP + 0x2000, 0),
        ],
        vec![("AddOnContent", APP + 0x1000 + 2001, 1)],
    ] {
        assert!(
            merge_nup_dlc(
                APP,
                vec![],
                vec![package("extra.nsp", GenericRole::Dlc, &metas)]
            )
            .is_err()
        );
    }
}

#[test]
fn strict_membership_rejects_fallback_and_ambiguity() {
    use nxb_extract::meta::Cnmt;
    use nxb_extract::nca::ContentType::{Data, Program, PublicData};
    let application = Cnmt {
        kind: 0x80,
        title: APP,
        version: 2,
        contents: vec!["listed".into()],
    };
    assert!(checked_metadata(vec![application.clone()], &[("renamed", Program, APP)]).is_err());
    assert!(checked_metadata(vec![application.clone()], &[("listed", Program, APP + 1)]).is_err());
    assert!(
        checked_metadata(
            vec![application.clone()],
            &[("listed", Program, APP), ("listed", Program, APP)]
        )
        .is_err()
    );
    assert!(
        checked_metadata(
            vec![application.clone(), application.clone()],
            &[("listed", Program, APP)]
        )
        .is_err()
    );
    assert_eq!(
        checked_metadata(vec![application], &[("listed", Program, APP)]).unwrap(),
        vec![("Application".into(), APP, 2)]
    );
    let dlc = Cnmt {
        kind: 0x82,
        title: APP + 0x1001,
        version: 3,
        contents: vec!["listed".into()],
    };
    assert!(checked_metadata(vec![dlc.clone()], &[("unlisted", PublicData, dlc.title)]).is_err());
    assert!(
        checked_metadata(
            vec![dlc.clone()],
            &[
                ("listed", PublicData, dlc.title),
                ("unlisted", Data, dlc.title)
            ]
        )
        .is_err()
    );
    assert!(checked_metadata(vec![dlc.clone()], &[("listed", Data, dlc.title)]).is_ok());
}

#[test]
fn generic_base_version_comes_from_selected_metadata() {
    let inputs = select_generic(
        vec![
            package(
                "a-base [v0].nsp",
                GenericRole::Input,
                &[("Application", APP, 65536)],
            ),
            package(
                "z-base [v999].nsp",
                GenericRole::Input,
                &[("Application", APP, 131072)],
            ),
        ],
        None,
    )
    .unwrap();
    assert_eq!(inputs.base, PathBuf::from("a-base [v0].nsp"));
    assert_eq!(inputs.base_version, 65536);
    assert!(inputs.updates.is_empty());
}

#[test]
fn generic_selection_uses_metadata_and_latest_versions() {
    let inputs = select_generic(
        vec![
            base(),
            package(
                "old [v999].nsz",
                GenericRole::Update,
                &[("Patch", APP + 0x800, 1)],
            ),
            package(
                "new [v0].xcz",
                GenericRole::Update,
                &[("Patch", APP + 0x800, 3)],
            ),
            package(
                "dlc-old.nsp",
                GenericRole::Dlc,
                &[("AddOnContent", APP + 0x1001, 5)],
            ),
            package(
                "dlc-new.nsz",
                GenericRole::Dlc,
                &[("AddOnContent", APP + 0x1001, 7)],
            ),
        ],
        None,
    )
    .unwrap();
    assert_eq!(inputs.app, APP);
    assert_eq!(inputs.updates, vec![(3, PathBuf::from("new [v0].xcz"))]);
    assert_eq!(
        inputs.dlc,
        vec![(APP + 0x1001, 7, PathBuf::from("dlc-new.nsz"))]
    );
    assert_eq!(inputs.app_flag, Some(APP));
}

#[test]
fn generic_selection_rejects_wrong_explicit_families_and_kinds() {
    for (role, kind, title) in [
        (GenericRole::Update, "Patch", APP + 0x2800),
        (GenericRole::Dlc, "AddOnContent", APP + 0x3001),
        (GenericRole::Update, "Application", APP),
        (GenericRole::Dlc, "Patch", APP + 0x800),
    ] {
        assert!(
            select_generic(
                vec![base(), package("extra.nsp", role, &[(kind, title, 1)])],
                Some(APP)
            )
            .is_err()
        );
    }
}

#[test]
fn generic_selection_requires_base_and_explicit_choice() {
    assert!(
        select_generic(
            vec![package(
                "patch.nsp",
                GenericRole::Input,
                &[("Patch", APP + 0x800, 1)]
            )],
            None
        )
        .is_err()
    );
    let packages = vec![
        base(),
        package(
            "other.nsp",
            GenericRole::Input,
            &[("Application", APP + 0x2000, 0)],
        ),
    ];
    assert!(select_generic(packages.clone(), None).is_err());
    assert_eq!(select_generic(packages, Some(APP)).unwrap().app, APP);
    assert!(select_generic(vec![base()], Some(APP + 0x2000)).is_err());
}

#[test]
fn generic_dlc_indices_are_bounded() {
    for index in [1, 2000] {
        assert_eq!(
            generic_family("AddOnContent", APP + 0x1000 + index),
            Ok(APP)
        );
    }
    for index in [0, 2001, 4095] {
        assert!(generic_family("AddOnContent", APP + 0x1000 + index).is_err());
    }
    assert!(generic_family("AddOnContent", 1).is_err());
    assert!(generic_family("Application", APP + 1).is_err());
    assert!(generic_family("Patch", APP).is_err());
}

#[test]
fn generic_equal_versions_select_deterministically() {
    let mut packages = vec![
        base(),
        package(
            "z.nsz",
            GenericRole::Input,
            &[("Patch", APP + 0x800, 9), ("AddOnContent", APP + 0x1001, 2)],
        ),
        package(
            "a.nsp",
            GenericRole::Input,
            &[("Patch", APP + 0x800, 9), ("AddOnContent", APP + 0x1001, 2)],
        ),
    ];
    let first = select_generic(packages.clone(), None).unwrap();
    packages.reverse();
    let second = select_generic(packages, None).unwrap();
    assert_eq!(first.updates, second.updates);
    assert_eq!(first.dlc, second.dlc);
    assert_eq!(first.updates[0].1, PathBuf::from("a.nsp"));
    assert_eq!(first.dlc[0].2, PathBuf::from("a.nsp"));
}

#[test]
fn generic_merged_package_and_missing_metadata() {
    let inputs = select_generic(
        vec![package(
            "merged.xci",
            GenericRole::Input,
            &[
                ("Application", APP, 0),
                ("Patch", APP + 0x800, 12),
                ("AddOnContent", APP + 0x1001, 4),
            ],
        )],
        None,
    )
    .unwrap();
    assert_eq!(inputs.updates[0].0, 12);
    assert_eq!(inputs.dlc[0].1, 4);
    assert!(
        select_generic(
            vec![package(
                "tagged [0100ABCD00000000].nsp",
                GenericRole::Input,
                &[]
            )],
            None
        )
        .is_err()
    );
}

#[test]
fn standalone_updates_select_latest_and_preserve_all_dlc() {
    let mut original = select_generic(vec![base()], None).unwrap();
    original.dlc = vec![(APP + 0x1001, 1, original.base.clone())];
    let mut older = original.clone();
    older.source = Source::Nup;
    older.base = PathBuf::from("old.nup");
    older.updates = vec![(2, older.base.clone())];
    older.dlc = vec![(APP + 0x1002, 1, older.base.clone())];
    let mut newer = older.clone();
    newer.base = PathBuf::from("new.nup");
    newer.updates = vec![(3, newer.base.clone())];
    newer.dlc = vec![(APP + 0x1001, 2, newer.base.clone())];
    let first =
        select_standalone_updates(original.clone(), vec![older.clone(), newer.clone()]).unwrap();
    let second = select_standalone_updates(original, vec![newer, older]).unwrap();
    assert_eq!(first.base, PathBuf::from("new.nup"));
    assert_eq!(first.source, Source::Nup);
    assert_eq!(first.updates, second.updates);
    assert_eq!(first.dlc, second.dlc);
    assert_eq!(
        first.dlc,
        vec![
            (APP + 0x1001, 2, PathBuf::from("new.nup")),
            (APP + 0x1002, 1, PathBuf::from("old.nup")),
        ]
    );
}

#[test]
fn standalone_update_ties_are_deterministic_and_wrong_app_fails() {
    let mut original = select_generic(vec![base()], None).unwrap();
    original.updates = vec![(4, PathBuf::from("z.nsp"))];
    let mut candidate = original.clone();
    candidate.source = Source::Nup;
    candidate.base = PathBuf::from("a.nup");
    candidate.updates = vec![(4, candidate.base.clone())];
    assert_eq!(
        select_standalone_updates(original.clone(), vec![candidate.clone()])
            .unwrap()
            .base,
        candidate.base
    );
    candidate.updates[0].0 = 3;
    assert_eq!(
        select_standalone_updates(original.clone(), vec![candidate.clone()])
            .unwrap()
            .base,
        original.base
    );
    candidate.app += 0x2000;
    assert!(select_standalone_updates(original, vec![candidate]).is_err());
}

#[test]
fn nup_external_package_updates_select_only_newer_and_merge_dlc() {
    let mut nup = select_generic(vec![base()], None).unwrap();
    nup.source = Source::Nup;
    nup.base = PathBuf::from("base.nup");
    nup.updates = vec![(2, nup.base.clone())];
    let equal = package("a.nsp", GenericRole::Update, &[("Patch", APP + 0x800, 2)]);
    assert_eq!(
        merge_package_updates(nup.clone(), vec![equal])
            .unwrap()
            .updates,
        nup.updates
    );
    let newer = package(
        "z.nsz",
        GenericRole::Update,
        &[("Patch", APP + 0x800, 3), ("AddOnContent", APP + 0x1001, 1)],
    );
    let tie = package("a.nsp", GenericRole::Update, &[("Patch", APP + 0x800, 3)]);
    let first = merge_package_updates(nup.clone(), vec![newer.clone(), tie.clone()]).unwrap();
    let second = merge_package_updates(nup.clone(), vec![tie, newer]).unwrap();
    assert_eq!(first.source, Source::Nup);
    assert_eq!(first.base, nup.base);
    assert_eq!(first.updates, vec![(3, PathBuf::from("a.nsp"))]);
    assert_eq!(first.updates, second.updates);
    assert_eq!(first.dlc, vec![(APP + 0x1001, 1, PathBuf::from("z.nsz"))]);
    for metas in [
        vec![],
        vec![("Application", APP, 0)],
        vec![("Patch", APP + 0x2800, 4)],
    ] {
        assert!(
            merge_package_updates(
                nup.clone(),
                vec![package("bad.nsp", GenericRole::Update, &metas)]
            )
            .is_err()
        );
    }
}

#[test]
fn backing_reconstruction_checks_version_identity_and_preserves_dlc() {
    let mut original = select_generic(vec![base()], None).unwrap();
    original.source = Source::Nup;
    original.updates = vec![(3, PathBuf::from("base.nup"))];
    original.dlc = vec![(APP + 0x1001, 1, PathBuf::from("base.nup"))];
    let mut backing = select_generic(vec![base()], None).unwrap();
    backing.updates = vec![(3, PathBuf::from("patch.nsp"))];
    let result = validate_reconstruction(original.clone(), backing.clone()).unwrap();
    assert_eq!(result.source, Source::Packages);
    assert_eq!(result.dlc, original.dlc);
    backing.updates[0].0 = 2;
    assert!(validate_reconstruction(original.clone(), backing.clone()).is_err());
    backing.updates[0].0 = 3;
    backing.base_version = 1;
    assert!(validate_reconstruction(original.clone(), backing.clone()).is_err());
    original.has_base = false;
    assert!(validate_reconstruction(original.clone(), backing.clone()).is_ok());
    backing.app += 0x2000;
    assert!(validate_reconstruction(original, backing).is_err());
}

#[test]
fn explicit_merged_update_cannot_replace_original_application_metadata() {
    let inputs = select_generic(
        vec![
            package("z-base.nsp", GenericRole::Input, &[("Application", APP, 2)]),
            package(
                "a-update.nsp",
                GenericRole::Update,
                &[("Application", APP, 1), ("Patch", APP + 0x800, 3)],
            ),
        ],
        None,
    )
    .unwrap();
    assert_eq!(inputs.base, PathBuf::from("z-base.nsp"));
    assert_eq!(inputs.base_version, 2);
}

#[test]
fn nup_update_records_accept_application_or_patch_id_but_not_other_families() {
    let dir = common::tmp("generic-nup-update-identity");
    let path = dir.join("update.nup");
    for (title, program, valid) in [
        (APP, APP, true),
        (APP | 0x800, APP, true),
        (APP + 0x2000, APP, false),
        (APP, APP + 0x2000, false),
    ] {
        let update = nup::Pkg {
            title,
            kind: 1,
            version: 65536,
            flags: 1,
            exefs: Some(vec![nup::Node::File(
                "main.npdm",
                common::npdm(program),
                false,
            )]),
            romfs: Some(vec![nup::Node::File("data", b"update".to_vec(), false)]),
            meta: vec![],
        };
        std::fs::write(&path, nup::build(&[update], false)).unwrap();
        let args = Args {
            nsp_dir: path.clone(),
            title_id: Some(format!("{APP:016X}")),
            ..Args::default()
        };
        let result = discover_generic(&args);
        assert_eq!(
            result.is_ok(),
            valid,
            "record {title:016X}, NPDM {program:016X}: {result:?}"
        );
        if let Ok(input) = result {
            assert_eq!(input.app, APP);
            assert_eq!(input.updates, vec![(65536, path.clone())]);
            assert!(!input.has_base);
            assert!(generic_nup(&path, &Args::default(), Some(APP), true).is_ok());
        }
    }
    common::cleanup(&dir);
}
