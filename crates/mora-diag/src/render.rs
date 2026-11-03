use annotate_snippets::{AnnotationKind, Group, Level, Patch, Renderer, Snippet};
use mora_span::{FileId, Span};

use crate::sources::clamp;
use crate::{Diagnostic, Severity, Sources};

/// Renders a diagnostic in the terminal layout of §17.6, with ANSI colors
/// when `color` is true. The text ends without a newline.
pub fn render(diagnostic: &Diagnostic, sources: &dyn Sources, color: bool) -> String {
    let renderer = if color {
        Renderer::styled()
    } else {
        Renderer::plain()
    };
    let level = match diagnostic.severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
    };
    let code = diagnostic.code.to_string();

    let mut main = Group::with_title(
        level
            .primary_title(diagnostic.message.as_str())
            .id(code.as_str()),
    );
    let spans = std::iter::once(&diagnostic.primary)
        .chain(&diagnostic.secondary)
        .map(|label| label.span);
    for file in distinct_files(spans) {
        let labels = std::iter::once((&diagnostic.primary, AnnotationKind::Primary))
            .chain(
                diagnostic
                    .secondary
                    .iter()
                    .map(|l| (l, AnnotationKind::Context)),
            )
            .filter(|(label, _)| label.span.file == file);
        let text = sources.text(file);
        let annotations = labels.map(|(label, kind)| {
            kind.span(clamp(text, label.span))
                .label(label.message.as_deref())
        });
        main = main.element(
            Snippet::source(text)
                .path(sources.path(file))
                .annotations(annotations),
        );
    }
    if let Some(note) = &diagnostic.note {
        main = main.element(Level::NOTE.message(note.as_str()));
    }

    let mut report: Vec<Group<'_>> = vec![main];
    for fix in &diagnostic.fixes {
        let mut group = Group::with_title(Level::HELP.secondary_title(fix.message.as_str()));
        for file in distinct_files(fix.edits.iter().map(|edit| edit.span)) {
            let text = sources.text(file);
            let patches = fix
                .edits
                .iter()
                .filter(|edit| edit.span.file == file)
                .map(|edit| Patch::new(clamp(text, edit.span), edit.replacement.as_str()));
            group = group.element(
                Snippet::source(text)
                    .path(sources.path(file))
                    .patches(patches),
            );
        }
        report.push(group);
    }
    renderer.render(&report)
}

/// The files of `spans`, each once, in the order they first appear. A
/// diagnostic touches a handful of files, so a linear search is enough.
fn distinct_files(spans: impl IntoIterator<Item = Span>) -> Vec<FileId> {
    let mut files = Vec::new();
    for span in spans {
        if !files.contains(&span.file) {
            files.push(span.file);
        }
    }
    files
}
