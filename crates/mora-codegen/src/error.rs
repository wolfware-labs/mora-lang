//! What can stop a build before or during code generation.

use std::fmt;

/// An error from code generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodegenError {
    /// The project has no entry point: it is a library, or an app with neither
    /// top-level statements nor `Main`.
    NoEntryPoint,
    /// The target cannot be generated for.
    UnsupportedTarget {
        /// The target's name.
        target: String,
        /// Why it is not supported.
        reason: String,
    },
    /// Cranelift reported an error.
    Cranelift(String),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEntryPoint => f.write_str("the project has no entry point"),
            Self::UnsupportedTarget { target, reason } => {
                write!(f, "the target `{target}` is not supported: {reason}")
            }
            Self::Cranelift(message) => write!(f, "code generation failed: {message}"),
        }
    }
}

impl std::error::Error for CodegenError {}

#[cfg(test)]
mod tests {
    use super::CodegenError;

    #[test]
    fn errors_display_their_cause() {
        assert_eq!(
            CodegenError::NoEntryPoint.to_string(),
            "the project has no entry point"
        );
        let unsupported = CodegenError::UnsupportedTarget {
            target: "t".into(),
            reason: "r".into(),
        };
        assert_eq!(
            unsupported.to_string(),
            "the target `t` is not supported: r"
        );
        assert_eq!(
            CodegenError::Cranelift("boom".into()).to_string(),
            "code generation failed: boom"
        );
    }
}
