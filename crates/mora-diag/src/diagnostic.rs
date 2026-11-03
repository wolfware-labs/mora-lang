use mora_span::{FileId, Span, TextRange, TextSize};
use serde::{Deserialize, Serialize};

use crate::Code;

/// Whether a diagnostic stops the build (§17.6). It serializes as
/// `"error"` or `"warning"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A span and the text shown under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: Option<String>,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: Some(message.into()),
        }
    }

    pub fn unlabeled(span: Span) -> Self {
        Self {
            span,
            message: None,
        }
    }
}

/// Replaces the text of a span. An empty span inserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub replacement: String,
}

impl TextEdit {
    pub fn replace(span: Span, replacement: impl Into<String>) -> Self {
        Self {
            span,
            replacement: replacement.into(),
        }
    }

    pub fn insert(file: FileId, offset: TextSize, text: impl Into<String>) -> Self {
        Self::replace(Span::new(file, TextRange::empty(offset)), text)
    }
}

/// A change the user can apply, which the LSP offers as a code action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<TextEdit>,
}

impl Fix {
    pub fn new(message: impl Into<String>, edits: Vec<TextEdit>) -> Self {
        Self {
            message: message.into(),
            edits,
        }
    }
}

/// The one diagnostic type every crate reports (§17.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The registered code, from [`crate::codes`].
    pub code: Code,
    /// Whether the diagnostic stops the build.
    pub severity: Severity,
    /// One line, shown after `error[MRnnnn]: `.
    pub message: String,
    /// Where the problem is; the label points at the cause.
    pub primary: Label,
    /// Other places that explain it, such as where a value was moved.
    pub secondary: Vec<Label>,
    /// Extra explanation, shown after the source as `= note:`.
    pub note: Option<String>,
    /// Suggested changes, shown as `help:` with the patched source.
    pub fixes: Vec<Fix>,
}

impl Diagnostic {
    pub fn new(code: Code, severity: Severity, message: impl Into<String>, primary: Label) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            note: None,
            fixes: Vec::new(),
        }
    }

    pub fn error(code: Code, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Error, message, primary)
    }

    pub fn warning(code: Code, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Warning, message, primary)
    }

    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn with_fix(mut self, fix: Fix) -> Self {
        self.fixes.push(fix);
        self
    }
}
