//! The `patch` verb of the control socket (W38): load, unload, list and reload patch modules while the game runs.
//!
//! `patch load PATH` copies the `cdylib` at `PATH`, checks its ABI version and installs it. A patch of the same name
//! is replaced at once, so a reload has no moment without a patch. `patch unload [NAME]` takes the patch out (all
//! patches without a name). It returns when the encoder is no longer inside a patch call, then it unloads the library.
//! `patch reload [NAME]` loads the file of each loaded patch (or of one) again after a rebuild. A relative path is
//! relative to the folder of the host, so use absolute paths.

use std::path::Path;

use nxkit_patch_abi::registry::{self, HOOK_DRAW, HOOK_FRAME, HOOK_PASS};

use crate::control::{Answer, Control};

const USAGE: &str = "patch load PATH | unload [NAME] | list | reload [NAME]";

/// Adds the `patch` verb to the control.
pub fn add(control: &mut Control) {
    control.verb(
        "patch load|unload|list|reload",
        "patch modules: load PATH, unload [NAME], list, reload [NAME] (docs/diagnostics.md)",
        Box::new(patch),
    );
}

fn patch(rest: &str) -> Answer {
    let (sub, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let arg = arg.trim();
    match sub {
        "load" if !arg.is_empty() => load(Path::new(arg)).map(|l| vec![l]),
        "unload" => unload((!arg.is_empty()).then_some(arg)),
        "list" => Ok(list()),
        "reload" => reload((!arg.is_empty()).then_some(arg)),
        _ => Err(format!("usage: {USAGE}")),
    }
}

fn hooks(mask: u32) -> String {
    let names: Vec<&str> = [
        (HOOK_FRAME, "frame"),
        (HOOK_PASS, "pass"),
        (HOOK_DRAW, "draw"),
    ]
    .into_iter()
    .filter(|(b, _)| mask & b != 0)
    .map(|(_, n)| n)
    .collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(",")
    }
}

/// Loads `path` and installs it. The old patch of the same name is unloaded after the install.
fn load(path: &Path) -> Result<String, String> {
    let entry = crate::sys::patchlib::load(path)?;
    let line = format!(
        "loaded {} (hooks {}) from {}",
        entry.name,
        hooks(entry.table.hook_mask()),
        entry.path
    );
    let replaced = registry::install(entry);
    let tail = if replaced.is_some() {
        "  # replaced the loaded patch of this name"
    } else {
        ""
    };
    // The lock of the registry is free here, the drop does the `dlclose`.
    drop(replaced);
    Ok(format!("{line}{tail}"))
}

fn unload(name: Option<&str>) -> Answer {
    let gone = registry::remove(name);
    if gone.is_empty() {
        return Err(match name {
            Some(n) => format!("no patch named {n:?} (patch list shows the loaded ones)"),
            None => "no patch is loaded".into(),
        });
    }
    let lines = gone
        .iter()
        .map(|e| format!("unloaded {}", e.name))
        .collect();
    drop(gone);
    Ok(lines)
}

fn list() -> Vec<String> {
    let loaded = registry::list();
    if loaded.is_empty() {
        return vec!["no patch is loaded".into()];
    }
    loaded
        .iter()
        .map(|l| format!("{}  hooks {}  {}", l.name, hooks(l.hooks), l.path))
        .collect()
}

fn reload(name: Option<&str>) -> Answer {
    let wanted: Vec<_> = registry::list()
        .into_iter()
        .filter(|l| name.is_none_or(|n| l.name == n))
        .collect();
    if wanted.is_empty() {
        return Err(match name {
            Some(n) => format!("no patch named {n:?} (patch list shows the loaded ones)"),
            None => "no patch is loaded".into(),
        });
    }
    let mut out = Vec::new();
    for old in wanted {
        let entry = crate::sys::patchlib::load(Path::new(&old.path))
            .map_err(|e| format!("reload of {} failed, the old patch stays: {e}", old.name))?;
        let new_name = entry.name.clone();
        out.push(format!(
            "reloaded {new_name} (hooks {}) from {}",
            hooks(entry.table.hook_mask()),
            entry.path
        ));
        let replaced = registry::install(entry);
        drop(replaced);
        if new_name != old.name {
            drop(registry::remove(Some(&old.name)));
            out.push(format!(
                "unloaded {} (the rebuilt patch has a new name)",
                old.name
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verb_checks_its_request() {
        assert!(patch("").is_err_and(|e| e.starts_with("usage")));
        assert!(patch("load").is_err_and(|e| e.starts_with("usage")));
        assert!(patch("load /no/such/patch.dylib").is_err());
        assert!(patch("unload nothing").is_err());
        assert!(patch("reload").is_err());
        assert_eq!(patch("list"), Ok(vec!["no patch is loaded".to_string()]));
    }

    #[test]
    fn the_hooks_read_as_names() {
        assert_eq!(hooks(0), "none");
        assert_eq!(hooks(HOOK_FRAME | HOOK_DRAW), "frame,draw");
    }
}
