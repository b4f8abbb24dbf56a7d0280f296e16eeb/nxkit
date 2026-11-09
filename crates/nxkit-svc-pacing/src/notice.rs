//! What the service tells the host that a person should see (R15).
//!
//! The service has no log of its own. Every fall back, every refused value and every change that the engine writes to its
//! log is a [`Notice`] that a call returns, so nothing happens in silence.

/// One line for the log of the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The line needs attention: a request was refused or a game part was not found. The host prints it as a warning.
    pub warn: bool,
    /// The text.
    pub text: String,
}

impl Notice {
    /// A line that reports what the service did.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            warn: false,
            text: text.into(),
        }
    }

    /// A line that reports a refusal or something missing.
    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            warn: true,
            text: text.into(),
        }
    }
}
