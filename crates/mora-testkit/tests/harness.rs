#![allow(clippy::unwrap_used)]
// Helpers below unwrap too, and `allow-unwrap-in-tests` covers only test functions.

//! Self-tests of the harnesses, with a fake compiler.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mora_diag::{Code, Diagnostic, Files, Label};
use mora_span::{Span, TextRange, TextSize};
use mora_testkit::harness::{Options, Report, run_suite, ui_suite};
use mora_testkit::{Backend, Checked, Compiler, Output};

/// Compiles a toy language, one statement per line, with `//` comments:
/// `fail MRnnnn` reports an error on the word `fail`, `warn MRnnnn` a
/// warning, `print text` prints a line, `jit-print text` prints it under the
/// JIT only, `exit n` sets the exit code, `hir text` adds a line to the HIR
/// and `panic` crashes the compiler.
struct Fake {
    backends: &'static [Backend],
}

const BOTH: Fake = Fake {
    backends: &[Backend::Jit, Backend::Aot],
};

fn statements(text: &str) -> impl Iterator<Item = (usize, &str, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').filter_map(move |line| {
        let start = offset;
        offset += line.len();
        let code = line.split("//").next().unwrap_or("");
        let indent = code.len() - code.trim_start().len();
        let (word, rest) = code.trim().split_once(' ').unwrap_or((code.trim(), ""));
        (!word.is_empty()).then_some((start + indent, word, rest.trim()))
    })
}

impl Compiler for Fake {
    fn check(&self, path: &str, text: &str) -> Checked {
        let mut files = Files::new();
        let file = files.add(path, text);
        let mut diagnostics = Vec::new();
        for (at, word, rest) in statements(text) {
            let span = Span::new(
                file,
                TextRange::at(TextSize::new(at as u32), TextSize::of(word)),
            );
            let code = || rest.parse::<Code>().unwrap();
            match word {
                "fail" => diagnostics.push(Diagnostic::error(
                    code(),
                    "fake failure",
                    Label::new(span, "here"),
                )),
                "warn" => diagnostics.push(Diagnostic::warning(
                    code(),
                    "fake warning",
                    Label::unlabeled(span),
                )),
                "panic" => panic!("the fake compiler crashed"),
                _ => {}
            }
        }
        Checked {
            files,
            sources: vec![file],
            diagnostics,
        }
    }

    fn backends(&self) -> &[Backend] {
        self.backends
    }

    fn run(&self, path: &str, text: &str, backend: Backend) -> Result<Output, Checked> {
        let checked = self.check(path, text);
        if !checked.diagnostics.is_empty() {
            return Err(checked);
        }
        let mut output = Output {
            stdout: String::new(),
            exit: 0,
        };
        for (_, word, rest) in statements(text) {
            match word {
                "print" => {
                    output.stdout.push_str(rest);
                    output.stdout.push('\n');
                }
                "jit-print" if backend == Backend::Jit => {
                    output.stdout.push_str(rest);
                    output.stdout.push('\n');
                }
                "exit" => output.exit = rest.parse().unwrap(),
                _ => {}
            }
        }
        Ok(output)
    }

    fn hir(&self, path: &str, text: &str) -> Result<String, Checked> {
        let checked = self.check(path, text);
        if checked
            .diagnostics
            .iter()
            .any(|d| d.severity == mora_diag::Severity::Error)
        {
            return Err(checked);
        }
        Ok(statements(text)
            .filter(|&(_, word, _)| word == "hir")
            .map(|(_, _, rest)| format!("{rest}\n"))
            .collect())
    }

    fn mir(&self, path: &str, text: &str) -> Result<String, Checked> {
        let checked = self.check(path, text);
        if checked
            .diagnostics
            .iter()
            .any(|d| d.severity == mora_diag::Severity::Error)
        {
            return Err(checked);
        }
        Ok(statements(text)
            .filter(|&(_, word, _)| word == "mir")
            .map(|(_, _, rest)| format!("{rest}\n"))
            .collect())
    }
}

/// A fresh directory with `files` written below `tests/<suite>`.
fn suite_dir(name: &str, suite: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("tests").join(suite);
    fs::create_dir_all(&dir).unwrap();
    for (path, text) in files {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    root
}

fn ui(root: &Path, options: &Options) -> Report {
    ui_suite(&BOTH, root, "tests/ui", options).unwrap()
}

fn run(compiler: &Fake, root: &Path) -> Report {
    run_with(compiler, root, &Options::default())
}

fn run_with(compiler: &Fake, root: &Path, options: &Options) -> Report {
    run_suite(compiler, root, "tests/run", options).unwrap()
}

fn bless() -> Options {
    Options {
        bless: true,
        ..Options::default()
    }
}

/// The problems of the one failed case.
fn failure(report: &Report) -> &str {
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    &report.failures[0].1
}

const MOVED: &str = "\
print a
fail MR0300 //~ ERROR MR0300
warn MR2002 //~ WARNING MR2002
";

#[test]
fn ui_case_passes_once_blessed() {
    let root = suite_dir("ui-blessed", "ui", &[("moves/moved.mr", MOVED)]);
    let first = ui(&root, &Options::default());
    assert!(failure(&first).contains("no snapshot; run with --bless"));

    let blessed = ui(&root, &bless());
    assert_eq!((blessed.passed, blessed.failures.len()), (1, 0));
    let snapshot = fs::read_to_string(root.join("tests/ui/moves/moved.stderr")).unwrap();
    assert_eq!(
        snapshot,
        "\
error[MR0300]: fake failure
 --> tests/ui/moves/moved.mr:2:1
  |
2 | fail MR0300 //~ ERROR MR0300
  | ^^^^ here

warning[MR2002]: fake warning
 --> tests/ui/moves/moved.mr:3:1
  |
3 | warn MR2002 //~ WARNING MR2002
  | ^^^^
"
    );

    let after = ui(&root, &Options::default());
    assert_eq!((after.passed, after.failures.len()), (1, 0));
}

#[test]
fn ui_reports_a_diagnostic_without_an_annotation() {
    let text = "print a\nfail MR0300\n";
    let root = suite_dir("ui-unannotated", "ui", &[("a.mr", text)]);
    let report = ui(&root, &bless());
    assert_eq!(
        failure(&report),
        "line 2: unexpected error MR0300: fake failure"
    );
}

#[test]
fn ui_reports_an_annotation_nothing_matched() {
    let text = "print a //~ ERROR MR0300\nfail MR0301 //~ ERROR MR0300\n";
    let root = suite_dir("ui-unreported", "ui", &[("a.mr", text)]);
    let report = ui(&root, &bless());
    assert_eq!(
        failure(&report),
        "\
line 2: unexpected error MR0301: fake failure
line 1: expected error MR0300, not reported
line 2: expected error MR0300, not reported"
    );
}

#[test]
fn ui_reports_a_changed_snapshot_with_a_diff() {
    let root = suite_dir("ui-changed", "ui", &[("a.mr", MOVED)]);
    ui(&root, &bless());
    let snapshot = root.join("tests/ui/a.stderr");
    let old = fs::read_to_string(&snapshot).unwrap();
    fs::write(&snapshot, old.replace("^^^^ here", "^^^^ there")).unwrap();

    let report = ui(&root, &Options::default());
    let problems = failure(&report);
    assert!(problems.starts_with("the output differs from the snapshot"));
    assert!(problems.contains("\n-   | ^^^^ there\n"), "{problems}");
    assert!(problems.contains("\n+   | ^^^^ here\n"), "{problems}");
}

#[test]
fn ui_snapshots_ignore_a_missing_final_newline() {
    let root = suite_dir("ui-newline", "ui", &[("a.mr", MOVED)]);
    ui(&root, &bless());
    let snapshot = root.join("tests/ui/a.stderr");
    let old = fs::read_to_string(&snapshot).unwrap();
    fs::write(&snapshot, old.trim_end()).unwrap();
    assert_eq!(ui(&root, &Options::default()).failures, []);
}

#[test]
fn bless_removes_the_snapshot_of_a_case_that_became_clean() {
    let root = suite_dir(
        "ui-clean",
        "ui",
        &[("a.mr", "print a\n"), ("a.stderr", "old\n")],
    );
    let report = ui(&root, &bless());
    assert_eq!((report.passed, report.failures.len()), (1, 0));
    assert!(!root.join("tests/ui/a.stderr").exists());
}

#[test]
fn a_panic_fails_only_its_case() {
    let root = suite_dir(
        "ui-panic",
        "ui",
        &[
            ("a.mr", "print a\n"),
            ("b.mr", "panic\n"),
            ("c.mr", "print c\n"),
        ],
    );
    let report = ui(&root, &Options::default());
    assert_eq!(report.passed, 2);
    assert_eq!(report.failures[0].0, "tests/ui/b.mr");
    assert_eq!(failure(&report), "panicked: the fake compiler crashed");
}

#[test]
fn filters_select_cases_by_path() {
    let root = suite_dir(
        "ui-filter",
        "ui",
        &[("moves/a.mr", "panic\n"), ("views/b.mr", "print b\n")],
    );
    let options = Options::parse(["views".to_owned()]);
    let report = ui(&root, &options);
    assert_eq!((report.passed, report.failures.len()), (1, 0));
}

#[test]
fn a_missing_suite_directory_is_an_error() {
    let root = suite_dir("ui-missing", "run", &[]);
    assert!(ui_suite(&BOTH, &root, "tests/ui", &Options::default()).is_err());
}

#[test]
fn an_empty_suite_passes() {
    let root = suite_dir("ui-empty", "ui", &[]);
    let report = ui(&root, &Options::default());
    assert_eq!((report.passed, report.failures.len()), (0, 0));
}

#[test]
fn run_case_passes_on_every_backend() {
    let text = "// expect: one\n// expect: two\n// exit: 3\nprint one\nprint two\nexit 3\n";
    let root = suite_dir("run-pass", "run", &[("a.mr", text)]);
    let report = run(&BOTH, &root);
    assert_eq!((report.passed, report.failures.len()), (1, 0));
}

#[test]
fn run_reports_a_stdout_mismatch_with_a_diff() {
    let text = "// expect: one\n// expect: two\nprint one\nprint 2\n";
    let root = suite_dir("run-stdout", "run", &[("a.mr", text)]);
    let report = run(&BOTH, &root);
    assert_eq!(
        failure(&report),
        "\
jit: stdout differs (- expected, + actual):
  one
- two
+ 2

aot: stdout differs (- expected, + actual):
  one
- two
+ 2
"
    );
}

#[test]
fn run_reports_an_exit_code_mismatch() {
    let text = "// expect: done\n// exit: 1\nprint done\nexit 2\n";
    let root = suite_dir("run-exit", "run", &[("a.mr", text)]);
    let report = run(&BOTH, &root);
    assert_eq!(
        failure(&report),
        "jit: exit code 2, expected 1\naot: exit code 2, expected 1"
    );
}

#[test]
fn run_names_the_backend_that_disagrees() {
    let text = "// expect: a\n// expect: b\nprint a\njit-print b\n";
    let root = suite_dir("run-differential", "run", &[("a.mr", text)]);
    let report = run(&BOTH, &root);
    let problems = failure(&report);
    assert!(problems.starts_with("aot: stdout differs"), "{problems}");
    assert!(!problems.contains("jit:"), "{problems}");
}

#[test]
fn run_reports_a_program_that_does_not_compile_once() {
    let root = suite_dir("run-build", "run", &[("a.mr", "fail MR0300\n")]);
    let report = run(&BOTH, &root);
    let problems = failure(&report);
    assert!(
        problems.starts_with("the program does not compile:\nerror[MR0300]: fake failure\n"),
        "{problems}"
    );
    assert!(!problems.contains("jit:"), "{problems}");
}

#[test]
fn run_without_a_backend_compiles_and_compares_the_hir() {
    let text = "// expect: a\nprint a\nhir one\n";
    let root = suite_dir("run-no-backend", "run", &[("a.mr", text)]);
    let none = Fake { backends: &[] };
    let problems = failure(&run(&none, &root)).to_owned();
    assert!(
        problems.contains("no snapshot; run with --bless"),
        "{problems}"
    );
    let report = run_with(&none, &root, &bless());
    assert_eq!((report.passed, report.failures.len()), (1, 0));
    let hir = root.join("tests/run/a.hir");
    assert_eq!(fs::read_to_string(&hir).unwrap(), "one\n");
    assert_eq!(run(&none, &root).failures.len(), 0);
    fs::write(&hir, "two\n").unwrap();
    let problems = failure(&run(&none, &root)).to_owned();
    assert!(problems.contains("- two\n+ one"), "{problems}");
}

#[test]
fn an_unreadable_snapshot_fails_its_case() {
    let root = suite_dir("ui-unreadable", "ui", &[("a.mr", "print a\n")]);
    fs::create_dir(root.join("tests/ui/a.stderr")).unwrap();
    let report = ui(&root, &Options::default());
    let problems = failure(&report);
    assert!(
        problems.starts_with("cannot read the snapshot: "),
        "{problems}"
    );
}

#[test]
fn finish_fails_only_when_a_case_failed() {
    let code = |report: &Report| format!("{:?}", report.finish("ui"));
    let passed = Report {
        passed: 2,
        failures: Vec::new(),
    };
    assert_eq!(code(&passed), format!("{:?}", ExitCode::SUCCESS));
    assert_eq!(passed.summary("ui"), "ui result: ok. 2 passed; 0 failed");
    let failed = Report {
        passed: 1,
        failures: vec![("tests/ui/a.mr".to_owned(), "broken".to_owned())],
    };
    assert_eq!(code(&failed), format!("{:?}", ExitCode::FAILURE));
    assert_eq!(
        failed.summary("run"),
        "run result: FAILED. 1 passed; 1 failed"
    );
}

#[test]
fn run_reports_a_bad_exit_line() {
    let root = suite_dir(
        "run-bad-exit",
        "run",
        &[("a.mr", "// exit: one\nprint a\n")],
    );
    let report = run(&Fake { backends: &[] }, &root);
    assert_eq!(failure(&report), "line 1: `one` is not an exit code");
}

#[test]
fn blessing_an_empty_hir_writes_no_snapshot() {
    let root = suite_dir("run-bless-empty", "run", &[("a.mr", "print a\n")]);
    let report = run_with(&Fake { backends: &[] }, &root, &bless());
    assert_eq!((report.passed, report.failures.len()), (1, 0));
    assert!(!root.join("tests/run/a.hir").exists());
}

#[test]
fn a_snapshot_that_cannot_be_written_fails_its_case() {
    let root = suite_dir("run-bless-dir", "run", &[("a.mr", "hir one\n")]);
    fs::create_dir(root.join("tests/run/a.hir")).unwrap();
    let report = run_with(&Fake { backends: &[] }, &root, &bless());
    assert!(
        failure(&report).starts_with("cannot "),
        "{:?}",
        report.failures
    );
}

#[test]
fn run_suite_reports_a_stale_mir_snapshot() {
    let text = "// expect: a\nprint a\nhir one\nmir body\n";
    let root = suite_dir("run-stale-mir", "run", &[("a.mr", text)]);
    let none = Fake { backends: &[] };
    fs::write(root.join("tests/run/a.hir"), "one\n").unwrap();
    let problems = failure(&run(&none, &root)).to_owned();
    assert!(
        problems.contains("no snapshot; run with --bless"),
        "{problems}"
    );
    fs::write(root.join("tests/run/a.mir"), "old\n").unwrap();
    let problems = failure(&run(&none, &root)).to_owned();
    assert!(problems.contains("- old\n+ body"), "{problems}");
    fs::write(root.join("tests/run/a.mir"), "body\n").unwrap();
    assert_eq!(run(&none, &root).failures.len(), 0);
}

#[test]
fn bless_writes_the_mir_snapshot() {
    let text = "// expect: a\nprint a\nhir one\nmir body\n";
    let root = suite_dir("run-bless-mir", "run", &[("a.mr", text)]);
    let none = Fake { backends: &[] };
    let report = run_with(&none, &root, &bless());
    assert_eq!((report.passed, report.failures.len()), (1, 0));
    assert_eq!(
        fs::read_to_string(root.join("tests/run/a.mir")).unwrap(),
        "body\n"
    );
    assert_eq!(run(&none, &root).failures.len(), 0);
}

#[test]
fn blessing_an_empty_mir_removes_the_snapshot() {
    let root = suite_dir("run-bless-empty-mir", "run", &[("a.mr", "print a\n")]);
    fs::write(root.join("tests/run/a.mir"), "old\n").unwrap();
    let report = run_with(&Fake { backends: &[] }, &root, &bless());
    assert_eq!((report.passed, report.failures.len()), (1, 0));
    assert!(!root.join("tests/run/a.mir").exists());
}
