//! The error of the host. Every failure of the start ends the process with one of these and its message (R15).

use std::fmt;

/// What can stop the host.
#[derive(Debug)]
pub enum HostError {
    /// A file could not be read.
    Io {
        /// The path.
        path: String,
        /// The system message.
        message: String,
    },
    /// `game.json` or a list file is wrong.
    Manifest(String),
    /// The reservation, a mapping or a protection change failed.
    Memory(String),
    /// The game library or a module could not be loaded.
    Load(String),
    /// A module imports a name that nothing provides. The game does not start (R15).
    MissingImport {
        /// The module.
        module: String,
        /// The Switch mangled name.
        name: String,
    },
    /// A call into the system failed.
    Os {
        /// What the host tried.
        what: &'static str,
        /// The `errno` value.
        errno: i32,
    },
    /// A part that another crate provides is not connected yet. See `seams.rs`.
    Unwired(&'static str),
    /// The window or the Metal path failed.
    App(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Io { path, message } => write!(f, "cannot read {path}: {message}"),
            HostError::Manifest(m) => write!(f, "game.json: {m}"),
            HostError::Memory(m) => write!(f, "guest memory: {m}"),
            HostError::Load(m) => write!(f, "loader: {m}"),
            HostError::MissingImport { module, name } => {
                write!(f, "module {module} imports {name} and nothing provides it")
            }
            HostError::Os { what, errno } => write!(f, "{what} failed, errno {errno}"),
            HostError::Unwired(what) => write!(f, "not connected yet: {what} (see seams.rs)"),
            HostError::App(m) => write!(f, "window: {m}"),
        }
    }
}

impl std::error::Error for HostError {}

impl HostError {
    /// An `Io` error for `path`.
    pub fn io(path: &std::path::Path, e: &std::io::Error) -> Self {
        HostError::Io {
            path: path.display().to_string(),
            message: e.to_string(),
        }
    }

    /// An `Os` error with the current `errno`.
    pub fn os(what: &'static str) -> Self {
        HostError::Os {
            what,
            errno: std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        }
    }
}

/// The result type of the host.
pub type Result<T> = std::result::Result<T, HostError>;
