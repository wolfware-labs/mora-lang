//! The diagnostics of memberwise creation: `MR0129` for a field it cannot
//! see, `MR0143`, `MR0144`, `MR0147`, `MR0148` and `MR0153`, and `MR0205`
//! for arguments to a type without a constructor.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Span, TextRange};

use super::Source;
use crate::bodies::{BodyKind, BodyOwner, ExprId, TypeId, body, body_owners};
use crate::defs::ItemId;
use crate::inference::NotAField;
use crate::items::Visibility;
use crate::report::{Declared, not_visible};
use crate::resolve::{Scope, item_data};
use crate::ty::Ty;
use crate::type_ref::TypeRef;

/// `MR0143`: the memberwise creation of `T`, `new T { ... }` or `new T()`,
/// leaves a field without an initializer unassigned.
pub(super) fn missing_field(source: &Source<'_>, expr: ExprId, ty: &Ty, field: &str) -> Diagnostic {
    let t = source.name(ty);
    Diagnostic::error(
        codes::MR0143,
        format!("memberwise creation of `{t}` does not assign `{field}`"),
        Label::new(
            source.span(expr),
            format!("`{field}` has no initializer, so it needs a value here"),
        ),
    )
    .with_note("give the field an initializer, or assign it in the `new` expression")
}

/// `MR0144`: memberwise creation of `T` where its field `field`, which has
/// no initializer, is not visible, so no memberwise creation can assign it.
pub(super) fn no_memberwise(
    source: &Source<'_>,
    expr: ExprId,
    ty: &Ty,
    field: &str,
    visibility: Visibility,
) -> Diagnostic {
    let t = source.name(ty);
    Diagnostic::error(
        codes::MR0144,
        format!("`{t}` cannot be created memberwise here"),
        Label::new(
            source.span(expr),
            format!("`{field}` has no initializer and {}", hidden(&t, visibility)),
        ),
    )
    .with_note(format!(
        "memberwise creation needs every field without an initializer visible; create `{t}` through what its type provides"
    ))
}

/// `MR0153`: `new T { ... }`, with the struct `item` written `written`,
/// creates `T` memberwise while it is positional or declares a
/// constructor, which turns memberwise creation off. When one of its
/// constructors takes no parameters and every name in the braces is
/// `writable` after it, the fix calls it; otherwise only the author knows
/// what to write (D-147).
pub(super) fn memberwise_off(
    source: &Source<'_>,
    expr: ExprId,
    item: ItemId,
    written: TypeId,
    writable: bool,
) -> Diagnostic {
    let t = source.name(&Ty::Struct(item));
    let positional = item_data(source.db, item).is_positional;
    let (message, note) = if positional {
        (
            format!("`{t}` is a positional struct, so it is not created memberwise"),
            format!("create it from its positional parameters: `new {t}(...)`"),
        )
    } else {
        (
            format!("`{t}` declares a constructor, so it is not created memberwise"),
            format!(
                "declaring a constructor turns memberwise creation off; call one with `new {t}(...)`"
            ),
        )
    };
    let diagnostic = Diagnostic::error(
        codes::MR0153,
        message,
        Label::new(source.span(expr), format!("memberwise creation of `{t}`")),
    )
    .with_note(note);
    // The parentheses go right after the type.
    let end = match &source.body.types[written] {
        TypeRef::Path(path) => source
            .map
            .type_paths
            .get(path.index)
            .and_then(|segments| segments.last()),
        _ => None,
    };
    match end {
        Some(end) if !positional && writable && has_parameterless_constructor(source, item) => {
            diagnostic.with_fix(Fix::new(
                "call the constructor that takes no parameters",
                vec![TextEdit::insert(source.file.id(source.db), end.end(), "()")],
            ))
        }
        _ => diagnostic,
    }
}

/// Whether the struct `item` declares a constructor that takes no
/// parameters.
fn has_parameterless_constructor(source: &Source<'_>, item: ItemId) -> bool {
    body_owners(source.db, item.file).into_iter().any(|owner| {
        let BodyOwner::Member(ty, _) = owner else {
            return false;
        };
        if ty != item.item {
            return false;
        }
        let hir = body(source.db, item.file, owner);
        hir.kind == BodyKind::Constructor && hir.params.is_empty()
    })
}

/// `MR0129`: memberwise creation, in `from`, assigns `value` to the field
/// `name`, `field`, which the expression cannot see. It reads as any
/// declaration the use cannot see (§4.5).
pub(super) fn hidden_field(
    source: &Source<'_>,
    value: ExprId,
    field: Declared,
    name: &str,
    from: Scope,
) -> Diagnostic {
    not_visible(source.db, from, name, name_span(source, value), field)
}

/// Why a field of visibility `visibility` of the type named `t` is hidden.
fn hidden(t: &str, visibility: Visibility) -> String {
    match visibility {
        Visibility::Private => format!("is private to `{t}`"),
        Visibility::File => format!("is visible only in the file that declares `{t}`"),
        Visibility::Folder => format!("is visible only in the folder that declares `{t}`"),
        Visibility::Internal | Visibility::Public => {
            format!("is visible only in the project that declares `{t}`")
        }
    }
}

/// `MR0147`: the creation of `T`, memberwise when `memberwise` is set and
/// otherwise after a constructor call, assigns `value` to `name`, which is
/// `member` rather than an instance field of `T`.
pub(super) fn not_a_field(
    source: &Source<'_>,
    value: ExprId,
    ty: &Ty,
    name: &str,
    member: NotAField,
    suggestion: Option<&str>,
    memberwise: bool,
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
    let note = if memberwise {
        format!("memberwise creation of `{t}` assigns the fields that each value holds")
    } else {
        format!(
            "the assignments after a constructor call name the fields that each value of `{t}` holds"
        )
    };
    let diagnostic =
        Diagnostic::error(codes::MR0147, message, Label::new(span, label)).with_note(note);
    match suggestion {
        Some(suggestion) => diagnostic.with_fix(Fix::new(
            format!("did you mean `{suggestion}`?"),
            vec![TextEdit::replace(span, suggestion)],
        )),
        None => diagnostic,
    }
}

/// `MR0148`: memberwise creation of `T` assigns the field `name` again.
/// The values are those of its first assignment, of the initializer
/// written just before the repeated one, and of the repeated one.
pub(super) fn repeated_field(
    source: &Source<'_>,
    [first, previous, value]: [ExprId; 3],
    ty: &Ty,
    name: &str,
) -> Diagnostic {
    let t = source.name(ty);
    let diagnostic = Diagnostic::error(
        codes::MR0148,
        format!("memberwise creation of `{t}` assigns `{name}` twice"),
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

/// `MR0205`: `expr` passes arguments to a constructor of `T`, which
/// declares none and is not positional.
pub(super) fn no_constructor(source: &Source<'_>, expr: ExprId, ty: &Ty) -> Diagnostic {
    let t = source.name(ty);
    Diagnostic::error(
        codes::MR0205,
        format!("`{t}` declares no constructor, so `new {t}(...)` takes no arguments"),
        Label::new(source.span(expr), "no constructor takes these arguments"),
    )
    .with_note(format!(
        "a type without a constructor is created memberwise: `new {t} {{ ... }}` names its fields"
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
