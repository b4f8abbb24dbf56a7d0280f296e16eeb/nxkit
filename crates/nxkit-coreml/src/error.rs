//! The error type of the Core ML runner.

use std::fmt;

use objc2::rc::Retained;
use objc2_foundation::NSError;

/// What went wrong in the Core ML runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreMlError {
    /// A file or directory operation failed (the model cache).
    Io(String),
    /// Core ML did not compile the model package. The text is its own message.
    Compile(String),
    /// Core ML did not load the compiled model.
    Load(String),
    /// The compute plan could not be made.
    Plan(String),
    /// A tensor could not be allocated or read.
    Tensor(String),
    /// A prediction failed. The text is the message of Core ML.
    Predict(String),
    /// The arguments of a call do not fit the model or each other.
    Invalid(String),
}

impl fmt::Display for CoreMlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "model cache: {e}"),
            Self::Compile(e) => write!(f, "Core ML compile failed: {e}"),
            Self::Load(e) => write!(f, "Core ML load failed: {e}"),
            Self::Plan(e) => write!(f, "Core ML compute plan failed: {e}"),
            Self::Tensor(e) => write!(f, "tensor: {e}"),
            Self::Predict(e) => write!(f, "Core ML prediction failed: {e}"),
            Self::Invalid(e) => write!(f, "invalid call: {e}"),
        }
    }
}

impl std::error::Error for CoreMlError {}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, CoreMlError>;

/// The localized text of an `NSError`.
pub(crate) fn ns_text(e: &Retained<NSError>) -> String {
    e.localizedDescription().to_string()
}
