//! Typed errors shared by every stage (layer L0, in V because reason strings reach `_b_build.json`).
//!
//! The classes follow design-v2 section 11:
//!
//! | class | scope | effect |
//! |---|---|---|
//! | [`ErrorClass::Shader`] | one shader | reason string, the build continues |
//! | [`ErrorClass::Tool`] | one job | recovery, then a shader error unless the circuit breaker escalates |
//! | [`ErrorClass::FrontEndReject`] | one shader | Apple fallback, never a failure by itself |
//! | [`ErrorClass::Build`] | the build | `[RED]` line, exit 1 |
//! | [`ErrorClass::Env`] | the machine | abort, no stamp, no reuse key, never retried |
//! | [`ErrorClass::Internal`] | one task | a caught panic: `internal error` for the shader |
//!
//! Every crate keeps its own error enum with a `class()` mapping into [`ErrorClass`]. This crate holds the shared
//! parts.

mod env;
mod tool;

pub use env::{EnvError, EnvKind, IoOp};
pub use tool::{ToolError, ToolFailure};

use nxb_model::{ShaderKey, StageId};
use std::fmt;

/// The error classes of design-v2 11.1.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ErrorClass {
    /// One shader failed, the build continues.
    Shader,
    /// One tool job failed (crash, timeout, non-zero exit).
    Tool,
    /// The own front end declined a shader, Apple's compiler takes it.
    FrontEndReject,
    /// A build-wide problem with a `[RED]` text.
    Build,
    /// A machine problem: disk full, too many open files, toolchain gone.
    Env,
    /// A caught panic or a broken invariant.
    Internal,
}

impl ErrorClass {
    /// Stable lower-case name for events and metrics.
    pub fn name(self) -> &'static str {
        match self {
            ErrorClass::Shader => "shader",
            ErrorClass::Tool => "tool",
            ErrorClass::FrontEndReject => "frontend_reject",
            ErrorClass::Build => "build",
            ErrorClass::Env => "env",
            ErrorClass::Internal => "internal",
        }
    }

    /// True when this class stops the whole run (design-v2 11.2).
    pub fn is_fatal(self) -> bool {
        matches!(self, ErrorClass::Build | ErrorClass::Env)
    }
}

impl fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a shader failed: today's reason strings, byte-exact (they appear in `_b_build.json`).
///
/// The list is completed during the port from the Python chain (responsibility-map section 3.5), unknown texts use
/// [`Reason::Other`] until they get a variant.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Reason {
    /// `translation failed`
    TranslationFailed,
    /// `MSL failed`
    MslFailed,
    /// `metal failed`
    MetalFailed,
    /// `pool rewrite failed`
    PoolRewriteFailed,
    /// `timed out`
    TimedOut,
    /// `internal error`
    Internal,
    /// Any other exact text.
    Other(String),
}

impl Reason {
    /// The exact text.
    pub fn as_str(&self) -> &str {
        match self {
            Reason::TranslationFailed => "translation failed",
            Reason::MslFailed => "MSL failed",
            Reason::MetalFailed => "metal failed",
            Reason::PoolRewriteFailed => "pool rewrite failed",
            Reason::TimedOut => "timed out",
            Reason::Internal => "internal error",
            Reason::Other(s) => s,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One shader's failure. Never stops other shaders.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShaderError {
    /// The shader.
    pub key: ShaderKey,
    /// The reason string.
    pub reason: Reason,
    /// Detail for `explain` and logs (tool stderr head, pass name). Not part of the output contract.
    pub detail: String,
}

impl fmt::Display for ShaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.reason)?;
        if !self.detail.is_empty() {
            write!(f, " ({})", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for ShaderError {}

/// A build-wide problem. Printed as a `[RED] ` line, whose text is a contract with the front ends' classifiers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuildError {
    /// The message after `[RED] `.
    pub message: String,
}

impl BuildError {
    /// A build error with this message.
    pub fn new(message: impl Into<String>) -> Self {
        BuildError {
            message: message.into(),
        }
    }

    /// The exact stdout line: `[RED] <message>`.
    pub fn red_line(&self) -> String {
        format!("[RED] {}", self.message)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BuildError {}

/// What aborts a run (design-v2 11.2): stop scheduling, kill children, publish nothing, exit 1.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fatal {
    /// A build error.
    Build(BuildError),
    /// A machine problem.
    Env(EnvError),
    /// An internal failure that makes the run's result untrustworthy.
    Internal(String),
}

impl Fatal {
    /// The class.
    pub fn class(&self) -> ErrorClass {
        match self {
            Fatal::Build(_) => ErrorClass::Build,
            Fatal::Env(_) => ErrorClass::Env,
            Fatal::Internal(_) => ErrorClass::Internal,
        }
    }

    /// Process exit code: 1 for every fatal error (130 is reserved for an interrupt).
    pub fn exit_code(&self) -> i32 {
        1
    }
}

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fatal::Build(e) => write!(f, "{e}"),
            Fatal::Env(e) => write!(f, "{e}"),
            Fatal::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for Fatal {}

impl From<BuildError> for Fatal {
    fn from(e: BuildError) -> Self {
        Fatal::Build(e)
    }
}

impl From<EnvError> for Fatal {
    fn from(e: EnvError) -> Self {
        Fatal::Env(e)
    }
}

/// A stage-level error for the stages around the shader step (design-pipeline-rest 13).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StageError {
    /// The stage.
    pub stage: StageId,
    /// The class.
    pub class: ErrorClass,
    /// The reason text printed today.
    pub reason: Reason,
    /// Detail for logs.
    pub detail: String,
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.stage, self.class, self.reason)?;
        if !self.detail.is_empty() {
            write!(f, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for StageError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_strings_are_exact() {
        assert_eq!(Reason::TranslationFailed.as_str(), "translation failed");
        assert_eq!(Reason::MslFailed.as_str(), "MSL failed");
        assert_eq!(Reason::MetalFailed.as_str(), "metal failed");
        assert_eq!(Reason::PoolRewriteFailed.as_str(), "pool rewrite failed");
        assert_eq!(Reason::Other("x y".into()).to_string(), "x y");
    }

    #[test]
    fn red_line() {
        assert_eq!(
            BuildError::new("stale header").red_line(),
            "[RED] stale header"
        );
    }

    #[test]
    fn fatal_classes() {
        assert!(ErrorClass::Env.is_fatal());
        assert!(ErrorClass::Build.is_fatal());
        assert!(!ErrorClass::Shader.is_fatal());
        assert_eq!(Fatal::from(BuildError::new("x")).class(), ErrorClass::Build);
    }
}
