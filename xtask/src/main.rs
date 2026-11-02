//! Repository tooling, run as `cargo xtask <command>`. Commands arrive with
//! the M0 deliverables that need them.

// Test and tooling code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bench_compare;
mod bench_run;
mod codegen;
mod error_codes;
mod layers;
mod runtime;
mod spec_examples;
mod sysroot;
mod target_names;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cargo_metadata::MetadataCommand;
use mora_diag::registry;

const USAGE: &str = "usage: cargo xtask <layers | error-codes | spec-examples | codegen [--check] | runtime [--test] [target...] | sysroot | bench-compare [--base <rev>] [--threshold <percent>]>";

/// `cargo metadata --no-deps` for the workspace that contains this crate.
fn metadata_command() -> MetadataCommand {
    let mut command = MetadataCommand::new();
    command.manifest_path(root().join("Cargo.toml")).no_deps();
    command
}

fn run_layers() -> ExitCode {
    let metadata = match metadata_command().exec() {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!("layers: cargo metadata failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let violations = layers::check(&metadata);
    for violation in &violations {
        eprintln!("{violation}");
    }
    let hits = match target_names::check(&root()) {
        Ok(hits) => hits,
        Err(err) => {
            eprintln!("layers: cannot scan crates/: {err}");
            return ExitCode::FAILURE;
        }
    };
    for hit in &hits {
        eprintln!("{hit}");
    }
    let count = violations.len() + hits.len();
    if count == 0 {
        println!(
            "layers: {} crates, no target names outside mora-target, mora-link and mora-codegen, ok",
            metadata.workspace_packages().len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("layers: {count} violation(s)");
        ExitCode::FAILURE
    }
}

/// The repository root: the parent of this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn run_error_codes() -> ExitCode {
    let repo = match error_codes::load(&root()) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("error-codes: {err}");
            return ExitCode::FAILURE;
        }
    };
    let problems = error_codes::check(&repo, registry::RANGES, registry::CODES);
    for problem in &problems {
        eprintln!("{problem}");
    }
    if problems.is_empty() {
        let pages = repo
            .pages
            .iter()
            .filter(|(file, _)| file != "README.md")
            .count();
        println!(
            "error-codes: {} codes, {pages} pages, {} uses, ok",
            registry::CODES.len(),
            repo.uses.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("error-codes: {} problem(s)", problems.len());
        ExitCode::FAILURE
    }
}

fn run_spec_examples() -> ExitCode {
    let repo = match spec_examples::load(&root()) {
        Ok(repo) => repo,
        Err(err) => {
            eprintln!("spec-examples: {err}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = spec_examples::check(&repo, mora_testkit::compiler());
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

/// Builds both runtime variants for each target, the host when none is
/// given, and with `--test` checks each with the C test program.
fn run_runtime(args: &[&str]) -> ExitCode {
    let test = args.contains(&"--test");
    let mut targets: Vec<String> = args
        .iter()
        .filter(|arg| **arg != "--test")
        .map(|arg| (*arg).to_owned())
        .collect();
    if targets.is_empty() {
        match runtime::host() {
            Ok(host) => targets.push(host),
            Err(err) => {
                eprintln!("runtime: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    let root = root();
    for target in &targets {
        for variant in runtime::VARIANTS {
            let result = runtime::build(&root, target, variant).and_then(|built| {
                if test {
                    runtime::test(&root, target, &built)?;
                }
                Ok(built)
            });
            match result {
                Ok(built) => println!(
                    "runtime: {target} {}{} (links {})",
                    variant.file,
                    if test { ", test passed" } else { "" },
                    built.native_libs.join(" ")
                ),
                Err(err) => {
                    eprintln!("runtime: {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Installs the pinned linker and C runtime pieces for this host into
/// `target/sysroot`.
fn run_sysroot() -> ExitCode {
    let root = root();
    let archives = match sysroot::archives(std::env::consts::ARCH, std::env::consts::OS) {
        Ok(archives) => archives,
        Err(err) => {
            eprintln!("sysroot: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Cargo resolves a relative target directory against the root.
    let out = root
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("sysroot");
    match sysroot::install(&archives, &root.join("LICENSES"), &out) {
        Ok(sysroot::Installed::UpToDate) => {
            println!("sysroot: {} is up to date", out.display());
            ExitCode::SUCCESS
        }
        Ok(sysroot::Installed::Fresh(lld)) => {
            println!("sysroot: installed into {} ({lld})", out.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("sysroot: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `path` relative to the repository root, for messages.
fn relative(path: &Path) -> &Path {
    path.strip_prefix(root()).unwrap_or(path)
}

/// Writes the generated syntax code, or with `check` reports what is stale.
fn run_codegen(check: bool) -> ExitCode {
    let outputs = match codegen::outputs(&root()) {
        Ok(outputs) => outputs,
        Err(err) => {
            eprintln!("codegen: {err}");
            return ExitCode::FAILURE;
        }
    };
    let stale = codegen::stale(&outputs);
    if check {
        for path in &stale {
            eprintln!(
                "codegen: {} is stale; run `cargo xtask codegen`",
                path.display()
            );
        }
        return if stale.is_empty() {
            println!("codegen: {} files, up to date", outputs.len());
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    for (path, text) in &outputs {
        if stale.contains(path) {
            let parent = path.parent().unwrap_or(path);
            if let Err(err) =
                std::fs::create_dir_all(parent).and_then(|()| std::fs::write(path, text))
            {
                eprintln!("codegen: {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
            println!("codegen: wrote {}", relative(path).display());
        }
    }
    ExitCode::SUCCESS
}

/// Benchmarks a base revision and the working tree, and fails when a
/// benchmark regresses by more than the threshold.
fn run_bench_compare(args: &[&str]) -> ExitCode {
    let options = match bench_compare::parse_args(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("bench-compare: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let metadata = match metadata_command().exec() {
        Ok(metadata) => metadata,
        Err(err) => {
            eprintln!("bench-compare: cargo metadata failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let target_dir = metadata.target_directory.clone().into_std_path_buf();
    let comparisons = match bench_run::run(&root(), &target_dir, &metadata, &options) {
        Ok(comparisons) => comparisons,
        Err(err) => {
            eprintln!("bench-compare: {err}");
            return ExitCode::FAILURE;
        }
    };
    let (text, regressions) = bench_compare::report(&comparisons, &options);
    println!(
        "\nbench-compare: the working tree against {}, failing over {:.0}%\n\n{text}",
        options.base,
        options.threshold * 100.0
    );
    if regressions == 0 {
        println!(
            "bench-compare: {} benchmarks, no regression, ok",
            comparisons.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("bench-compare: {regressions} regression(s)");
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["layers"] => run_layers(),
        ["error-codes"] => run_error_codes(),
        ["spec-examples"] => run_spec_examples(),
        ["codegen"] => run_codegen(false),
        ["codegen", "--check"] => run_codegen(true),
        ["runtime", ref rest @ ..] => run_runtime(rest),
        ["sysroot"] => run_sysroot(),
        ["bench-compare", ref rest @ ..] => run_bench_compare(rest),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
