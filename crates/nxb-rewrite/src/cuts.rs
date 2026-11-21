//! The registered cuts of a build, or the function-matched ones for a build no profile registers.

use std::collections::HashSet;
use std::fmt::Write;

use crate::error::RewriteError;
use crate::matchfn::{CutRow, Outcome, resolve};
use crate::options::Options;
use crate::patches::{Cut, PatchTable};
use crate::rw::{R, Rw};

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
    let prof_rules = patches
        .rules
        .get(profile)
        .and_then(|m| m.get(name))
        .unwrap_or(&empty);
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
    let a = resolve(&rw.img, rw.text_end, prof_rules, log, &mut outcomes, true)
        .map_err(RewriteError::Failed)?;
    cuts.extend(conv(a));
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
