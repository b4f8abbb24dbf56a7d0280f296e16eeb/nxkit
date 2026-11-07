//! The one list of families and title modules.
//!
//! Each module is behind its own cargo feature (`agl`, `oot`, `botw`, `acnh`), so a build can leave any of them out. This is the
//! only crate that depends on family and title crates. The core depends on this crate and on `nxtitle`.

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
    ];
    Catalog::new(families, titles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_builds() {
        let c = catalog().unwrap();
        #[cfg(feature = "agl")]
        assert!(c.missing_families().is_empty());
        #[cfg(not(feature = "agl"))]
        assert_eq!(
            c.missing_families().len(),
            usize::from(cfg!(feature = "oot"))
                + usize::from(cfg!(feature = "botw"))
                + usize::from(cfg!(feature = "acnh"))
        );
        #[cfg(not(any(feature = "agl", feature = "oot", feature = "botw", feature = "acnh")))]
        assert!(c.titles().is_empty() && c.families().is_empty());
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
}
