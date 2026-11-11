use super::*;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nxkit-carry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn list(entries: &[u64]) -> Seen {
    let mut t = format!("{}\n", super::super::record::HEADER);
    for k in entries {
        t.push_str(&format!("C\t{k:016x}\tlib{k}_b\tmain0\n"));
    }
    Seen::parse(&t).unwrap()
}

fn keys(s: &Seen) -> Vec<String> {
    s.entries().iter().map(|e| format!("{e:?}")).collect()
}

#[test]
fn the_carried_file_is_named_by_the_key_scheme() {
    let b = Path::new("/x/pipecache");
    assert_eq!(list_path(b, 1), b.join("seen-1.list"));
    assert_ne!(list_path(b, 1), list_path(b, 2));
}

#[test]
fn the_text_has_the_scheme_and_reads_back_the_same() {
    let s = list(&[1, 2, 3]);
    let t = text(&s, 7);
    assert!(t.starts_with(&format!("{}\n#scheme 7\n", super::super::record::HEADER)));
    assert_eq!(keys(&Seen::parse(&t).unwrap()), keys(&s));
}

#[test]
fn a_new_identity_starts_with_the_list_of_the_old_ones() {
    let base = tmp("rebuild");
    // two identity folders of earlier builds with overlapping lists, as the earlier host builds wrote them
    for (id, entries) in [("id-0001", &[1u64, 2][..]), ("id-0002", &[2, 3][..])] {
        let d = base.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(LEGACY_FILE), list(entries).to_text()).unwrap();
    }
    let got = load(&base, 1);
    assert_eq!(got.seen.len(), 3);
    assert_eq!(got.migrated, 3);
    // written to its file, the next start reads that file and the old folders add nothing
    std::fs::write(list_path(&base, 1), text(&got.seen, 1)).unwrap();
    let again = load(&base, 1);
    assert_eq!(again.seen.len(), 3);
    assert_eq!(again.migrated, 0);
    assert_eq!(keys(&again.seen), keys(&got.seen));
    // the folders of the old identities are gone (the start removes them), the list stays
    std::fs::remove_dir_all(base.join("id-0001")).unwrap();
    std::fs::remove_dir_all(base.join("id-0002")).unwrap();
    assert_eq!(load(&base, 1).seen.len(), 3);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn the_list_of_another_scheme_is_not_used() {
    let base = tmp("scheme");
    std::fs::write(list_path(&base, 2), text(&list(&[1, 2]), 2)).unwrap();
    assert_eq!(load(&base, 1).seen.len(), 0);
    assert_eq!(load(&base, 2).seen.len(), 2);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_file_of_another_version_or_a_missing_folder_gives_an_empty_list() {
    let base = tmp("foreign");
    std::fs::write(list_path(&base, 1), "nxkit-pipelines-seen 9\nC\t1\ta\tb\n").unwrap();
    assert!(load(&base, 1).seen.is_empty());
    assert!(load(&base.join("missing"), 1).seen.is_empty());
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn the_start_waits_for_everything_until_the_folder_says_it_made_the_list() {
    let d = tmp("done");
    let s = list(&[1]);
    assert!(needs_full_start(&d, &s));
    // nothing listed, nothing to wait for
    assert!(!needs_full_start(&d, &Seen::default()));
    std::fs::write(d.join(DONE_FILE), b"").unwrap();
    assert!(!needs_full_start(&d, &s));
    std::fs::remove_dir_all(&d).unwrap();
}
