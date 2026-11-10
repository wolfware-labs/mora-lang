//! The functions of the standard library that never return, and the loops
//! that never end by their condition (D-138).

use crate::{Body, BodyResolution, Expr, ExprId, Literal, Resolved, STD, Target};

/// The names of the standard library's functions of the never type (§11.4),
/// which a path ends at. Until the standard library is loaded they are
/// recognised by name.
const DIVERGING: [&str; 3] = ["panic", "Unreachable", "Todo"];

/// Whether a call to `path` never returns (D-138): its last segment names
/// a function of the never type.
pub fn is_diverging(path: &[String]) -> bool {
    path.last()
        .is_some_and(|name| DIVERGING.contains(&name.as_str()))
}

/// Whether a call of `callee` never returns (D-138): a call of `panic`,
/// `Unreachable` or `Todo` of the standard library, by its name or as
/// `Std.panic`, while the library is not loaded. The call has the never
/// type, and the code after it is not reached.
pub fn calls_diverging(body: &Body, resolution: &BodyResolution, callee: ExprId) -> bool {
    // The path that has to name the standard library: the function's own
    // name, or `Std` before it.
    let (name, path) = match &body.exprs[callee] {
        Expr::Path(path) => match path.segments.as_slice() {
            [name] => (name, callee),
            _ => return false,
        },
        Expr::Member { receiver, name, .. } => match &body.exprs[*receiver] {
            Expr::Path(path) if path.segments == [STD] => (name, *receiver),
            _ => return false,
        },
        _ => return false,
    };
    is_diverging(std::slice::from_ref(name))
        && resolution
            .paths
            .get(&path)
            .is_some_and(|resolution| matches!(resolution.target, Target::Item(Resolved::External)))
}

/// Whether `expr` is the literal `value`: a loop whose condition is `true`
/// ends only by `break`, and one whose condition is `false` after its body
/// does not go back (D-138). Only the literal counts, not a constant.
pub fn is_literal_bool(body: &Body, expr: ExprId, value: bool) -> bool {
    matches!(body.exprs[expr], Expr::Literal(Literal::Bool(literal)) if literal == value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_diverging_matches_d138_names() {
        for name in ["Todo", "Unreachable", "panic"] {
            assert!(is_diverging(&[name.to_owned()]), "{name}");
            assert!(
                is_diverging(&["Std".to_owned(), name.to_owned()]),
                "Std.{name}"
            );
        }
        assert!(!is_diverging(&[
            "Console".to_owned(),
            "WriteLine".to_owned()
        ]));
        assert!(!is_diverging(&["Panic".to_owned()]));
        assert!(!is_diverging(&["panic".to_owned(), "WriteLine".to_owned()]));
        assert!(!is_diverging(&[]));
    }

    #[test]
    fn a_call_diverges_by_its_standard_library_name() {
        use crate::test_db::{TestDb, app, file_named};
        use crate::{BodyOwner, body, body_resolution};

        let db = TestDb::new();
        let text = "Todo();\nStd.panic(\"x\");\nConsole.Todo();\nConsole.WriteLine(1);\nF();\n\
                    Unreachable();\nStd.Max(1, 2);\nF() {}\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let resolution = body_resolution(&db, project, file, BodyOwner::TopLevel);
        let found: Vec<bool> = body
            .exprs
            .iter()
            .filter_map(|(_, expr)| match expr {
                Expr::Call { callee, .. } => Some(calls_diverging(body, resolution, *callee)),
                _ => None,
            })
            .collect();
        // `Std.Max` is of the library too, but returns.
        assert_eq!(found, [true, true, false, false, false, true, false]);
    }

    #[test]
    fn only_a_literal_is_a_literal_bool() {
        use crate::test_db::{TestDb, app, file_named};
        use crate::{BodyOwner, Stmt, body};

        let db = TestDb::new();
        let text = "let a = true;\nlet b = false;\nlet c = !false;\nlet d = 1 < 2;\n";
        let project = app(&db, &[("src/Program.mr", text)]);
        let file = file_named(&db, project, "src/Program.mr");
        let body = body(&db, file, BodyOwner::TopLevel);
        let found: Vec<(bool, bool)> = body
            .stmts
            .iter()
            .filter_map(|(_, stmt)| match stmt {
                Stmt::Local { init, .. } => *init,
                _ => None,
            })
            .map(|init| {
                (
                    is_literal_bool(body, init, true),
                    is_literal_bool(body, init, false),
                )
            })
            .collect();
        assert_eq!(
            found,
            [(true, false), (false, true), (false, false), (false, false)]
        );
    }
}
