//! The executable examples of the spec and of the error pages (§20.4):
//! `cargo test -p mora-testkit --test spec_examples`, which
//! `cargo xtask spec-examples` runs. The target has `test = false`, so a
//! plain `cargo test` leaves it out.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use mora_testkit::FrontEnd;
use mora_testkit::harness::without_panic_output;
use mora_testkit::spec_examples;

fn main() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = match spec_examples::load(&root) {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("spec-examples: {error}");
            return ExitCode::FAILURE;
        }
    };
    let front_end = FrontEnd::with_runner(PathBuf::from(env!("CARGO_BIN_EXE_jit-runner")));
    // An example that panics, as a pending one whose construct is not lowered
    // yet does, reports the message in its problem; the default hook would
    // print it as well, on every run.
    let outcome = without_panic_output(|| spec_examples::check(&repo, &front_end));
    for problem in &outcome.problems {
        eprintln!("{problem}");
    }
    if outcome.problems.is_empty() {
        println!(
            "spec-examples: {} examples, {} passed, {} pending, ok",
            repo.examples.len(),
            outcome.passed,
            outcome.pending
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("spec-examples: {} problem(s)", outcome.problems.len());
        ExitCode::FAILURE
    }
}
