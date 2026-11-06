//! The [`Compiler`] the harnesses drive, and the stand-ins used until the
//! driver can compile.

use std::fmt::Write;

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
}

/// The compiler the suites and `cargo xtask spec-examples` use. It is
/// [`FrontEnd`] until the driver can compile a file (M1.16).
pub fn compiler() -> &'static dyn Compiler {
    &FrontEnd
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
/// signatures and entry points. A case may describe several files (see
/// [`crate::case`]); the standard library is not loaded, so names under
/// `Std`, the names of the prelude and names in a file with a `using`
/// directive of `Std` are not checked when nothing else declares them. It
/// has no backend.
pub struct FrontEnd;

/// The analysis database of one check.
#[salsa::db]
#[derive(Clone, Default)]
struct Database {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl mora_span::Db for Database {}

/// One case, loaded into a fresh database.
struct Analysis {
    db: Database,
    project: Project,
    inputs: Vec<SourceFile>,
    /// The paths the files are shown under, in the order of the case.
    shown: Vec<String>,
    checked: Checked,
}

impl FrontEnd {
    fn analyze(&self, path: &str, text: &str) -> Analysis {
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

impl Compiler for FrontEnd {
    fn check(&self, path: &str, text: &str) -> Checked {
        self.analyze(path, text).checked
    }

    fn backends(&self) -> &[Backend] {
        &[]
    }

    fn run(&self, _: &str, _: &str, _: Backend) -> Result<Output, Checked> {
        unimplemented!("the driver cannot run programs yet");
    }

    /// The HIR of each file of the case. A case of several files shows each
    /// under a `//@ file` line with its path.
    fn hir(&self, path: &str, text: &str) -> Result<String, Checked> {
        let analysis = self.analyze(path, text);
        let failed = analysis
            .checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error);
        if failed {
            return Err(analysis.checked);
        }
        let mut out = String::new();
        let several = analysis.inputs.len() > 1;
        for (file, shown) in analysis.inputs.iter().zip(&analysis.shown) {
            if several {
                // Writing to a `String` cannot fail.
                let _ = writeln!(out, "//@ file {shown}");
            }
            out.push_str(&mora_hir::hir_dump(&analysis.db, analysis.project, *file));
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
    pub exit: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "cannot compile")]
    fn the_unbuilt_compiler_has_no_hir() {
        let _ = Unbuilt.hir("a.mr", "let x = 1;\n");
    }

    #[test]
    fn a_program_with_an_error_has_no_hir() {
        let checked = FrontEnd.hir("a.mr", "let x = y;\n").unwrap_err();
        assert_eq!(checked.diagnostics.len(), 1);
    }

    #[test]
    fn the_hir_of_one_file_has_no_header() {
        let hir = FrontEnd.hir("a.mr", "let x = 1;\n").unwrap();
        assert_eq!(hir, "top-level {\n    let x#0 = 1;\n}\n");
    }

    #[test]
    fn the_hir_of_several_files_names_each() {
        let text = "//@ file src/Program.mr\nlet x = 1;\n//@ file src/Util.mr\nF() {}\n";
        let hir = FrontEnd.hir("a.mr", text).unwrap();
        assert_eq!(
            hir,
            "//@ file src/Program.mr\ntop-level {\n    let x#0 = 1;\n}\n//@ file src/Util.mr\nfn App.F() {}\n"
        );
    }
}
