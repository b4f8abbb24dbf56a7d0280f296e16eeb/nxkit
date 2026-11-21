//! The saved choices of the player: the strings `launcher.<env>` in the preferences domain of the app, its bundle
//! identifier, as a launcher writes them (`nxkit_abi::launch`).

use objc2_core_foundation::{CFBundle, CFPreferencesCopyAppValue, CFRetained, CFString};

/// The preferences domain of the app: the identifier of the main bundle. None when the host does not run from an
/// app bundle (a run from the command line), which has no saved choices.
pub(crate) struct Prefs {
    domain: CFRetained<CFString>,
}

impl Prefs {
    /// The domain of the main bundle.
    pub(crate) fn of_app() -> Option<Prefs> {
        CFBundle::main_bundle()
            .identifier()
            .map(|domain| Prefs { domain })
    }

    /// The value of `key`, when it is a string.
    pub(crate) fn string(&self, key: &str) -> Option<String> {
        let v = CFPreferencesCopyAppValue(&CFString::from_str(key), &self.domain)?;
        v.downcast_ref::<CFString>().map(|s| s.to_string())
    }
}
