//! The `Info.plist` of the app and the version that the game shows.

use super::{Args, Game, bundle};

pub(super) fn plist(a: &Args, t: &Game) -> String {
    let id = a
        .bundle_id
        .clone()
        .unwrap_or_else(|| bundle::default_bundle_id(t.id));
    let shown = shown_version(t);
    bundle::info_plist(
        &bundle::file_name(a.app_name.as_deref().unwrap_or(t.name)),
        &id,
        &shown,
    )
}

/// The version as the game shows it: the display version of the title module, else the number of the update.
pub(super) fn shown_version(t: &Game) -> String {
    t.st.settings
        .value
        .display_versions
        .first()
        .map_or_else(|| t.st.version.to_string(), |v| v.1.to_string())
}
