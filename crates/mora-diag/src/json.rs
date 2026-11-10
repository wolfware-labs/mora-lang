use mora_span::{FileId, LineIndex, Span, to_text_size};
use serde::{Deserialize, Serialize};

use crate::sources::clamp;
use crate::{Code, Diagnostic, Label, Severity, Sources};

/// A diagnostic as `--message-format json` prints it: one object per line,
/// with every field of §17.6 and spans resolved to paths, lines and columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonDiagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    pub primary: JsonLabel,
    pub secondary: Vec<JsonLabel>,
    pub note: Option<String>,
    pub fixes: Vec<JsonFix>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonLabel {
    pub span: JsonSpan,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonFix {
    pub message: String,
    pub edits: Vec<JsonEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonEdit {
    pub span: JsonSpan,
    pub replacement: String,
}

/// A span with byte offsets for tools that edit text, and one-based lines
/// and columns, counted in characters, as the terminal shows them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonSpan {
    pub file: String,
    pub byte_start: u32,
    pub byte_end: u32,
    pub line_start: u32,
    pub column_start: u32,
    pub line_end: u32,
    pub column_end: u32,
}

impl JsonDiagnostic {
    pub fn new(diagnostic: &Diagnostic, sources: &dyn Sources) -> Self {
        let label = |label: &Label| JsonLabel {
            span: JsonSpan::new(label.span, sources),
            label: label.message.clone(),
        };
        Self {
            code: diagnostic.code,
            severity: diagnostic.severity,
            message: diagnostic.message.clone(),
            primary: label(&diagnostic.primary),
            secondary: diagnostic.secondary.iter().map(label).collect(),
            note: diagnostic.note.clone(),
            fixes: diagnostic
                .fixes
                .iter()
                .map(|fix| JsonFix {
                    message: fix.message.clone(),
                    edits: fix
                        .edits
                        .iter()
                        .map(|edit| JsonEdit {
                            span: JsonSpan::new(edit.span, sources),
                            replacement: edit.replacement.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl JsonSpan {
    fn new(span: Span, sources: &dyn Sources) -> Self {
        let range = clamp(sources.text(span.file), span);
        let (line_start, column_start) = position(sources, span.file, range.start);
        let (line_end, column_end) = position(sources, span.file, range.end);
        Self {
            file: sources.path(span.file).to_owned(),
            byte_start: to_text_size(range.start).into(),
            byte_end: to_text_size(range.end).into(),
            line_start,
            column_start,
            line_end,
            column_end,
        }
    }
}

/// One-based line and character column of a byte offset on a char boundary.
fn position(sources: &dyn Sources, file: FileId, offset: usize) -> (u32, u32) {
    line_column(sources.text(file), sources.line_index(file), offset).unwrap_or((1, 1))
}

/// The one-based line and the one-based column of a byte offset of `text`,
/// counted in characters as the terminal shows them. `None` when the offset
/// is past the end of the text or inside a character; `lines` is the line
/// index of `text`.
pub fn line_column(text: &str, lines: &LineIndex, offset: usize) -> Option<(u32, u32)> {
    let pos = lines.line_col(to_text_size(offset))?;
    let line_start = offset - pos.col as usize;
    let chars = text
        .get(line_start..offset)
        .map_or(0, |prefix| prefix.chars().count());
    let column = u32::try_from(chars).unwrap_or(u32::MAX);
    Some((pos.line.saturating_add(1), column.saturating_add(1)))
}

/// One JSON object for `--message-format json`, without a trailing newline.
pub fn to_json_line(diagnostic: &Diagnostic, sources: &dyn Sources) -> String {
    let json = JsonDiagnostic::new(diagnostic, sources);
    // Serializing plain structs of strings and integers cannot fail.
    serde_json::to_string(&json).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_characters_and_lines_start_at_one() {
        let text = "ab\n\u{e9}\u{1F600}x\n";
        let lines = LineIndex::new(text);
        let at = |needle: &str| line_column(text, &lines, text.find(needle).unwrap());
        assert_eq!(line_column(text, &lines, 0), Some((1, 1)));
        assert_eq!(at("b"), Some((1, 2)));
        assert_eq!(at("x"), Some((2, 3)));
        assert_eq!(at("\u{1F600}"), Some((2, 2)));
        // Inside a character, and past the end.
        assert_eq!(line_column(text, &lines, 4), None);
        assert_eq!(line_column(text, &lines, text.len() + 1), None);
    }
}
