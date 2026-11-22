//! The version of the input that a title module is made for.

use std::path::PathBuf;

/// The update of `version` among `updates`: none for v0, the base game. Fails with exit code 4 when the input lacks the
/// version: an update of another version, or a NUP that holds only updates for a title module of v0 (issue 47).
pub(super) fn updates_for(
    name: &str,
    version: u32,
    has_base: bool,
    updates: &[(u32, PathBuf)],
) -> Result<Vec<(u32, PathBuf)>, String> {
    let have = || {
        let have: Vec<String> = updates.iter().map(|u| format!("v{}", u.0)).collect();
        if have.is_empty() {
            "none".to_string()
        } else {
            have.join(", ")
        }
    };
    if version == 0 {
        if has_base {
            return Ok(Vec::new());
        }
        return Err(crate::exit::fail(
            4,
            format!(
                "{name}: the title module is made for v0, the base game, and the input holds only updates (found: {}). Add the base game to the input",
                have()
            ),
        ));
    }
    match updates.iter().find(|u| u.0 == version) {
        Some(u) => Ok(vec![u.clone()]),
        None => Err(crate::exit::fail(
            4,
            format!(
                "{name}: the title module is made for v{version} and the input has no update with that version (found: {})",
                have()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::updates_for;
    use std::path::PathBuf;

    fn ups(v: &[u32]) -> Vec<(u32, PathBuf)> {
        v.iter().map(|v| (*v, PathBuf::from("g.nup"))).collect()
    }

    #[test]
    fn v0_takes_the_base_game() {
        assert!(
            updates_for("G", 0, true, &ups(&[65536]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn v0_without_a_base_game_is_not_supported() {
        let e = updates_for("G", 0, false, &ups(&[917_504])).unwrap_err();
        assert!(e.contains("v917504"), "{e}");
    }

    #[test]
    fn an_update_version_takes_that_update() {
        let u = updates_for("G", 131_072, false, &ups(&[65536, 131_072])).unwrap();
        assert_eq!(u, ups(&[131_072]));
        assert!(updates_for("G", 131_072, true, &ups(&[65536])).is_err());
    }
}
