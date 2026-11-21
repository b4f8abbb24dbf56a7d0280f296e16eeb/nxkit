//! The readers of a JSON field of the run folder (`game.json`, the modules, the settings): each names the file and the
//! key in its error. The readers of those files share them, so they sit below all three.

use crate::json::Value;

use crate::error::{HostError, Result};

/// A manifest error with this text.
pub(crate) fn bad(m: impl Into<String>) -> HostError {
    HostError::Manifest(m.into())
}

pub(crate) fn field<'a>(v: &'a Value, key: &str, ctx: &str) -> Result<&'a Value> {
    v.get(key)
        .ok_or_else(|| bad(format!("{ctx}: missing \"{key}\"")))
}

pub(crate) fn num(v: &Value, key: &str, ctx: &str) -> Result<u64> {
    field(v, key, ctx)?
        .as_u64()
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a number")))
}

pub(crate) fn text(v: &Value, key: &str, ctx: &str) -> Result<String> {
    field(v, key, ctx)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad(format!("{ctx}: \"{key}\" is not a string")))
}
