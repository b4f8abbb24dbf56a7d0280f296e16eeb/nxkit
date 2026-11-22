//! The registered cuts of a build, or the function-matched ones for a build no profile registers.

use std::collections::HashSet;
use std::fmt::Write;

use crate::error::RewriteError;
use crate::matchfn::{CutRow, Outcome, resolve};
use crate::options::Options;
use crate::patches::{Cut, PatchTable, Rule};
use crate::rw::{R, Rw};

/// The function-matched rules of a module from the given scopes, in their order.
fn scoped_rules<'a>(
    patches: &PatchTable,
    scopes: impl IntoIterator<Item = &'a str>,
    module: &str,
) -> Vec<Rule> {
    scopes
        .into_iter()
        .filter_map(|scope| patches.rules.get(scope).and_then(|m| m.get(module)))
        .flatten()
        .cloned()
        .collect()
}

pub(crate) fn cuts_for(
    rw: &Rw,
    opts: &Options,
    patches: &PatchTable,
    build_id: &str,
    log: &mut Vec<String>,
    extra: &mut Vec<(String, Vec<u8>)>,
) -> R<Vec<Cut>> {
    let mut cuts: Vec<Cut> = patches.cuts.get(build_id).cloned().unwrap_or_default();
    if patches.cuts.contains_key(build_id) {
        return Ok(cuts);
    }
    let name = &opts.name;
    let profile = &opts.profile;
    let (mut known, mut own): (Vec<(String, String, usize)>, HashSet<String>) =
        (Vec::new(), HashSet::new());
    if !profile.is_empty()
        && let Some(rows) = patches.gbuild.get(profile)
    {
        for row in rows {
            let c: Vec<&str> = row.iter().map(|x| x.trim()).collect();
            if c.first() == Some(&"build") && c.len() > 3 {
                let bid = c[3..]
                    .iter()
                    .filter_map(|x| x.split_once('='))
                    .rev()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.to_string());
                if let Some(b) = bid {
                    own.insert(b.clone());
                    if b != build_id
                        && let Some(cs) = patches.cuts.get(&b)
                        && !cs.is_empty()
                    {
                        known.push((b, c[2].to_string(), cs.len()));
                    }
                }
            }
        }
    }
    let (mut outcomes, mut sdk_outcomes): (Vec<Outcome>, Vec<Outcome>) = (Vec::new(), Vec::new());
    let empty = Vec::new();
    let prof_rules = scoped_rules(patches, [profile.as_str()], name);
    // the families' rules: a family rule that matches one version of its engine skips the others, so a skip is no
    // [RED] line
    let layer_rules = scoped_rules(patches, opts.layers.iter().map(String::as_str), name);
    let sdk_rules = patches
        .rules
        .get("sdk")
        .and_then(|m| m.get(name))
        .unwrap_or(&empty);
    let conv = |rows: Vec<CutRow>| {
        rows.into_iter().map(|(off, old, new, why)| Cut {
            off: off as usize,
            old,
            new,
            why,
        })
    };
    let a = resolve(&rw.img, rw.text_end, &prof_rules, log, &mut outcomes, true)
        .map_err(RewriteError::Failed)?;
    cuts.extend(conv(a));
    let f = resolve(
        &rw.img,
        rw.text_end,
        &layer_rules,
        log,
        &mut outcomes,
        false,
    )
    .map_err(RewriteError::Failed)?;
    cuts.extend(conv(f));
    let b = resolve(
        &rw.img,
        rw.text_end,
        sdk_rules,
        log,
        &mut sdk_outcomes,
        false,
    )
    .map_err(RewriteError::Failed)?;
    cuts.extend(conv(b));
    if !own.contains(build_id)
        && (!known.is_empty() || !outcomes.is_empty() || !sdk_outcomes.is_empty())
    {
        let mut s = format!(
            "# {name} {build_id}: not a registered build. registered: build ID, display version, cuts; patch (the game profile's) or sdk (SDK-wide): name, cut offsets or -, implementation or why not\n"
        );
        for (bid, disp, n) in &known {
            let _ = writeln!(s, "registered\t{bid}\t{disp}\t{n}");
        }
        for (kind, got) in [("patch", &outcomes), ("sdk", &sdk_outcomes)] {
            for (n, offs, why) in got {
                let o = match offs {
                    Some(v) if !v.is_empty() => v
                        .iter()
                        .map(|x| format!("{x:#x}"))
                        .collect::<Vec<_>>()
                        .join(","),
                    _ => "-".into(),
                };
                let _ = writeln!(s, "{kind}\t{n}\t{o}\t{why}");
            }
        }
        extra.push((format!("{name}.match.tsv"), s.into_bytes()));
    }
    Ok(cuts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(scope: &str, rule: &str) -> (String, String) {
        let text = format!(
            "scope = \"{scope}\"\n\n[[rule]]\nbinding = \"engine\"\nkind = \"match\"\nmodule = \"main\"\nname = \"{rule}\"\n\
             multi = false\nevidence = \"test\"\n\n[[rule.function]]\nsize = 4\nfingerprints = [\"00\"]\n\n\
             [[rule.function.cut]]\ndelta = 0\nold = \"1f2003d5\"\nnew = \"1f2003d5\"\nwhy = \"nop\"\n"
        );
        (format!("{scope}.toml"), text)
    }

    #[test]
    fn the_rules_of_a_scope_are_its_own() {
        let t = PatchTable::from_toml(&[
            file("0100000000010000", "title"),
            file("unity", "engine"),
            file("vulkan", "graphics"),
            file("other", "not asked"),
        ])
        .unwrap();
        let names = |layers: &[&str]| -> Vec<String> {
            std::iter::once("0100000000010000")
                .chain(layers.iter().copied())
                .flat_map(|s| scoped_rules(&t, [s], "main"))
                .map(|r| r.name)
                .collect()
        };
        assert_eq!(names(&[]), ["title"]);
        assert_eq!(names(&["unity", "vulkan"]), ["title", "engine", "graphics"]);
        assert!(scoped_rules(&t, ["0100000000010000", "unity"], "sdk").is_empty());
    }
}
