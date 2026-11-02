//! The `tests/run` suite: `cargo test -p mora-testkit --test run -- [filter...]`.

use std::path::Path;
use std::process::ExitCode;

use mora_testkit::compiler;
use mora_testkit::harness::{Options, run_suite};

fn main() -> ExitCode {
    let options = Options::parse(std::env::args().skip(1));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match run_suite(compiler(), &root, "tests/run", &options) {
        Ok(report) => report.finish("run"),
        Err(error) => {
            eprintln!("cannot read tests/run: {error}");
            ExitCode::FAILURE
        }
    }
}
