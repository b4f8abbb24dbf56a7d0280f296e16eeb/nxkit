//! The libraries that a person swapped in at run time (W36, `crate::hotswap`).
//!
//! A swapped library replaces the library of a program when the program is read. The cache of programs forgets the
//! programs of a changed key, so the next draw reads it again. The hot path does not look here.

use std::sync::{PoisonError, RwLock};

use super::{Programs, StageProgram};
use crate::hotswap::SwapMap;

/// The swaps that are in force.
#[derive(Default)]
pub(super) struct Swaps(RwLock<SwapMap>);

impl Swaps {
    /// Puts the swapped library of `key` into `p`, when there is one.
    pub(super) fn apply(&self, key: &str, p: &mut StageProgram) {
        let map = self.0.read().unwrap_or_else(PoisonError::into_inner);
        if let Some((lib, n)) = map.get(key) {
            p.lib = lib.clone();
            p.swap = *n;
        }
    }
}

impl Programs {
    /// Makes `now` the swaps in force. The programs of every key that changed are read again at their next use.
    pub(crate) fn set_swaps(&self, now: SwapMap) {
        let changed: std::collections::BTreeSet<String> = {
            let mut cur = self.swaps.0.write().unwrap_or_else(PoisonError::into_inner);
            let number = |m: &SwapMap, k: &str| m.get(k).map(|e| e.1);
            let keys: std::collections::BTreeSet<String> = cur
                .keys()
                .chain(now.keys())
                .filter(|k| number(&cur, k) != number(&now, k))
                .cloned()
                .collect();
            *cur = now;
            keys
        };
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        for k in &changed {
            for m in cache.iter_mut() {
                m.remove(k.as_str());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skip::Skip;
    use nxkit_metal::{Context, ShaderLibrary};

    const SOURCE: &str =
        "#include <metal_stdlib>\nusing namespace metal;\nfragment float4 main0() { return 1; }\n";

    #[test]
    fn a_changed_swap_makes_the_program_read_again() {
        let Ok(ctx) = Context::system_default() else {
            return;
        };
        let lib = ShaderLibrary::from_source(&ctx, "t", SOURCE).unwrap();
        let p = Programs::new(&ctx, "/nonexistent".into());
        let put = |p: &Programs| {
            for v in [false, true] {
                p.cache.write().unwrap()[usize::from(v)].insert("0abc".into(), Err(Skip::Program));
                p.cache.write().unwrap()[usize::from(v)].insert("0def".into(), Err(Skip::Program));
            }
        };
        let held =
            |p: &Programs, k: &str| p.cache.read().unwrap().iter().any(|m| m.contains_key(k));
        put(&p);
        p.set_swaps(SwapMap::from([("0abc".to_string(), (lib.clone(), 1))]));
        assert!(
            !held(&p, "0abc") && held(&p, "0def"),
            "only the changed key goes"
        );
        put(&p);
        p.set_swaps(SwapMap::from([("0abc".to_string(), (lib.clone(), 1))]));
        assert!(held(&p, "0abc"), "the same swap changes nothing");
        p.set_swaps(SwapMap::from([("0abc".to_string(), (lib, 2))]));
        assert!(!held(&p, "0abc"), "a new number reads again");
        put(&p);
        p.set_swaps(SwapMap::new());
        assert!(!held(&p, "0abc") && held(&p, "0def"), "a reset reads again");
    }
}
