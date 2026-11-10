//! The diagnostics of memberwise creation: `MR0143`, `MR0147` and `MR0148`.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Span, TextRange};

use super::Source;
use crate::bodies::ExprId;
use crate::inference::NotAField;
use crate::ty::Ty;

/// `MR0143`: `new T { ... }` leaves a field without an initializer
/// unassigned.
pub(super) fn missing_field(source: &Source<'_>, expr: ExprId, ty: &Ty, field: &str) -> Diagnostic {
    let t = source.name(ty);
    Diagnostic::error(
        codes::MR0143,
        format!("`new {t} {{ ... }}` does not assign `{field}`"),
        Label::new(
            source.span(expr),
            format!("`{field}` has no initializer, so it needs a value here"),
        ),
    )
    .with_note("give the field an initializer, or assign it in the `new` expression")
}

/// `MR0147`: `new T { ... }` assigns `value` to `name`, which is `member`
/// rather than an instance field of `T`.
pub(super) fn not_a_field(
    source: &Source<'_>,
    value: ExprId,
    ty: &Ty,
    name: &str,
    member: NotAField,
    suggestion: Option<&str>,
) -> Diagnostic {
    let t = source.name(ty);
    let (message, label) = match member {
        NotAField::Unknown => (
            format!("`{t}` has no field named `{name}`"),
            format!("not a field of `{t}`"),
        ),
        NotAField::Getter => (
            format!("`{name}` is a getter of `{t}` and cannot be assigned"),
            "a getter computes its value".to_owned(),
        ),
        NotAField::Static => (
            format!("`{name}` is a static field of `{t}`, not a field of its values"),
            "belongs to the type".to_owned(),
        ),
        NotAField::Constant => (
            format!("`{name}` is a constant of `{t}`, not a field of its values"),
            "belongs to the type".to_owned(),
        ),
    };
    let span = name_span(source, value);
    let diagnostic = Diagnostic::error(codes::MR0147, message, Label::new(span, label)).with_note(
        format!("`new {t} {{ ... }}` assigns the fields that each value holds"),
    );
    match suggestion {
        Some(suggestion) => diagnostic.with_fix(Fix::new(
            format!("did you mean `{suggestion}`?"),
            vec![TextEdit::replace(span, suggestion)],
        )),
        None => diagnostic,
    }
}

/// `MR0148`: `new T { ... }` assigns the field `name` again. The values
/// are those of its first assignment, of the initializer written just
/// before the repeated one, and of the repeated one.
pub(super) fn repeated_field(
    source: &Source<'_>,
    [first, previous, value]: [ExprId; 3],
    ty: &Ty,
    name: &str,
) -> Diagnostic {
    let t = source.name(ty);
    let diagnostic = Diagnostic::error(
        codes::MR0148,
        format!("`new {t} {{ ... }}` assigns `{name}` twice"),
        Label::new(name_span(source, value), "assigned again here"),
    )
    .with_secondary(Label::new(name_span(source, first), "first assigned here"));
    // From the end of the initializer before it to its own end: the comma
    // and the repeated initializer.
    let (Some(before), Some(repeated)) = (
        source.map.inits.get(&previous),
        source.map.inits.get(&value),
    ) else {
        return diagnostic;
    };
    let removed = TextRange::new(before.init.end(), repeated.init.end());
    diagnostic.with_fix(Fix::new(
        "remove the repeated assignment",
        vec![TextEdit::replace(
            Span::new(source.file.id(source.db), removed),
            "",
        )],
    ))
}

/// Where the name assigned `value` is written, or the value itself when
/// the initializer has no name.
fn name_span(source: &Source<'_>, value: ExprId) -> Span {
    match source.map.inits.get(&value) {
        Some(written) => Span::new(source.file.id(source.db), written.name),
        None => source.span(value),
    }
}
