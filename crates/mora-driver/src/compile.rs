//! The passes that check a project and compile it, in their order and with
//! the gates between them (§14.1, §14.2): parsing (§14.4), name resolution
//! and types (§14.5) and the HIR checks, then constant evaluation (§14.6)
//! and the sizes of the types (§13.5) only when no earlier pass reported an
//! error, then code generation for the JIT (§14.13, §14.16).

use mora_codegen::{CodegenError, Cranelift, JitBackend, JitProgram, Profile, Program};
use mora_diag::Diagnostic;
use mora_span::{Db, Project};

/// The diagnostics of `project`, in the order the passes report them.
///
/// The lexical and syntax errors of each file come first, then the errors
/// of name resolution, signatures, types, control flow and entry points;
/// when none of those is an error, the errors of constant evaluation, and
/// when there is still none, those of the sizes of the types the project
/// declares (`MR0156`). The standard library is not loaded yet, so names
/// under `Std`, the names of the prelude and names in a file with a `using`
/// directive of `Std` are not checked when nothing else declares them.
///
/// The passes recurse as deep as the source nests: run it on the compiler's
/// stack ([`crate::on_compiler_stack`]).
pub fn check(db: &dyn Db, project: Project) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for &file in project.files(db) {
        let id = file.id(db);
        let errors = mora_hir::parse(db, file).errors();
        diagnostics.extend(errors.iter().map(|error| error.to_diagnostic(id)));
    }
    diagnostics.extend(mora_hir::diagnostics(db, project).iter().cloned());
    diagnostics.extend(mora_check::diagnostics(db, project).iter().cloned());
    if !diagnostics.iter().any(Diagnostic::is_error) {
        diagnostics.extend(mora_mir::diagnostics(db, project).iter().cloned());
    }
    if !diagnostics.iter().any(Diagnostic::is_error)
        && let Err(error) = mora_codegen::check_sizes_on_host(db, project)
    {
        diagnostics.extend(error.diagnostic());
    }
    diagnostics
}

/// `project` compiled for this process in `profile`, ready to run, when
/// [`check`] reports no error. Run it on the compiler's stack, as
/// [`check`].
///
/// # Errors
///
/// [`BuildError::Program`] with every diagnostic of [`check`] when one of
/// them is an error, or with those and the error of code generation that
/// is the program's own, such as a function whose frame is too large;
/// [`BuildError::Internal`] when code generation fails otherwise.
pub fn jit(db: &dyn Db, project: Project, profile: Profile) -> Result<JitProgram, BuildError> {
    let diagnostics = check(db, project);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(BuildError::Program(diagnostics));
    }
    let program = Program::collect(db, project).map_err(BuildError::Internal)?;
    Cranelift
        .jit(&program, profile)
        .map_err(|error| BuildError::from_codegen(diagnostics, error))
}

/// Why [`jit`] could not build a project.
#[derive(Debug)]
pub enum BuildError {
    /// The program's own errors, which the front end or code generation
    /// reported, with the other diagnostics of the project.
    Program(Vec<Diagnostic>),
    /// A failure of the compiler or of the target.
    Internal(CodegenError),
}

impl BuildError {
    /// The failure of code generation `error` in a project whose checks
    /// reported `diagnostics`: the program's own, after them, when it has a
    /// diagnostic; a failure of the compiler or of the target otherwise.
    fn from_codegen(mut diagnostics: Vec<Diagnostic>, error: CodegenError) -> Self {
        match error.diagnostic() {
            Some(diagnostic) => {
                diagnostics.push(diagnostic);
                BuildError::Program(diagnostics)
            }
            None => BuildError::Internal(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, app, codes, doubling};

    use super::*;

    /// `text` as `src/Program.mr`, alone in an app.
    fn program(db: &TestDb, text: &str) -> mora_span::Project {
        app(db, &[("src/Program.mr", text)])
    }

    #[test]
    fn a_front_end_error_stops_the_check_before_constant_evaluation() {
        let db = TestDb::new();
        let project = program(
            &db,
            "const Zero: int = 0;\nconst Ratio: int = 1 / Zero;\nlet x: int = \"one\";\nreturn 0;\n",
        );
        assert_eq!(codes(&check(&db, project)), ["MR0203"]);
    }

    #[test]
    fn a_failing_constant_is_reported_when_the_front_end_accepts_the_program() {
        let db = TestDb::new();
        let project = program(
            &db,
            "const Zero: int = 0;\nconst Ratio: int = 1 / Zero;\nreturn 0;\n",
        );
        assert_eq!(codes(&check(&db, project)), ["MR0461"]);
    }

    #[test]
    fn a_type_too_large_is_reported_after_the_other_checks() {
        let db = TestDb::new();
        let project = program(&db, &format!("{}return 0;\n", doubling(29)));
        assert_eq!(codes(&check(&db, project)), ["MR0156"]);
    }

    #[test]
    fn an_earlier_error_stops_the_size_check() {
        let db = TestDb::new();
        let project = program(
            &db,
            &format!("{}const Ratio: int = 1 / 0;\nreturn 0;\n", doubling(29)),
        );
        assert_eq!(codes(&check(&db, project)), ["MR0461"]);
    }

    #[test]
    fn the_errors_of_parsing_come_first() {
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                ("src/Program.mr", "let x: int = \"one\";\nreturn 0;\n"),
                ("src/Other.mr", "Broken( {\n"),
            ],
        );
        let other = project.files(&db)[1];
        let parse_errors: Vec<String> = mora_hir::parse(&db, other)
            .errors()
            .iter()
            .map(|error| error.code.to_string())
            .collect();
        assert!(!parse_errors.is_empty());
        let found = codes(&check(&db, project));
        assert_eq!(found[..parse_errors.len()], parse_errors, "{found:?}");
        assert!(found.contains(&"MR0203".to_owned()), "{found:?}");
    }

    #[test]
    fn a_valid_program_runs_and_gives_its_exit_code() {
        let db = TestDb::new();
        let project = program(&db, "let x = 4;\nreturn x - 1;\n");
        let jit = jit(&db, project, Profile::Debug).expect("the program builds");
        assert_eq!(jit.run_main(), 3);
    }

    #[test]
    fn a_program_with_an_error_does_not_build() {
        let db = TestDb::new();
        let project = program(&db, "let x: int = \"one\";\nreturn 0;\n");
        let error = jit(&db, project, Profile::Release).err();
        assert!(
            matches!(&error, Some(BuildError::Program(d)) if codes(d) == ["MR0203"]),
            "{error:?}"
        );
    }

    #[test]
    fn a_hidden_method_beside_a_visible_static_one_is_reported_and_not_built() {
        // The call of the hidden instance method is the one mistake: the
        // static overload may be what it calls, so the call has no type and
        // nothing after it is reported, and the program is not lowered.
        let db = TestDb::new();
        let project = app(
            &db,
            &[
                (
                    "src/Program.mr",
                    "F(s: UsersApi.Store.S): int {\n    let p = s.N();\n    \
                     let q: string = p;\n    return 0;\n}\nreturn 0;\n",
                ),
                (
                    "src/Store/S.mr",
                    "public struct S {\n    public X: int;\n    private N(): int => 1;\n    \
                     public static N(): string => \"\";\n}\n",
                ),
            ],
        );
        assert_eq!(codes(&check(&db, project)), ["MR0129"]);
        let error = jit(&db, project, Profile::Debug).err();
        assert!(
            matches!(&error, Some(BuildError::Program(d)) if codes(d) == ["MR0129"]),
            "{error:?}"
        );
    }

    #[test]
    fn a_frame_too_large_is_the_program_s_error() {
        let db = TestDb::new();
        let project = program(&db, &format!("{}var t: S28;\nreturn 0;\n", doubling(28)));
        let error = jit(&db, project, Profile::Debug).err();
        assert!(
            matches!(&error, Some(BuildError::Program(d)) if codes(d) == ["MR0156"]),
            "{error:?}"
        );
    }

    #[test]
    fn a_failure_of_code_generation_without_a_diagnostic_is_internal() {
        let error = BuildError::from_codegen(Vec::new(), CodegenError::NoEntryPoint);
        assert!(
            matches!(error, BuildError::Internal(CodegenError::NoEntryPoint)),
            "{error:?}"
        );
    }
}
