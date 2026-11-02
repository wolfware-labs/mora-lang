//! Finding the cases of a suite, running them in parallel and reporting.

use std::any::Any;
use std::fs;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::compiler::Compiler;
use crate::{run, ui};

/// The arguments after `--` in `cargo test --test ui -- --bless`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Write the snapshots of UI cases instead of comparing them.
    pub bless: bool,
    /// Run only the cases whose path contains one of these; all when empty.
    pub filters: Vec<String>,
}

impl Options {
    /// Reads `--bless` and filters. Other flags are ignored, because
    /// `cargo test` passes the flags of the standard harness, such as
    /// `--nocapture`, to every test binary. The value after one of those
    /// flags that takes a value, as in `--test-threads 4`, is skipped too, so
    /// it does not become a filter.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut options = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg == "--bless" {
                options.bless = true;
            } else if TAKES_VALUE.contains(&arg.as_str()) {
                args.next();
            } else if !arg.starts_with('-') {
                options.filters.push(arg);
            }
        }
        options
    }

    fn selects(&self, path: &str) -> bool {
        self.filters.is_empty() || self.filters.iter().any(|f| path.contains(f.as_str()))
    }
}

/// The flags of the standard test harness that take their value as the next
/// argument.
const TAKES_VALUE: &[&str] = &[
    "--color",
    "--format",
    "--logfile",
    "--shuffle-seed",
    "--skip",
    "--test-threads",
    "-Z",
];

/// The outcome of a suite: how many cases passed and why the others failed.
#[derive(Debug, Default)]
pub struct Report {
    /// How many cases passed.
    pub passed: usize,
    /// The path of each failed case and what went wrong, in path order.
    pub failures: Vec<(String, String)>,
}

impl Report {
    /// Prints the failures and a summary, and gives the exit code for them.
    pub fn finish(&self, suite: &str) -> ExitCode {
        for (path, problems) in &self.failures {
            println!("---- {path} ----\n{problems}");
        }
        println!("{}", self.summary(suite));
        if self.failures.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    /// The last line [`Report::finish`] prints, as the standard harness
    /// words it.
    pub fn summary(&self, suite: &str) -> String {
        let failed = self.failures.len();
        let result = if failed == 0 { "ok" } else { "FAILED" };
        format!(
            "{suite} result: {result}. {} passed; {failed} failed",
            self.passed
        )
    }
}

/// Runs the UI cases in `root/dir`: every `.mr` file below it, with its
/// snapshot in the `.stderr` file beside it. With `--bless` the snapshots
/// are written (and removed for cases with no output) instead of compared;
/// the annotations are still checked.
pub fn ui_suite(
    compiler: &dyn Compiler,
    root: &Path,
    dir: &str,
    options: &Options,
) -> io::Result<Report> {
    suite(root, dir, options, |path, file, text| {
        let outcome = ui::check(compiler, path, text);
        let mut problems = outcome.problems;
        let snapshot_file = file.with_extension("stderr");
        problems.extend(snapshot(&snapshot_file, &outcome.rendered, options.bless));
        problems
    })
}

/// Runs the run cases in `root/dir`: every `.mr` file below it.
/// Compares `actual` with the snapshot in `file`, or with `bless` writes it
/// there; an empty `actual` means no snapshot file. Returns the problems.
fn snapshot(file: &Path, actual: &str, bless: bool) -> Vec<String> {
    let snapshot = match fs::read_to_string(file) {
        Ok(snapshot) => Some(snapshot),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return vec![format!("cannot read the snapshot: {error}")],
    };
    if !bless {
        return ui::compare_snapshot(snapshot.as_deref(), actual)
            .into_iter()
            .collect();
    }
    let written = if actual.is_empty() {
        if snapshot.is_some() {
            fs::remove_file(file)
        } else {
            Ok(())
        }
    } else {
        fs::write(file, actual)
    };
    match written {
        Ok(()) => Vec::new(),
        Err(error) => vec![format!("cannot bless the snapshot: {error}")],
    }
}

pub fn run_suite(
    compiler: &dyn Compiler,
    root: &Path,
    dir: &str,
    options: &Options,
) -> io::Result<Report> {
    suite(root, dir, options, |path, file, text| {
        let outcome = run::check_case(compiler, path, text);
        let mut problems = outcome.problems;
        // A program that does not compile has no HIR to compare.
        let Some(hir) = outcome.hir else {
            return problems;
        };
        let snapshot_file = file.with_extension("hir");
        problems.extend(snapshot(&snapshot_file, &hir, options.bless));
        problems
    })
}

/// Finds the cases and runs `case` on each, on as many threads as there are
/// cores. A panic fails its case only.
fn suite(
    root: &Path,
    dir: &str,
    options: &Options,
    case: impl Fn(&str, &Path, &str) -> Vec<String> + Sync,
) -> io::Result<Report> {
    let mut cases = Vec::new();
    collect(&root.join(dir), &mut cases)?;
    cases.sort();
    let cases: Vec<(String, PathBuf)> = cases
        .into_iter()
        .map(|file| (shown(root, &file), file))
        .filter(|(path, _)| options.selects(path))
        .collect();

    let next = AtomicUsize::new(0);
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(cases.len());
    let mut results: Vec<Option<Vec<String>>> = vec![None; cases.len()];
    thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, file)) = cases.get(index) else {
                            break;
                        };
                        let problems = match fs::read_to_string(file) {
                            Ok(text) => guarded(|| case(path, file, &text)),
                            Err(error) => vec![format!("cannot read the case: {error}")],
                        };
                        done.push((index, problems));
                    }
                    done
                })
            })
            .collect();
        for handle in handles {
            for (index, problems) in handle.join().unwrap() {
                results[index] = Some(problems);
            }
        }
    });

    let mut report = Report::default();
    for ((path, _), problems) in cases.into_iter().zip(results) {
        let problems = problems.unwrap_or_default();
        if problems.is_empty() {
            report.passed += 1;
        } else {
            report.failures.push((path, problems.join("\n")));
        }
    }
    Ok(report)
}

/// The problems `case` returns, or the message of its panic as the only one.
pub fn guarded(case: impl FnOnce() -> Vec<String>) -> Vec<String> {
    panic::catch_unwind(AssertUnwindSafe(case))
        .unwrap_or_else(|payload| vec![format!("panicked: {}", message(&*payload))])
}

/// Every `.mr` file below `dir`. A missing directory is an error, so a
/// misspelled suite does not pass by running nothing.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "mr") {
            out.push(path);
        }
    }
    Ok(())
}

/// The path relative to `root` with `/` separators, as reports and rendered
/// diagnostics show it on every platform.
fn shown(root: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<_> = relative.iter().map(|part| part.to_string_lossy()).collect();
    parts.join("/")
}

fn message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-string payload".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_keep_bless_and_filters_and_skip_other_flags() {
        let args = [
            "--nocapture",
            "moves",
            "--test-threads",
            "4",
            "--bless",
            "--skip",
            "slow",
            "-q",
            "--format=terse",
            "views",
        ]
        .map(String::from);
        assert_eq!(
            Options::parse(args),
            Options {
                bless: true,
                filters: vec!["moves".to_owned(), "views".to_owned()],
            }
        );
    }
}
