//! The settings of the Unreal take-overs from the `NXKIT_*` environment (W27). The host builds them once from its
//! snapshot ([`UeSettings::from_env`]) and passes them to [`crate::wire`].

use nxkit_abi::env::Env;

/// The settings of `nxkit-ue`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UeSettings {
    /// `NXKIT_UE_HEAP` as given: the arena of the engine heap at the end of the alias region, in place of 40 % of it (a
    /// size as `nxkit_abi::env::parse_size` reads it).
    pub heap: Option<String>,
}

impl UeSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        UeSettings {
            heap: env.var("NXKIT_UE_HEAP").ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heap_size_is_kept_as_given() {
        assert_eq!(UeSettings::from_env(&Env::default()).heap, None);
        let s = UeSettings::from_env(&Env::from_pairs([("NXKIT_UE_HEAP", "512M")]));
        assert_eq!(s.heap.as_deref(), Some("512M"));
    }
}
