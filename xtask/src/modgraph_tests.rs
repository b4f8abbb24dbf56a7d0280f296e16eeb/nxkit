use super::*;
use crate::modules::{compare, cycle_groups, parse_baseline};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Writes `files` (path below `src`, text) into a fresh folder and returns the `src` folder.
fn fake_crate(files: &[(&str, &str)]) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("xtask-modgraph-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (path, text) in files {
        let p = root.join("src").join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    root.join("src")
}

fn edges(g: &Graph) -> Vec<(String, String)> {
    g.edges.keys().cloned().collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.into(), b.into())
}

#[test]
fn edges_from_crate_paths_groups_reexports_and_super() {
    let src = fake_crate(&[
        (
            "lib.rs",
            "mod a;\nmod b;\nmod c;\nmod d;\npub use d::Thing;\n#[cfg(test)]\nmod t;\n",
        ),
        (
            "a.rs",
            "use crate::{b::X, c};\n// crate::d\nfn f() -> crate::Thing { todo!() }\n",
        ),
        ("b.rs", "mod inner;\n"),
        (
            "b/inner.rs",
            "use super::super::c::Y;\nconst S: &str = \"crate::a\";\n",
        ),
        (
            "c.rs",
            "#[cfg(test)]\nmod tests { use crate::a; }\n#[cfg(test)]\nuse crate::b;\n",
        ),
        ("d.rs", "#[path = \"d_extra.rs\"]\nmod extra;\n"),
        ("d_extra.rs", "pub(crate) fn g() { crate::a::f(); }\n"),
        ("t.rs", "use crate::a;\n"),
    ]);
    let g = graph(&src);
    assert_eq!(g.modules.len(), 4, "the test module t is left out");
    assert_eq!(
        edges(&g),
        [
            pair("a", "b"),
            pair("a", "c"),
            pair("a", "d"),
            pair("b", "c"),
            pair("d", "a")
        ]
    );
}

#[test]
fn test_files_are_left_out() {
    let src = fake_crate(&[
        ("lib.rs", "mod a;\nmod b;\nmod a_test;\n"),
        ("a.rs", "use crate::b;\n"),
        ("b.rs", "pub fn f() {}\n"),
        (
            "a_test.rs",
            "#![cfg(test)]\nuse crate::a;\nmod x { use crate::b; }\n",
        ),
    ]);
    let g = graph(&src);
    assert_eq!(edges(&g), [pair("a", "b")]);
    assert!(cycle_groups(&g).is_empty());
}

#[test]
fn a_new_cycle_fails_and_a_smaller_one_asks_to_shrink() {
    let src = fake_crate(&[
        ("lib.rs", "mod a;\nmod b;\nmod c;\n"),
        ("a.rs", "use crate::b;\n"),
        ("b.rs", "use crate::c;\n"),
        ("c.rs", "use crate::a;\n"),
    ]);
    let groups = cycle_groups(&graph(&src));
    assert_eq!(groups.len(), 1);
    let now = [("k".to_string(), groups)].into_iter().collect();
    assert!(compare(&now, &parse_baseline("k: a b c\n")).is_empty());
    let new = compare(&now, &parse_baseline("# none\n"));
    assert_eq!(new.len(), 1);
    assert!(
        new[0].contains("new or larger module cycle: k: a b c"),
        "{new:?}"
    );
    let grown = compare(&now, &parse_baseline("k: a b\n"));
    assert!(
        grown.iter().any(|e| e.contains("new or larger")),
        "{grown:?}"
    );
    let smaller = compare(&now, &parse_baseline("k: a b c d\n"));
    assert_eq!(smaller.len(), 1);
    assert!(smaller[0].contains("shrink") && smaller[0].contains("write k: a b c"));
    let gone = compare(&Default::default(), &parse_baseline("k: a b\n"));
    assert!(gone[0].contains("delete the line"), "{gone:?}");
}
