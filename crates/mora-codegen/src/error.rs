//! What can stop a build before or during code generation.

use std::fmt;

use mora_diag::{Diagnostic, Label, codes};
use mora_span::Span;

use crate::layout::MAX_SIZE;

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
    /// A value of the type `name` would take more than 2^31 bytes (§13.3).
    TypeTooLarge {
        /// The type: the innermost struct that is too large, or a tuple no
        /// struct holds.
        name: String,
        /// The struct's name in its declaration, or for a tuple the body
        /// of the function with a local of it.
        span: Span,
    },
}

impl CodegenError {
    /// The diagnostic of an error that is the program's own mistake rather
    /// than the compiler's or the target's: `MR0156` for
    /// [`CodegenError::TypeTooLarge`]. `None` for the others.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let CodegenError::TypeTooLarge { name, span } = self else {
            return None;
        };
        Some(
            Diagnostic::error(
                codes::MR0156,
                format!("a value of `{name}` would take more than {MAX_SIZE} bytes"),
                Label::new(*span, format!("`{name}` is too large")),
            )
            .with_note(format!(
                "a value of any type takes at most 2^31 = {MAX_SIZE} bytes"
            )),
        )
    }
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEntryPoint => f.write_str("the project has no entry point"),
            Self::UnsupportedTarget { target, reason } => {
                write!(f, "the target `{target}` is not supported: {reason}")
            }
            Self::Cranelift(message) => write!(f, "code generation failed: {message}"),
            Self::TypeTooLarge { name, .. } => {
                write!(
                    f,
                    "a value of `{name}` would take more than {MAX_SIZE} bytes"
                )
            }
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
