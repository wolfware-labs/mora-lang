//! The [`Compiler`] the harnesses drive, and the stand-ins used until the
//! driver can compile.

use std::fmt::Write;
use std::path::PathBuf;
use std::time::Duration;

use mora_diag::{Diagnostic, Files, Severity};
use mora_span::{FileId, Project, SourceFile};

use crate::case;

/// What the harnesses need from a compiler. The driver implements it once it
/// can compile; the self-tests implement it with a fake.
///
/// A panic in any method fails only the case being run.
pub trait Compiler: Sync {
    /// Compiles `text` as the file `path` and returns what it reported.
    fn check(&self, path: &str, text: &str) -> Checked;

    /// The backends [`Compiler::run`] supports, empty until code generation
    /// exists. A run test goes through each of them.
    fn backends(&self) -> &[Backend];

    /// Builds the program and runs it with `backend`, or returns the
    /// diagnostics that stopped the build.
    fn run(&self, path: &str, text: &str, backend: Backend) -> Result<Output, Checked>;

    /// Compiles `text` as the file `path` and returns the text form of its
    /// HIR, or the diagnostics when it reports an error. A compiler that
    /// cannot compile panics, which fails only the case.
    fn hir(&self, _path: &str, _text: &str) -> Result<String, Checked> {
        unimplemented!("this compiler cannot compile");
    }

    /// Compiles `text` as the file `path` and returns the text form of its
    /// MIR, or the diagnostics when it reports an error. A compiler that
    /// cannot compile panics, which fails only the case.
    fn mir(&self, _path: &str, _text: &str) -> Result<String, Checked> {
        unimplemented!("this compiler cannot compile");
    }
}

/// The compiler the suites and `cargo xtask spec-examples` use. It is
/// [`FrontEnd`] until the driver can compile a file (M1.16).
pub fn compiler() -> &'static dyn Compiler {
    static FRONT_END: FrontEnd = FrontEnd::new();
    &FRONT_END
}

/// A compiler that can do nothing: it has no backend, so run cases fail
/// without calling it, and checking a file panics, which fails only that
/// case. The self-tests use it.
pub struct Unbuilt;

impl Compiler for Unbuilt {
    fn check(&self, _: &str, _: &str) -> Checked {
        unimplemented!("the driver cannot compile yet");
    }

    fn backends(&self) -> &[Backend] {
        &[]
    }

    fn run(&self, _: &str, _: &str, _: Backend) -> Result<Output, Checked> {
        unimplemented!("the driver cannot run programs yet");
    }
}

/// Stands in for the driver until it can compile: checking a case reports
/// its lexical and syntax errors and the errors of name resolution,
/// signatures, types, control flow and entry points, and, when none of those
/// is an error, the errors of constant evaluation, then those of the sizes of
/// the types it declares (`MR0156`). A case may describe several files (see
/// [`crate::case`]); the standard library is not loaded, so names under
/// `Std`, the names of the prelude and names in a file with a `using`
/// directive of `Std` are not checked when nothing else declares them. It
/// has no backend, unless it was given a `jit-runner` executable with
/// [`FrontEnd::with_runner`]: then it runs programs through the JIT in a
/// child process of it.
pub struct FrontEnd {
    /// The `jit-runner` executable, when the front end can run programs.
    runner: Option<PathBuf>,
    /// How long a program may run.
    timeout: Duration,
}

/// How long a program may run before its case fails.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// The analysis database of one check.
#[salsa::db]
#[derive(Clone, Default)]
pub(crate) struct Database {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl mora_span::Db for Database {}

/// One case, loaded into a fresh database.
pub(crate) struct Analysis {
    pub(crate) db: Database,
    pub(crate) project: Project,
    inputs: Vec<SourceFile>,
    /// The paths the files are shown under, in the order of the case.
    shown: Vec<String>,
    pub(crate) checked: Checked,
}

impl Default for FrontEnd {
    fn default() -> Self {
        Self::new()
    }
}

impl FrontEnd {
    /// A front end with no backend.
    pub const fn new() -> FrontEnd {
        FrontEnd {
            runner: None,
            timeout: DEFAULT_RUN_TIMEOUT,
        }
    }

    /// A front end that runs programs through the JIT, each in a child
    /// process started from the `jit-runner` executable `runner`.
    pub fn with_runner(runner: PathBuf) -> FrontEnd {
        FrontEnd {
            runner: Some(runner),
            timeout: DEFAULT_RUN_TIMEOUT,
        }
    }

    /// The same front end with another limit on how long a program runs.
    #[must_use]
    pub fn with_timeout(self, timeout: Duration) -> FrontEnd {
        FrontEnd { timeout, ..self }
    }

    pub(crate) fn analyze(&self, path: &str, text: &str) -> Analysis {
        let case = case::split(path, text).unwrap_or_else(|problem| panic!("{problem}"));
        let namespace = case
            .namespace
            .or_else(|| mora_hir::pascal_case(&case.name))
            .unwrap_or_else(|| panic!("the project name `{}` gives no namespace", case.name));
        let db = Database::default();
        let mut files = Files::new();
        let mut sources = Vec::new();
        let mut inputs = Vec::new();
        let mut shown = Vec::new();
        let mut diagnostics = Vec::new();
        for file in case.files {
            let id = files.add(&file.shown, &file.text);
            let input = SourceFile::new(&db, id, file.path, file.text);
            let parse = mora_hir::parse(&db, input);
            diagnostics.extend(parse.errors().iter().map(|error| error.to_diagnostic(id)));
            sources.push(id);
            inputs.push(input);
            shown.push(file.shown);
        }
        let project = Project::new(
            &db,
            case.name,
            inputs.clone(),
            namespace,
            case.kind,
            case.entry,
            Vec::new(),
        );
        diagnostics.extend(mora_hir::diagnostics(&db, project).iter().cloned());
        diagnostics.extend(mora_check::diagnostics(&db, project).iter().cloned());
        if !has_error(&diagnostics) {
            diagnostics.extend(mora_mir::diagnostics(&db, project).iter().cloned());
        }
        // The sizes of types depend on the target: the host's, which the
        // JIT runs on.
        if !has_error(&diagnostics)
            && let Ok(host) = mora_target::Target::host()
            && let Err(error) = mora_codegen::check_sizes(&db, project, &host)
        {
            diagnostics.extend(error.diagnostic());
        }
        Analysis {
            shown,
            db,
            project,
            inputs,
            checked: Checked {
                files,
                sources,
                diagnostics,
            },
        }
    }
}

/// Whether `diagnostics` holds an error.
pub(crate) fn has_error(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
}

impl Compiler for FrontEnd {
    fn check(&self, path: &str, text: &str) -> Checked {
        self.analyze(path, text).checked
    }

    fn backends(&self) -> &[Backend] {
        if self.runner.is_some() {
            &[Backend::Jit]
        } else {
            &[]
        }
    }

    /// Runs the program in a `jit-runner` process. Panics when the program
    /// runs longer than the limit, when a signal ends it, or when the runner
    /// cannot build it, which fails only the case.
    fn run(&self, path: &str, text: &str, _: Backend) -> Result<Output, Checked> {
        let Some(runner) = &self.runner else {
            unimplemented!("this front end has no runner");
        };
        let analysis = self.analyze(path, text);
        if has_error(&analysis.checked.diagnostics) {
            return Err(analysis.checked);
        }
        Ok(crate::runner::run_child(runner, path, text, self.timeout))
    }

    /// The HIR of each file of the case. A case of several files shows each
    /// under a `//@ file` line with its path.
    fn hir(&self, path: &str, text: &str) -> Result<String, Checked> {
        self.dump(path, text, mora_hir::hir_dump)
    }

    /// The MIR of each file of the case, shown as [`Compiler::hir`] shows
    /// the HIR.
    fn mir(&self, path: &str, text: &str) -> Result<String, Checked> {
        self.dump(path, text, mora_mir::mir_dump)
    }
}

impl FrontEnd {
    /// Analyzes the case and, when it reports no error, joins what `dump`
    /// writes for each of its files.
    fn dump(
        &self,
        path: &str,
        text: &str,
        dump: fn(&dyn mora_span::Db, Project, SourceFile) -> String,
    ) -> Result<String, Checked> {
        let analysis = self.analyze(path, text);
        if has_error(&analysis.checked.diagnostics) {
            return Err(analysis.checked);
        }
        let mut out = String::new();
        let several = analysis.inputs.len() > 1;
        for (file, shown) in analysis.inputs.iter().zip(&analysis.shown) {
            if several {
                // Writing to a `String` cannot fail.
                let _ = writeln!(out, "//@ file {shown}");
            }
            out.push_str(&dump(&analysis.db, analysis.project, *file));
        }
        Ok(out)
    }
}

/// The diagnostics of one compilation and the files they point into.
#[derive(Debug)]
pub struct Checked {
    /// Every file the diagnostics point into.
    pub files: Files,
    /// The files compiled, in the order of the case; their diagnostics are
    /// matched against their annotations.
    pub sources: Vec<FileId>,
    /// What the compiler reported, in the order it reported it.
    pub diagnostics: Vec<Diagnostic>,
}

/// A way of building a program that a run test goes through (§20.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Compiled just in time, in memory, and run at once.
    Jit,
    /// Built ahead of time into an executable that runs as its own process.
    Aot,
}

impl Backend {
    /// The name problems are reported under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Jit => "jit",
            Self::Aot => "aot",
        }
    }
}

/// What a program printed and how it exited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_front_end_with_a_runner_has_a_backend() {
        assert_eq!(FrontEnd::new().backends(), []);
        assert_eq!(FrontEnd::with_runner("x".into()).backends(), [Backend::Jit]);
    }

    #[test]
    fn the_default_front_end_has_no_backend() {
        assert_eq!(FrontEnd::default().backends(), []);
    }

    #[test]
    fn with_timeout_sets_the_limit() {
        let front_end = FrontEnd::new().with_timeout(Duration::from_secs(3));
        assert_eq!(front_end.timeout, Duration::from_secs(3));
        assert_eq!(FrontEnd::new().timeout, DEFAULT_RUN_TIMEOUT);
    }

    #[test]
    #[should_panic(expected = "no runner")]
    fn a_front_end_without_a_runner_cannot_run() {
        let _ = FrontEnd::new().run("a.mr", "let x = 1;\n", Backend::Jit);
    }

    #[test]
    #[should_panic(expected = "cannot compile")]
    fn the_unbuilt_compiler_has_no_hir() {
        let _ = Unbuilt.hir("a.mr", "let x = 1;\n");
    }

    #[test]
    fn a_program_with_an_error_has_no_hir() {
        let checked = FrontEnd::new().hir("a.mr", "let x = y;\n").unwrap_err();
        assert_eq!(checked.diagnostics.len(), 1);
    }

    #[test]
    fn the_hir_of_one_file_has_no_header() {
        let hir = FrontEnd::new().hir("a.mr", "let x = 1;\n").unwrap();
        assert_eq!(hir, "top-level {\n    let x#0: int = 1;\n}\n");
    }

    #[test]
    #[should_panic(expected = "cannot compile")]
    fn the_unbuilt_compiler_has_no_mir() {
        let _ = Unbuilt.mir("a.mr", "let x = 1;\n");
    }

    #[test]
    fn a_program_with_an_error_has_no_mir() {
        let checked = FrontEnd::new().mir("a.mr", "let x = y;\n").unwrap_err();
        assert_eq!(checked.diagnostics.len(), 1);
    }

    #[test]
    fn the_mir_of_several_files_names_each() {
        let text = "//@ file src/Program.mr\nlet x = 1;\n//@ file src/Util.mr\nF() {}\n";
        let mir = FrontEnd::new().mir("a.mr", text).unwrap();
        assert!(mir.starts_with("//@ file src/Program.mr\n"), "{mir}");
        assert!(
            mir.contains("//@ file src/Util.mr\nfn App.F() -> () {"),
            "{mir}"
        );
    }

    #[test]
    fn the_hir_of_several_files_names_each() {
        let text = "//@ file src/Program.mr\nlet x = 1;\n//@ file src/Util.mr\nF() {}\n";
        let hir = FrontEnd::new().hir("a.mr", text).unwrap();
        assert_eq!(
            hir,
            "//@ file src/Program.mr\ntop-level {\n    let x#0: int = 1;\n}\n//@ file src/Util.mr\nfn App.F() {}\n"
        );
    }
}
