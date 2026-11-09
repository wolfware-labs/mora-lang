//! The diagnostic of memberwise creation: `MR0143`.

use mora_diag::{Diagnostic, Label, codes};

use super::Source;
use crate::bodies::ExprId;
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
