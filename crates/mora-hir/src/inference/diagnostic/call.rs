//! The diagnostics of calls: `MR0205` to `MR0208`.

use mora_diag::{Diagnostic, Fix, Label, TextEdit, codes};
use mora_span::{Span, TextRange, TextSize};

use super::Source;
use crate::bodies::{ArgMode, ExprId};
use crate::inference::{Callee, NamedProblem};
use crate::report::item_name_span;
use crate::ty::{ParamMode, Ty};

/// The keyword of a parameter mode or an argument marker.
fn keyword(mode: ArgMode) -> &'static str {
    match mode {
        ArgMode::Ref => "ref",
        ArgMode::In => "in",
        ArgMode::Move => "move",
    }
}

/// `MR0205`: a call of one declaration with an argument too many, or
/// without one for a parameter that has no default.
pub(super) fn arity(
    source: &Source<'_>,
    expr: ExprId,
    signature: &str,
    missing: Option<&str>,
) -> Diagnostic {
    match missing {
        Some(param) => Diagnostic::error(
            codes::MR0205,
            format!("missing an argument for `{param}` in this call of `{signature}`"),
            Label::new(source.span(expr), format!("`{param}` has no default value")),
        )
        .with_note("every parameter without a default value takes an argument"),
        None => Diagnostic::error(
            codes::MR0205,
            format!("too many arguments in this call of `{signature}`"),
            Label::new(source.span(expr), "no parameter takes this argument"),
        ),
    }
}

/// `MR0205`: an argument whose marker does not suit its parameter's mode
/// (§10.3), with the fix that writes the marker it needs.
pub(super) fn mode(
    source: &Source<'_>,
    arg: ExprId,
    signature: &str,
    param: &str,
    needs: ParamMode,
    given: Option<ArgMode>,
) -> Diagnostic {
    let span = source.span(arg);
    let file = source.file.id(source.db);
    // The marker written before the argument, and where it is.
    let marker = given.and_then(|given| {
        let text = source.file.text(source.db);
        let start = usize::from(span.range.start());
        let before = text.get(..start)?.trim_end();
        let word = keyword(given);
        before.ends_with(word).then(|| {
            let from = TextSize::try_from(before.len() - word.len()).unwrap_or_default();
            (word, TextRange::new(from, span.range.start()))
        })
    });
    let (message, label, fix) = match (needs, given) {
        (ParamMode::Ref, None) => (
            format!("the argument for `ref {param}` in `{signature}` must be marked `ref`"),
            "pass it as `ref`".to_owned(),
            Some(Fix::new(
                "mark it `ref`",
                vec![TextEdit::insert(file, span.range.start(), "ref ")],
            )),
        ),
        (ParamMode::Ref, Some(given)) => (
            format!(
                "the argument for `ref {param}` in `{signature}` is marked `{}`, not `ref`",
                keyword(given)
            ),
            "pass it as `ref`".to_owned(),
            marker.map(|(word, range)| {
                let at = TextRange::at(range.start(), TextSize::of(word));
                Fix::new(
                    "mark it `ref`",
                    vec![TextEdit::replace(Span::new(file, at), "ref")],
                )
            }),
        ),
        (_, given) => {
            let word = given.map_or("", keyword);
            (
                format!(
                    "the parameter `{param}` of `{signature}` does not take an argument marked `{word}`"
                ),
                format!("remove `{word}`"),
                marker.map(|(word, range)| {
                    Fix::new(
                        format!("remove `{word}`"),
                        vec![TextEdit::replace(Span::new(file, range), "")],
                    )
                }),
            )
        }
    };
    let diagnostic = Diagnostic::error(codes::MR0205, message, Label::new(span, label))
        .with_note("`ref` before an argument passes the place itself; it is written exactly for a `ref` parameter, and `in` or `move` only for a parameter of that mode");
    match fix {
        Some(fix) => diagnostic.with_fix(fix),
        None => diagnostic,
    }
}

/// The candidates of a call, each as a label at its declaration.
fn with_candidates(
    source: &Source<'_>,
    mut diagnostic: Diagnostic,
    candidates: &[(Callee, String)],
) -> Diagnostic {
    for (callee, signature) in candidates {
        let span = match callee {
            Callee::Item(item) => item_name_span(source.db, *item),
            Callee::Local(local) => Span::new(source.file.id(source.db), source.map.local(*local)),
        };
        diagnostic = diagnostic.with_secondary(Label::new(span, format!("`{signature}`")));
    }
    diagnostic
}

/// `MR0205`: no overload takes the arguments of a call.
pub(super) fn no_overload(
    source: &Source<'_>,
    expr: ExprId,
    name: &str,
    candidates: &[(Callee, String)],
) -> Diagnostic {
    let diagnostic = Diagnostic::error(
        codes::MR0205,
        format!("no overload of `{name}` takes these arguments"),
        Label::new(
            source.span(expr),
            format!("none of the {} overloads fits", candidates.len()),
        ),
    )
    .with_note(
        "an overload fits when every argument goes to a parameter, by position or by name, \
         converts to its type and has the marker its mode needs",
    );
    with_candidates(source, diagnostic, candidates)
}

/// `MR0206`: more than one overload fits a call equally well.
pub(super) fn ambiguous(
    source: &Source<'_>,
    expr: ExprId,
    name: &str,
    candidates: &[(Callee, String)],
) -> Diagnostic {
    let diagnostic = Diagnostic::error(
        codes::MR0206,
        format!("the call of `{name}` is ambiguous"),
        Label::new(
            source.span(expr),
            format!("{} overloads fit equally well", candidates.len()),
        ),
    )
    .with_note("convert an argument explicitly, as with a cast `(T)x`, to choose one");
    with_candidates(source, diagnostic, candidates)
}

/// `MR0207`: a named argument that names no parameter or one already
/// given, a name given twice, or a positional argument after a named one.
pub(super) fn named(source: &Source<'_>, arg: ExprId, problem: &NamedProblem) -> Diagnostic {
    let span = source.span(arg);
    let (message, label) = match problem {
        NamedProblem::AfterNamed => (
            "a positional argument after a named one".to_owned(),
            "name this argument too, or move it before the named ones",
        ),
        NamedProblem::Duplicate { name } => (
            format!("the argument `{name}` is named twice"),
            "named again here",
        ),
        NamedProblem::Unknown { name, function } => (
            format!("`{function}` has no parameter named `{name}`"),
            "no parameter of this name",
        ),
        NamedProblem::AlreadyGiven { name } => (
            format!("the parameter `{name}` is given by position and by name"),
            "given by name here",
        ),
    };
    Diagnostic::error(codes::MR0207, message, Label::new(span, label)).with_note(
        "arguments come by position first; after the first named one, every argument is named, each name once",
    )
}

/// `MR0208`: a call of a value whose type is not a function type.
pub(super) fn not_callable(source: &Source<'_>, expr: ExprId, ty: &Ty) -> Diagnostic {
    let t = source.name(ty);
    Diagnostic::error(
        codes::MR0208,
        format!("cannot call a value of type `{t}`"),
        Label::new(source.span(expr), format!("this is `{t}`, not a function")),
    )
    .with_note("only a function, a method or a value of a function type can be called")
}
