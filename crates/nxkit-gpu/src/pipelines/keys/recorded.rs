//! Keys against a list that a host wrote before the key functions moved into `keys.rs` (commit 87bdfb3, inline
//! hashing): the key of each entry, computed again from its fields, must be the key that the list recorded.

use super::super::record::Seen;
use super::entry_keys;

/// Real lines of that list: three render pipelines (a layout, a blend state with a layout, a point topology that no
/// fallback made), two mesh pipelines (task and mesh with a fragment shader and no colour, mesh with a colour) and two
/// compute pipelines. They hold shader key hashes and Metal state names.
const LINES: &[&str] = &[
    "R\t4fa3df93569be8e4\tc6669fe276f93797\t1\ta5b1bd74e47458d7_b\tmain0\t29e3c9dd92b8a571_b\tmain0\t0\tUnspecified\t1\t0\tRgba8UnormSrgb:f:-\ta0.Half4.0.16,l16.16.PerVertex.1",
    "R\t5e5cbbddeba11416\td472f52801c9f603\t1\t35514a24cf1b902d_b\tmain0\t1a42d67ae0db7474_b\tmain0\t0\tUnspecified\t1\t0\tRg11B10Float:f:SrcAlpha/OneMinusSrcAlpha/Add/One/OneMinusSrcAlpha/Add\ta0.Float4.0.16,a1.Float2.16.16,l16.24.PerVertex.1",
    "C\t1fed1b639f1e6461\ta361c4347e39b1c3_b\tmain0",
    "C\tbe8ce1344f659303\t6bc691164ef5cd0e_b\tmain0",
    "M\tb97b049ed81db3b6\ta5288d0a6c4971fd_b\tmain0\td4171cf9adf0504b_b\tmain0\tea1bf96b72e72476_b\tmain0\t1\t0\t32\t32\t256\t-",
    "M\t5c869320e1f9a36e\t-\t-\t17c8ab6419068d4b_b\tmain0\t73f646b30d90fd57_b\tmain0\t1\t0\t0\t32\t0\tRg11B10Float:f:-",
    "R\te132c792c0038e84\t634673b727b39245\t0\ta49b1e303cbb1d57_b\tmain0\tc78cff2cdd956442_b\tmain0\t0\tPoint\t1\t0\tRgba32Float:f:One/One/Add/One/One/Add,Rgba32Float:0:One/One/Add/One/One/Add\ta0.Float.0.16,a1.Float.4.16,a2.Float.8.16,l16.12.PerVertex.1",
];

fn check(lines: &[&str]) {
    let text = format!("nxkit-pipelines-seen 2\n{}\n", lines.join("\n"));
    let seen = Seen::parse(&text).unwrap();
    assert_eq!(seen.entries().len(), lines.len(), "every line parses");
    for (e, l) in seen.entries().iter().zip(lines) {
        let recorded = u64::from_str_radix(l.split('\t').nth(1).unwrap(), 16).unwrap();
        assert!(
            entry_keys(e).contains(&recorded),
            "the key of a recorded pipeline changed: raise KEY_SCHEME and update this value (recorded {recorded:#018x}, computed {:x?}) for {l:?}",
            entry_keys(e)
        );
    }
}

#[test]
fn the_keys_of_real_entries_are_the_recorded_keys() {
    check(LINES);
}

/// Reads the list named by `NXKIT_SEEN_FILE` and computes the key of every entry again.
#[test]
#[ignore = "needs NXKIT_SEEN_FILE, a pipelines.seen written by an earlier host"]
fn every_entry_of_a_recorded_list_keeps_its_key() {
    let Ok(path) = std::env::var("NXKIT_SEEN_FILE") else {
        eprintln!("NXKIT_SEEN_FILE is not set: nothing checked");
        return;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let mut checked = 0usize;
    let mut unparsed = 0usize;
    let mut mismatches = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for l in text.lines().skip(1) {
        let one = format!("nxkit-pipelines-seen 2\n{l}\n");
        let seen = Seen::parse(&one).unwrap();
        let Some(e) = seen.entries().first() else {
            unparsed += 1;
            continue;
        };
        checked += 1;
        let recorded = u64::from_str_radix(l.split('\t').nth(1).unwrap_or("x"), 16).unwrap_or(0);
        if !entry_keys(e).contains(&recorded) {
            mismatches += 1;
            if bad.len() < 5 {
                bad.push(format!("{l} (computed {:x?})", entry_keys(e)));
            }
        }
    }
    eprintln!(
        "{path}: {checked} entries checked, {unparsed} lines not parsed, {mismatches} mismatches"
    );
    assert!(
        mismatches == 0,
        "the key of a recorded pipeline changed: raise KEY_SCHEME and update this value: {mismatches} of {checked} entries, the first {}:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
