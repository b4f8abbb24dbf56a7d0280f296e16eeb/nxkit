//! Console variables and keys of the engine configuration through an extra pak: the project's
//! `Config/DefaultDeviceProfiles.ini` is copied with the variables set in the section of the platform's base device
//! profile, `Config/DefaultEngine.ini` with the keys set in their sections, and both are written as a pak that sorts
//! after all others. The engine reads each file from the pak mounted last, so the copies replace the game's files.
//!
//! The engine mounts the paks of `<Project>/Content/Paks` and reads a file from the pak that was mounted last.
//! The order is: a pak named `*_P.pak` over one without, then the higher patch number in `<Project>-<Platform>_<n>_P.pak`
//! over the lower. A `_P` pak without a number counts as number 0. The new pak gets the number after the highest
//! one, or 1 when there is no patch pak.

use crate::{ini, pak};
use nxtitle::{ConsoleSettings, RomfsRead};

/// The device profiles, where the console variables go.
const INI: &str = "Config/DefaultDeviceProfiles.ini";
/// The engine configuration, where the keys go.
const ENGINE_INI: &str = "Config/DefaultEngine.ini";

/// A pak file name `<Project>-<Platform>[_<n>][_P].pak`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PakName {
    platform: String,
    patch: bool,
    number: u32,
}

impl PakName {
    fn parse(file: &str, project: &str) -> Option<PakName> {
        let stem = file.strip_suffix(".pak")?;
        let rest = stem.strip_prefix(project)?.strip_prefix('-')?;
        let (rest, patch) = match rest.strip_suffix("_P") {
            Some(r) => (r, true),
            None => (rest, false),
        };
        let (platform, number) = match rest.rsplit_once('_') {
            Some((p, n)) if patch && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => {
                (p, n.parse().ok()?)
            }
            _ => (rest, 0),
        };
        (!platform.is_empty()).then(|| PakName {
            platform: platform.into(),
            patch,
            number,
        })
    }

    /// Higher sorts later and wins.
    fn rank(&self) -> (bool, u32) {
        (self.patch, self.number)
    }
}

/// The paks of the project folder `project`, the highest priority first.
fn paks(romfs: &dyn RomfsRead, project: &str) -> Result<Vec<(String, PakName)>, String> {
    let dir = format!("{project}/Content/Paks");
    let mut found: Vec<(String, PakName)> = romfs
        .list(&dir)?
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.dir)
        .filter_map(|e| PakName::parse(&e.name, project).map(|n| (e.name, n)))
        .collect();
    found.sort_by(|a, b| b.1.rank().cmp(&a.1.rank()).then_with(|| b.0.cmp(&a.0)));
    Ok(found
        .into_iter()
        .map(|(f, n)| (format!("{dir}/{f}"), n))
        .collect())
}

/// The file `ini` of the highest priority pak that holds it.
fn find_ini(
    romfs: &dyn RomfsRead,
    project: &str,
    paks: &[(String, PakName)],
    ini: &str,
) -> Result<Option<Vec<u8>>, String> {
    let want = format!("{project}/{ini}");
    for (path, _) in paks {
        let entries = pak::read_index(romfs, path)?;
        if let Some(e) = entries
            .iter()
            .find(|e| e.path().eq_ignore_ascii_case(&want))
        {
            return pak::read_entry(romfs, path, e).map(Some);
        }
    }
    Ok(None)
}

/// The new pak (path from the RomFS root, bytes) that sets `set`.
pub(crate) fn files(
    set: ConsoleSettings<'_>,
    romfs: &dyn RomfsRead,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    if set.is_empty() {
        return Ok(Vec::new());
    }
    let mut projects: Vec<(String, Vec<(String, PakName)>)> = Vec::new();
    for e in romfs.list("")?.unwrap_or_default() {
        if !e.dir || e.name.eq_ignore_ascii_case("Engine") {
            continue;
        }
        let found = paks(romfs, &e.name)?;
        if !found.is_empty() {
            projects.push((e.name, found));
        }
    }
    let mut hits = Vec::new();
    for (project, found) in &projects {
        if let Some(ini) = find_ini(romfs, project, found, INI)? {
            hits.push((project, found, ini));
        }
    }
    let (project, found, ini_bytes) = match hits.len() {
        0 => return Err(format!("no pak in */Content/Paks holds a {INI}")),
        1 => hits.remove(0),
        _ => return Err(format!("more than one project has a {INI}")),
    };
    let platform = &found[0].1.platform;
    let text = |bytes: Vec<u8>, ini: &str| {
        String::from_utf8(bytes).map_err(|_| format!("{project}/{ini} is not UTF-8 text"))
    };
    let mut inside: Vec<(String, String)> = Vec::new();
    if !set.vars.is_empty() {
        let edited = ini::set_cvars(
            &text(ini_bytes, INI)?,
            &format!("{platform} DeviceProfile"),
            set.vars,
        )?;
        inside.push((format!("{project}/{INI}"), edited));
    }
    if !set.keys.is_empty() {
        let engine = find_ini(romfs, project, found, ENGINE_INI)?
            .ok_or_else(|| format!("no pak of {project} holds a {ENGINE_INI}"))?;
        let edited = ini::set_keys(&text(engine, ENGINE_INI)?, set.keys);
        inside.push((format!("{project}/{ENGINE_INI}"), edited));
    }
    let n = found
        .iter()
        .filter(|(_, n)| n.patch)
        .map(|(_, n)| n.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("the patch number overflows")?;
    let entries: Vec<(&str, &[u8])> = inside
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_bytes()))
        .collect();
    let bytes = pak::build("../../../", &entries)?;
    Ok(vec![(
        format!("{project}/Content/Paks/{project}-{platform}_{n}_P.pak"),
        bytes,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pak::tests::Mem;
    use nxtitle::{ConfigKey, ConsoleVar};

    const VAR: ConsoleVar = ConsoleVar {
        name: "r.ScreenPercentage",
        value: "50",
    };

    fn pak_with(ini: &str) -> Vec<u8> {
        pak::build(
            "../../../",
            &[("G/Config/DefaultDeviceProfiles.ini", ini.as_bytes())],
        )
        .unwrap()
    }

    fn game(paks: &[(&str, &str)]) -> Mem {
        let mut m = Mem::default();
        for (name, ini) in paks {
            m.0.insert(format!("G/Content/Paks/{name}"), pak_with(ini));
        }
        m
    }

    const SECTION: &str = "[Switch DeviceProfile]\nDeviceType=Switch\n";

    fn set(vars: &[ConsoleVar]) -> ConsoleSettings<'_> {
        ConsoleSettings { vars, keys: &[] }
    }

    fn edited(out: &[(String, Vec<u8>)]) -> (String, String) {
        let (path, bytes) = &out[0];
        let mem = Mem([(path.clone(), bytes.clone())].into());
        let es = pak::read_index(&mem, path).unwrap();
        assert_eq!(es.len(), 1);
        let text = pak::read_entry(&mem, path, &es[0]).unwrap();
        (path.clone(), String::from_utf8(text).unwrap())
    }

    #[test]
    fn names_parse() {
        let p = |f| PakName::parse(f, "G");
        assert_eq!(p("G-Switch.pak").map(|n| n.rank()), Some((false, 0)));
        assert_eq!(p("G-Switch_P.pak").map(|n| n.rank()), Some((true, 0)));
        assert_eq!(
            p("G-Switch_12_P.pak").map(|n| (n.platform, n.number)),
            Some(("Switch".into(), 12))
        );
        assert_eq!(p("H-Switch.pak"), None);
        assert_eq!(
            p("G-Switch_1.pak").map(|n| n.platform),
            Some("Switch_1".into())
        );
    }

    #[test]
    fn the_new_pak_follows_the_highest_patch() {
        let mem = game(&[
            ("G-Switch.pak", SECTION),
            ("G-Switch_0_P.pak", SECTION),
            ("G-Switch_3_P.pak", SECTION),
        ]);
        let out = files(set(&[VAR]), &mem).unwrap();
        let (path, text) = edited(&out);
        assert_eq!(path, "G/Content/Paks/G-Switch_4_P.pak");
        assert!(text.ends_with("+CVars=r.ScreenPercentage=50\n"));
    }

    #[test]
    fn without_a_patch_pak_the_number_is_one() {
        let mem = game(&[("G-Switch.pak", SECTION)]);
        let out = files(set(&[VAR]), &mem).unwrap();
        assert_eq!(out[0].0, "G/Content/Paks/G-Switch_1_P.pak");
    }

    #[test]
    fn the_ini_of_the_highest_priority_pak_is_the_one_edited() {
        let mem = game(&[
            ("G-Switch.pak", "[Switch DeviceProfile]\nfrom=base\n"),
            ("G-Switch_0_P.pak", "[Switch DeviceProfile]\nfrom=patch0\n"),
            ("G-Switch_1_P.pak", "[Other]\n"),
        ]);
        // the pak with the highest number has no such section, so it fails: it is the copy that wins
        assert!(files(set(&[VAR]), &mem).unwrap_err().contains("no section"));
        let mut mem = mem;
        mem.0.remove("G/Content/Paks/G-Switch_1_P.pak");
        let (_, text) = edited(&files(set(&[VAR]), &mem).unwrap());
        assert!(text.contains("from=patch0") && !text.contains("from=base"));
    }

    #[test]
    fn a_pak_without_the_ini_is_skipped() {
        let mut mem = game(&[("G-Switch.pak", SECTION)]);
        mem.0.insert(
            "G/Content/Paks/G-Switch_2_P.pak".into(),
            pak::build("../../../", &[("G/Other.txt", b"x")]).unwrap(),
        );
        let out = files(set(&[VAR]), &mem).unwrap();
        assert_eq!(out[0].0, "G/Content/Paks/G-Switch_3_P.pak");
    }

    #[test]
    fn a_missing_ini_is_an_error() {
        let mem = Mem::default();
        assert!(files(set(&[VAR]), &mem).is_err());
    }

    #[test]
    fn a_key_goes_into_a_copy_of_the_engine_ini_in_the_same_pak() {
        let mut mem = Mem::default();
        mem.0.insert(
            "G/Content/Paks/G-Switch.pak".into(),
            pak::build(
                "../../../",
                &[
                    ("G/Config/DefaultDeviceProfiles.ini", SECTION.as_bytes()),
                    (
                        "G/Config/DefaultEngine.ini",
                        b"[UI]\nCurve=(old)\nOther=1\n",
                    ),
                ],
            )
            .unwrap(),
        );
        const KEY: ConfigKey = ConfigKey {
            section: "UI",
            key: "Curve",
            value: "(new)",
        };
        let out = files(
            ConsoleSettings {
                vars: &[VAR],
                keys: &[KEY],
            },
            &mem,
        )
        .unwrap();
        let (path, bytes) = &out[0];
        assert_eq!(path, "G/Content/Paks/G-Switch_1_P.pak");
        let m = Mem([(path.clone(), bytes.clone())].into());
        let es = pak::read_index(&m, path).unwrap();
        let read = |name: &str| {
            let e = es.iter().find(|e| e.path() == name).unwrap();
            String::from_utf8(pak::read_entry(&m, path, e).unwrap()).unwrap()
        };
        assert!(
            read("G/Config/DefaultDeviceProfiles.ini").ends_with("+CVars=r.ScreenPercentage=50\n")
        );
        assert_eq!(
            read("G/Config/DefaultEngine.ini"),
            "[UI]\nCurve=(new)\nOther=1\n"
        );
        let keys_only = files(
            ConsoleSettings {
                vars: &[],
                keys: &[KEY],
            },
            &mem,
        )
        .unwrap();
        let m = Mem([(keys_only[0].0.clone(), keys_only[0].1.clone())].into());
        assert_eq!(pak::read_index(&m, &keys_only[0].0).unwrap().len(), 1);
    }
}
