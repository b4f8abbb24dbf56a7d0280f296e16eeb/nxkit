//! `dm/ios/tools/xcode_projects.py`: the per-game Xcode signing settings (`signing_settings`, read through `plutil`)
//! and the workspace that lists every prepared game (`register_project`, `check_bundle_id`, `prune_unreadable`).
//! The workspace file is read and written as Python's `xml.etree.ElementTree` does (a small element tree with text and
//! tails, `ET.indent` with three spaces, the same serialiser), so the bytes equal the script's.

use super::etree::{Element, indent, parse, to_xml};
use super::resolve;
use super::resources::workspace_settings;
use crate::host::Runner;
use std::ffi::OsStr;
use std::path::Path;

fn data_file(workspace: &Path) -> std::path::PathBuf {
    workspace.join("contents.xcworkspacedata")
}

/// `workspace_tree(workspace)`: the parsed file, or an empty `<Workspace version="1.0">`.
pub fn workspace_tree(workspace: &Path) -> Result<Element, String> {
    let path = data_file(workspace);
    if !path.exists() {
        return Ok(Element::new("Workspace", &[("version", "1.0")]));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("cannot read workspace {}: {}", path.display(), e.0))
}

/// `workspace_projects(workspace)`: the resolved paths of the `absolute:` file references.
pub fn workspace_projects(workspace: &Path) -> Result<Vec<String>, String> {
    let root = workspace_tree(workspace)?;
    let mut refs = Vec::new();
    root.iter("FileRef", &mut refs);
    let mut out = Vec::new();
    for r in refs {
        let loc = r.get("location").unwrap_or("");
        if let Some(("absolute", p)) = loc.split_once(':') {
            out.push(resolve(p)?);
        }
    }
    Ok(out)
}

/// `prune_unreadable(workspace)`: remove the `absolute:` entries this user cannot `stat` (permission errors, not
/// missing files), save the file, and return how many went (with the script's note line, or `None`).
pub fn prune_unreadable(workspace: &Path) -> Result<(usize, Option<String>), String> {
    let path = data_file(workspace);
    if !path.exists() {
        return Ok((0, None));
    }
    let mut root = workspace_tree(workspace)?;
    let mut removed = 0;
    fn walk(e: &mut Element, removed: &mut usize) {
        if e.tag == "Group" {
            e.children.retain(|r| {
                if r.tag != "FileRef" {
                    return true;
                }
                let loc = r.get("location").unwrap_or("");
                let Some(("absolute", p)) = loc.split_once(':') else {
                    return true;
                };
                match std::fs::symlink_metadata(p).and_then(|_| std::fs::metadata(p)) {
                    Ok(_) => true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Err(_) => {
                        *removed += 1;
                        false
                    }
                }
            });
        }
        for c in e.children.iter_mut() {
            walk(c, removed);
        }
    }
    walk(&mut root, &mut removed);
    if removed == 0 {
        return Ok((0, None));
    }
    std::fs::write(&path, to_xml(&root, "UTF-8"))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let what = if removed == 1 { "y" } else { "ies" };
    Ok((
        removed,
        Some(format!(
            "[NOTE] removed {removed} workspace entr{what} the current user cannot read (registered by another macOS user)"
        )),
    ))
}

/// `signing_settings(project)`: the `DmIOS` target's Release `PRODUCT_BUNDLE_IDENTIFIER` and `DEVELOPMENT_TEAM`
/// (string values only), read with `/usr/bin/plutil -convert json`, empty without a project file or target.
pub fn signing_settings(r: &Runner, project: &Path) -> Result<Vec<(String, String)>, String> {
    let pbx = project.join("project.pbxproj");
    if !pbx.is_file() {
        return Ok(Vec::new());
    }
    let json = r
        .stdout(
            "/usr/bin/plutil",
            [
                OsStr::new("-convert"),
                OsStr::new("json"),
                OsStr::new("-o"),
                OsStr::new("-"),
                pbx.as_os_str(),
            ],
            None,
        )
        .map_err(|_| format!("cannot read Xcode project: {}", pbx.display()))?;
    let v = nxb_pyjson::parse(&json)
        .map_err(|_| format!("cannot read Xcode project: {}", pbx.display()))?;
    let bad = || format!("cannot read Xcode project: {}", pbx.display());
    let nxb_pyjson::Value::Object(objects) = v.get("objects").ok_or_else(bad)? else {
        return Err(bad());
    };
    let Some((_, target)) = objects.iter().find(|(_, o)| {
        o.get("isa").and_then(|x| x.as_str()) == Some("PBXNativeTarget")
            && o.get("name").and_then(|x| x.as_str()) == Some("DmIOS")
    }) else {
        return Ok(Vec::new());
    };
    let obj = |k: &str| objects.iter().find(|(n, _)| n == k).map(|(_, o)| o);
    let list = target
        .get("buildConfigurationList")
        .and_then(|x| x.as_str())
        .and_then(obj)
        .ok_or_else(bad)?;
    let configs = list
        .get("buildConfigurations")
        .and_then(|x| x.as_list())
        .ok_or_else(bad)?;
    let mut settings = None;
    for c in configs {
        let c = c.as_str().and_then(obj).ok_or_else(bad)?;
        if c.get("name").and_then(|x| x.as_str()) == Some("Release") {
            settings = Some(c.get("buildSettings").ok_or_else(bad)?);
            break;
        }
    }
    Ok(["PRODUCT_BUNDLE_IDENTIFIER", "DEVELOPMENT_TEAM"]
        .iter()
        .filter_map(|k| {
            let v = settings?.get(k)?.as_str()?;
            Some((k.to_string(), v.to_string()))
        })
        .collect())
}

/// `check_bundle_id(workspace, project, bundle_id)`: refuse a bundle ID another registered game's project uses
/// (case-insensitive). Returns the prune note, if any.
pub fn check_bundle_id(
    r: &Runner,
    workspace: &Path,
    project: &Path,
    bundle_id: &str,
) -> Result<Option<String>, String> {
    let (_, note) = prune_unreadable(workspace)?;
    let me = resolve(&utf8(project)?)?;
    for other in workspace_projects(workspace)? {
        if other == me || !Path::new(&other).exists() {
            continue;
        }
        // the script skips only a project it cannot open, an unreadable project file stops it
        let s = signing_settings(r, Path::new(&other))?;
        let used = s
            .iter()
            .find(|(k, _)| k == "PRODUCT_BUNDLE_IDENTIFIER")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        if used.to_lowercase() == bundle_id.to_lowercase() {
            return Err(format!(
                "bundle ID {bundle_id} is already used by {other}; choose a different --bundle-id for this game"
            ));
        }
    }
    Ok(note)
}

fn utf8(p: &Path) -> Result<String, String> {
    p.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{}: not UTF-8", p.display()))
}

/// `register_project(workspace, project, name, derived)`: add or rename this game's group (other games and settings
/// kept), write the file through a temporary one, and point the workspace's derived data at `derived`.
pub fn register_project(
    workspace: &Path,
    project: &Path,
    name: &str,
    derived: &str,
    user: &str,
) -> Result<(), String> {
    let project = resolve(&utf8(project)?)?;
    let mut root = workspace_tree(workspace)?;
    let location = format!("absolute:{project}");
    let has = |g: &Element| {
        g.children
            .iter()
            .any(|r| r.tag == "FileRef" && r.get("location") == Some(location.as_str()))
    };
    let i = match root
        .children
        .iter()
        .position(|g| g.tag == "Group" && has(g))
    {
        Some(i) => i,
        None => {
            let mut g = Element::new("Group", &[("location", "container:")]);
            g.children
                .push(Element::new("FileRef", &[("location", &location)]));
            root.children.push(g);
            root.children.len() - 1
        }
    };
    root.children[i].set("name", name);
    std::fs::create_dir_all(workspace).map_err(|e| format!("{}: {e}", workspace.display()))?;
    indent(&mut root, "   ");
    let path = data_file(workspace);
    let temp = workspace.join("contents.tmp");
    std::fs::write(&temp, to_xml(&root, "utf-8"))
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    workspace_settings(workspace, derived, user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ios::oracle;

    /// A workspace as Xcode writes it (attributes on their own lines, whitespace text in leaf elements), with a comment,
    /// an entity and another game.
    const XCODE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Workspace\n   version = \"1.0\">\n   <!-- note -->\n   <FileRef\n      location = \"group:Other &amp; Co.xcodeproj\">\n   </FileRef>\n   <Group\n      location = \"container:\"\n      name = \"Old\">\n      <FileRef\n         location = \"absolute:/nonexistent/old/DmIOS.xcodeproj\">\n      </FileRef>\n   </Group>\n</Workspace>\n";

    fn py_register(e: &Path, ws: &Path, project: &Path, name: &str, derived: &Path) {
        let code = "import sys; sys.path.insert(0, sys.argv[1]); import xcode_projects as x; x.register_project(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5])";
        let st = oracle::python()
            .args(["-c", code])
            .arg(e.join("dm/ios/tools"))
            .args([ws, project])
            .arg(name)
            .arg(derived)
            .env("USER", "tester")
            .status()
            .unwrap();
        assert!(st.success());
    }

    /// Oracle: The engine's `register_project` and this one, on a new workspace, on Xcode's file, a second game and a
    /// rename: `contents.xcworkspacedata` and the workspace settings equal after every step.
    #[test]
    fn register_equals_the_reference_script() {
        let Some(e) = oracle::engine() else { return };
        let (pd, rd) = (oracle::scratch("ws-py"), oracle::scratch("ws-rs"));
        for d in [&pd, &rd] {
            std::fs::create_dir_all(d.join("x.xcworkspace")).unwrap();
            std::fs::write(d.join("x.xcworkspace/contents.xcworkspacedata"), XCODE).unwrap();
            std::fs::create_dir_all(d.join("g1/DmIOS.xcodeproj")).unwrap();
        }
        let steps: [(&str, &str, &str); 4] = [
            (
                "new.xcworkspace",
                "g1/DmIOS.xcodeproj",
                "Game <One> & \"1\"",
            ),
            ("x.xcworkspace", "g1/DmIOS.xcodeproj", "Game One"),
            ("x.xcworkspace", "g2/DmIOS.xcodeproj", "Two"),
            ("x.xcworkspace", "g1/DmIOS.xcodeproj", "Renamed"),
        ];
        for (ws, proj, name) in steps {
            py_register(&e, &pd.join(ws), &pd.join(proj), name, &pd.join("dd"));
            register_project(
                &rd.join(ws),
                &rd.join(proj),
                name,
                rd.join("dd").to_str().unwrap(),
                "tester",
            )
            .unwrap();
            for f in [
                "contents.xcworkspacedata",
                "xcuserdata/tester.xcuserdatad/WorkspaceSettings.xcsettings",
            ] {
                let norm = |root: &Path| {
                    let real = std::fs::canonicalize(root).unwrap();
                    std::fs::read_to_string(root.join(ws).join(f))
                        .unwrap()
                        .replace(real.to_str().unwrap(), "<root>")
                        .replace(root.to_str().unwrap(), "<root>")
                };
                assert_eq!(norm(&rd), norm(&pd), "{ws} {f}");
            }
        }
        let _ = std::fs::remove_dir_all(&pd);
        let _ = std::fs::remove_dir_all(&rd);
    }

    /// Oracle: `signing_settings` on a project `xcodegen` would write (old-style plist), as the engine reads it.
    #[test]
    fn signing_equals_the_reference_script() {
        let Some(e) = oracle::engine() else { return };
        let d = oracle::scratch("ws-sign");
        let p = d.join("DmIOS.xcodeproj");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("project.pbxproj"),
            "// !$*UTF8*$!\n{\n\tarchiveVersion = 1;\n\tobjects = {\n\t\tT1 = {isa = PBXNativeTarget; name = DmIOS; buildConfigurationList = L1; };\n\t\tL1 = {isa = XCConfigurationList; buildConfigurations = (C1, C2); };\n\t\tC1 = {isa = XCBuildConfiguration; name = Debug; buildSettings = {PRODUCT_BUNDLE_IDENTIFIER = org.debug; }; };\n\t\tC2 = {isa = XCBuildConfiguration; name = Release; buildSettings = {PRODUCT_BUNDLE_IDENTIFIER = \"org.nxnative.x\"; DEVELOPMENT_TEAM = ABCDE12345; OTHER = (a, b); }; };\n\t};\n\trootObject = T1;\n}\n",
        )
        .unwrap();
        let code = "import sys, json; sys.path.insert(0, sys.argv[1]); import xcode_projects as x; print(json.dumps(sorted(x.signing_settings(sys.argv[2]).items())))";
        let o = oracle::python()
            .args(["-c", code])
            .arg(e.join("dm/ios/tools"))
            .arg(&p)
            .output()
            .unwrap();
        assert!(o.status.success());
        let r = Runner::from_parent().unwrap();
        let mut got = signing_settings(&r, &p).unwrap();
        got.sort();
        let v: Vec<nxb_pyjson::Value> = got
            .iter()
            .map(|(k, v)| {
                nxb_pyjson::Value::List(vec![nxb_pyjson::Value::str(k), nxb_pyjson::Value::str(v)])
            })
            .collect();
        assert_eq!(
            nxb_pyjson::dumps(&nxb_pyjson::Value::List(v)) + "\n",
            String::from_utf8(o.stdout).unwrap()
        );
        assert_eq!(got.len(), 2);
        // check_bundle_id: the other game's project uses the ID
        let ws = d.join("w.xcworkspace");
        register_project(&ws, &p, "A", d.join("dd").to_str().unwrap(), "tester").unwrap();
        let mine = d.join("mine/DmIOS.xcodeproj");
        assert!(check_bundle_id(&r, &ws, &mine, "ORG.nxnative.X").is_err());
        assert!(check_bundle_id(&r, &ws, &mine, "org.other").is_ok());
        assert!(check_bundle_id(&r, &ws, &p, "org.nxnative.x").is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}
