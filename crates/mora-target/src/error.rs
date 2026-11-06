use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::builtin::builtin_names;

/// Why a target could not be loaded.
#[derive(Debug)]
pub enum Error {
    /// No built-in target has this name.
    Unknown(String),
    /// The description file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The description does not parse or breaks a rule.
    Invalid {
        /// The file, or the built-in target.
        origin: String,
        /// What is wrong.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unknown(name) => {
                let names: Vec<_> = builtin_names().collect();
                write!(
                    f,
                    "unknown target `{name}`; the built-in targets are {}, and a path to a description ends in `.toml`",
                    names.join(", ")
                )
            }
            Error::Read { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Invalid { origin, message } => write!(f, "{origin}: {}", message.trim_end()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}
