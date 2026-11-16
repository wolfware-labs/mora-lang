//! The diagnostics of constant evaluation (§14.6): an operation a constant
//! cannot run (`MR0460`), an operation that fails (`MR0461`) and a value
//! that depends on itself (`MR0462`).

use mora_diag::{Diagnostic, Label, codes};
use mora_hir::ty_name;
use mora_span::{Db, Project, Span};

use super::arith::ArithError;
use super::graph::const_graph;
use super::interp::Failure;
use super::{ConstError, ConstKey, ErrorKind, Evaluated, const_values, expect_evaluating};

/// The label of `MR0460` and `MR0461`.
const EVALUATED: &str = "evaluated at compile time";

/// The note of `MR0460`.
const UNSUPPORTED_NOTE: &str = "constant evaluation runs literals, operators, casts, other constants and the creation of structs and tuples; a value computed when the program runs can be a `static lazy`";

/// The note of `MR0461`.
const FAILED_NOTE: &str = "a constant is computed when the program is compiled, where an operation that would panic at run time is an error in every build profile";

/// The errors of constant evaluation in `project` (`MR0460`, `MR0461`,
/// `MR0462`), from what its pass ([`const_values`]) gives: those of the
/// keys of its read graph, in the order of the graph, then those of the
/// initializers of other projects that the pass evaluated itself, in the
/// order it did. It runs on a program the front end accepted.
///
/// A key that fails because a key it reads has no value reports nothing,
/// so each cause is reported once.
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, project: Project) -> Vec<Diagnostic> {
    let values = const_values(db, project);
    const_graph(db, project)
        .order()
        .iter()
        .chain(values.extras())
        .filter_map(|&key| match values.get(key) {
            Some(Evaluated::Failed(Some(error))) => Some(render(db, project, error)),
            Some(Evaluated::Value(_) | Evaluated::Failed(None) | Evaluated::Later(_)) | None => {
                None
            }
        })
        .collect()
}

/// The diagnostic of `error`, reported by `project`, labelled at its range
/// in the file of its key; its keys are named as the project that declares
/// them names them.
fn render(db: &dyn Db, project: Project, error: &ConstError) -> Diagnostic {
    // Only an initializer of another project is evaluated by a project
    // other than its own, and the keys of a cycle are of one project.
    let project = match error.key {
        ConstKey::Initializer { item, .. } => item.project,
        ConstKey::Constant { .. } | ConstKey::Default { .. } => project,
    };
    let file = error.key.file();
    let span = Span::new(file.id(db), error.range);
    match &error.kind {
        ErrorKind::Unsupported(what) => {
            let message = format!("{what} cannot be evaluated at compile time");
            let note = match error.key {
                ConstKey::Initializer { .. } => {
                    let name = error.key.name(db, project);
                    let split = name.rsplit_once('.');
                    let (ty, field) =
                        expect_evaluating(split, || name.clone(), "a field without its type");
                    format!(
                        "{UNSUPPORTED_NOTE}\n`{name}` runs at compile time because a `{ty}` created at compile time leaves `{field}` out"
                    )
                }
                ConstKey::Constant { .. } | ConstKey::Default { .. } => UNSUPPORTED_NOTE.to_owned(),
            };
            Diagnostic::error(codes::MR0460, message, Label::new(span, EVALUATED)).with_note(note)
        }
        ErrorKind::Failed(failure) => {
            let code = &file.text(db)[std::ops::Range::<usize>::from(error.range)];
            let message = failed_message(code, failure, &ty_name(db, &failure.ty));
            Diagnostic::error(codes::MR0461, message, Label::new(span, EVALUATED))
                .with_note(FAILED_NOTE)
        }
        ErrorKind::Cycle(members) => {
            let names: Vec<String> = members.iter().map(|key| key.name(db, project)).collect();
            let first = &names[0];
            // A default value is never read, so a cycle never holds one.
            let message = match error.key {
                ConstKey::Constant { .. } => format!("constant `{first}` depends on its own value"),
                ConstKey::Initializer { .. } | ConstKey::Default { .. } => {
                    format!("the initializer of `{first}` depends on its own value")
                }
            };
            let read = names.get(1).unwrap_or(first);
            Diagnostic::error(
                codes::MR0462,
                message,
                Label::new(span, format!("this needs the value of `{read}`")),
            )
            .with_note(format!("the cycle is {} -> {first}", names.join(" -> ")))
        }
    }
}

/// The message of `MR0461` for `failure` of the code `code`, whose result
/// has the type named `ty`.
pub(super) fn failed_message(code: &str, failure: &Failure, ty: &str) -> String {
    let operation = &failure.operation;
    match failure.error {
        ArithError::Overflow => format!("`{code}` overflows `{ty}`: {operation}"),
        ArithError::DivisionByZero => format!("`{code}` divides by zero: {operation}"),
        ArithError::RemainderByZero => {
            format!("`{code}` takes a remainder of a division by zero: {operation}")
        }
        ArithError::TargetDependent => {
            format!("`{code}` gives a `{ty}` that is not the same on every target: {operation}")
        }
        ArithError::TooLong(bytes) => {
            format!("`{code}` makes a string of {bytes} bytes, more than a constant can hold")
        }
    }
}
