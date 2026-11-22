//! The engine markers of a game without a title module: one pass over the read-only data of the modules for the markers
//! of every family, and one over the RomFS file names. The hits are libraries of `TitleFacts`; the families decide.

use nxtitle::{Catalog, Layer, Marker, MarkerAt};
use std::collections::BTreeSet;

/// The ids of the markers that hit, once each.
pub(super) fn hits<'a>(
    catalog: &Catalog,
    rodata: impl IntoIterator<Item = &'a [u8]>,
    romfs_paths: impl IntoIterator<Item = &'a str>,
) -> Vec<&'static str> {
    let markers: Vec<&'static Marker> = catalog
        .families()
        .iter()
        .filter(|f| f.layer() == Layer::Engine)
        .flat_map(|f| f.markers().iter())
        .collect();
    let mut found = BTreeSet::new();
    // rodata: every position is looked at once. The table of first bytes keeps the check short.
    let byte_markers: Vec<&Marker> = markers
        .iter()
        .copied()
        .filter(|m| m.at == MarkerAt::Rodata && !m.pattern.is_empty())
        .collect();
    let mut first = [false; 256];
    for m in &byte_markers {
        first[m.pattern.as_bytes()[0] as usize] = true;
    }
    for data in rodata {
        for (i, b) in data.iter().enumerate() {
            if !first[*b as usize] {
                continue;
            }
            for m in &byte_markers {
                if !found.contains(m.id) && data[i..].starts_with(m.pattern.as_bytes()) {
                    found.insert(m.id);
                }
            }
        }
    }
    let path_markers: Vec<&Marker> = markers
        .iter()
        .copied()
        .filter(|m| m.at == MarkerAt::RomfsPath)
        .collect();
    for path in romfs_paths {
        for m in &path_markers {
            if !found.contains(m.id) && m.matches_path(path) {
                found.insert(m.id);
            }
        }
    }
    found.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxtitle::{BuildHooks, Evidence, Family, Match, RuntimeHooks, TitleFacts};

    struct F;
    impl BuildHooks for F {}
    impl RuntimeHooks for F {}
    impl Family for F {
        fn id(&self) -> &'static str {
            "f"
        }
        fn name(&self) -> &'static str {
            "F"
        }
        fn layer(&self) -> Layer {
            Layer::Engine
        }
        fn detect(&self, _: &TitleFacts) -> Match {
            Match::Unknown
        }
        fn markers(&self) -> &'static [Marker] {
            const E: Evidence = Evidence::Unverified { note: "test" };
            &[
                Marker {
                    id: "rod",
                    at: MarkerAt::Rodata,
                    pattern: "N4sead",
                    evidence: E,
                },
                Marker {
                    id: "path",
                    at: MarkerAt::RomfsPath,
                    pattern: "Data/",
                    evidence: E,
                },
            ]
        }
    }

    #[test]
    fn rodata_and_paths_hit_once_each() {
        static FAM: F = F;
        let c = Catalog::new(vec![&FAM], vec![]).unwrap();
        let ro: Vec<&[u8]> = vec![b"xx\0N4sead4HeapE\0N4sead", b"none"];
        let h = hits(&c, ro, ["a/b", "Data/x", "Data/y"]);
        assert_eq!(h, ["path", "rod"]);
        assert!(hits(&c, [b"N4sea".as_slice()], ["MetaData/x"]).is_empty());
    }
}
