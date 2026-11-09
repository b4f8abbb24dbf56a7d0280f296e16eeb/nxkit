//! The check of a function-matching rule (`nso_match.validate`): a malformed rule is refused, and so is a replacement
//! whose meaning depends on an address.

use crate::matchfn::{relative, unhex};
use crate::patches::{FnRule, Rule};

/// `_pc_relative`: a branch, `adr`, `adrp` or a literal load.
fn pc_relative(w: u32) -> bool {
    relative(w).is_some() || w & 0x1f00_0000 == 0x1000_0000 || w & 0x3b00_0000 == 0x1800_0000
}

fn is_hex(s: &str) -> bool {
    s.len().is_multiple_of(2) && s.bytes().all(|c| c.is_ascii_hexdigit())
}

fn function(name: &str, f: &FnRule) -> Result<(), String> {
    if name.is_empty() || f.size == 0 || !f.size.is_multiple_of(4) {
        return Err("invalid function size/name".into());
    }
    if f.fps.is_empty() {
        return Err("missing function fingerprints".into());
    }
    if f.fps
        .iter()
        .any(|d| d.len() != 64 || !d.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
    {
        return Err("invalid function fingerprint".into());
    }
    if f.cuts.is_empty() {
        return Err("function patch has no cuts".into());
    }
    let mut used = std::collections::HashSet::new();
    for (delta, old, new, reason) in &f.cuts {
        if !is_hex(old) || !is_hex(new) {
            return Err("non-hexadecimal number in a cut".into());
        }
        let (ob, nb) = (unhex(old), unhex(new));
        if reason.is_empty()
            || ob.len() != nb.len()
            || ob.is_empty()
            || !ob.len().is_multiple_of(4)
            || !delta.is_multiple_of(4)
            || *delta + ob.len() > f.size
        {
            return Err("invalid function-relative cut".into());
        }
        for b in *delta..*delta + ob.len() {
            if !used.insert(b) {
                return Err("overlapping cuts".into());
            }
        }
        for (i, c) in nb.as_chunks::<4>().0.iter().enumerate() {
            let w = u32::from_le_bytes(*c);
            // The fingerprint keeps a function's internal branches exact, so a branch that stays inside it means
            // the same on every build that matches. Anything else depends on an address.
            let inside = relative(w).is_some_and(|(_, d)| {
                let at = (*delta + i * 4) as i64 + d;
                0 <= at && at < f.size as i64
            });
            if pc_relative(w) && !inside {
                return Err("replacement must not contain PC-relative instructions, except branches inside the matched function".into());
            }
        }
    }
    Ok(())
}

/// `validate(rule)`: the rule is well formed.
pub(crate) fn validate(rule: &Rule) -> Result<(), String> {
    if rule.multi && rule.fns.len() < 2 {
        return Err("a patch of several functions needs at least two".into());
    }
    rule.fns.iter().try_for_each(|f| function(&rule.name, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOP: u32 = 0xd503_201f;
    const FP: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn hex(ws: &[u32]) -> String {
        ws.iter()
            .flat_map(|w| w.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `b` from word 0 of the replacement to `disp` bytes away (the word is relative, so its own place does not matter).
    fn b(disp: i64) -> u32 {
        0x1400_0000 | ((disp / 4) as u32 & 0x03ff_ffff)
    }

    fn rule(cut: Vec<(usize, String, String, String)>) -> Rule {
        Rule {
            name: "p".into(),
            fns: vec![FnRule {
                size: 28,
                fps: vec![FP.into()],
                cuts: cut,
            }],
            multi: false,
        }
    }

    fn one(delta: usize, old: &[u32], new: &[u32]) -> Rule {
        rule(vec![(delta, hex(old), hex(new), "heap".into())])
    }

    const MOV: u32 = 0xaa17_03e0;

    #[test]
    fn replacement_branches_must_stay_inside_the_function() {
        // the cut is at +8 of a function of 28 bytes: its first word and its last
        for d in [-8, 16] {
            assert_eq!(validate(&one(8, &[MOV], &[b(d)])), Ok(()), "{d}");
        }
        // before the function and just past it
        for d in [-12, 20] {
            let e = validate(&one(8, &[MOV], &[b(d)])).unwrap_err();
            assert!(e.contains("PC-relative"), "{d}: {e}");
        }
        // the second word of a replacement counts from its own place
        let e = validate(&one(8, &[MOV, NOP], &[NOP, b(16)])).unwrap_err();
        assert!(e.contains("PC-relative"), "{e}");
    }

    #[test]
    fn relocations_in_the_replacement_are_rejected() {
        let adrp = 0x9000_0000 | 1 << 29;
        for w in [b(0x1000), adrp, 0x5800_0020] {
            let e = validate(&one(8, &[MOV], &[w])).unwrap_err();
            assert!(e.contains("PC-relative"), "{w:#x}: {e}");
        }
        assert_eq!(validate(&one(8, &[MOV], &[0x9140_12e0])), Ok(()));
    }

    #[test]
    fn malformed_rules_are_refused() {
        let ok = one(8, &[MOV], &[NOP]);
        let mut r = ok.clone();
        r.fns[0].size = 10;
        assert!(validate(&r).is_err());
        let mut r = ok.clone();
        r.fns[0].fps = vec!["AB".into()];
        assert!(validate(&r).is_err());
        let mut r = ok.clone();
        r.fns[0].cuts[0].0 = 28;
        assert!(validate(&r).is_err());
        let mut r = ok.clone();
        r.fns[0]
            .cuts
            .push((8, hex(&[MOV]), hex(&[NOP]), "again".into()));
        assert_eq!(validate(&r), Err("overlapping cuts".into()));
        let mut r = ok;
        r.multi = true;
        assert!(validate(&r).is_err());
    }
}
