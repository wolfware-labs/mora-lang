//! Repository tooling, run as `cargo xtask <command>`: the layering, error
//! code and spec example checks, code generation, the runtime and sysroot
//! builds, and benchmark comparison. See [`USAGE`].

// Test and tooling code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bench_compare;
mod bench_run;
mod codegen;
mod error_codes;
mod layers;
mod paths;
mod runtime;
mod spec_examples;
mod sysroot;
mod target_names;

use std::process::ExitCode;

use cargo_metadata::MetadataCommand;
use mora_diag::registry;

use crate::paths::root;

const USAGE: &str = "usage: cargo xtask <layers | error-codes | spec-examples | codegen [--check] | runtime [--test] [target...] | sysroot | bench-compare [--base <rev>] [--threshold <percent>]>";

/// What a command ends with: its exit code, or the reason it could not run,
/// which `main` prints after the command's name.
type CommandResult = Result<ExitCode, String>;

/// `cargo metadata --no-deps` for the workspace that contains this crate.
fn metadata_command() -> MetadataCommand {
    let mut command = MetadataCommand::new();
    command.manifest_path(root().join("Cargo.toml")).no_deps();
    command
}

fn run_layers() -> CommandResult {
    let metadata = metadata_command()
        .exec()
        .map_err(|err| format!("cargo metadata failed: {err}"))?;
    let violations = layers::check(&metadata);
    for violation in &violations {
        eprintln!("{violation}");
    }
    let hits = target_names::check(&root()).map_err(|err| format!("cannot scan crates/: {err}"))?;
    for hit in &hits {
        eprintln!("{hit}");
    }
    let count = violations.len() + hits.len();
    if count == 0 {
        println!(
            "layers: {} crates, no target names outside mora-target, mora-link and mora-codegen, ok",
            metadata.workspace_packages().len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("layers: {count} violation(s)");
        Ok(ExitCode::FAILURE)
    }
}

fn run_error_codes() -> CommandResult {
    let repo = error_codes::load(&root()).map_err(|err| err.to_string())?;
    let problems = error_codes::check(&repo, registry::RANGES, registry::CODES);
    for problem in &problems {
        eprintln!("{problem}");
    }
    if problems.is_empty() {
        println!(
            "error-codes: {} codes, {} pages, {} uses, ok",
            registry::CODES.len(),
            repo.code_pages().count(),
            repo.uses.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("error-codes: {} problem(s)", problems.len());
        Ok(ExitCode::FAILURE)
    }
}

fn run_spec_examples() -> CommandResult {
    let repo = spec_examples::load(&root()).map_err(|err| err.to_string())?;
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
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("spec-examples: {} problem(s)", outcome.problems.len());
        Ok(ExitCode::FAILURE)
    }
}

/// Builds both runtime variants for each target, the host when none is
/// given, and with `--test` checks each with the C test program.
fn run_runtime(args: &[&str]) -> CommandResult {
    let test = args.contains(&"--test");
    let mut targets: Vec<String> = args
        .iter()
        .filter(|arg| **arg != "--test")
        .map(|arg| (*arg).to_owned())
        .collect();
    if targets.is_empty() {
        targets.push(runtime::host()?);
    }
    let root = root();
    for target in &targets {
        for variant in runtime::VARIANTS {
            let built = runtime::build(&root, target, variant)?;
            if test {
                runtime::test(&root, target, &built)?;
            }
            println!(
                "runtime: {target} {}{} (links {})",
                variant.file,
                if test { ", test passed" } else { "" },
                built.native_libs.join(" ")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Installs the pinned linker and C runtime pieces for this host into
/// `target/sysroot`.
fn run_sysroot() -> CommandResult {
    let root = root();
    let archives = sysroot::archives(std::env::consts::ARCH, std::env::consts::OS)?;
    // Cargo resolves a relative target directory against the root.
    let out = root
        .join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
        .join("sysroot");
    match sysroot::install(&archives, &root.join("LICENSES"), &out)? {
        sysroot::Installed::UpToDate => println!("sysroot: {} is up to date", out.display()),
        sysroot::Installed::Fresh(lld) => {
            println!("sysroot: installed into {} ({lld})", out.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Writes the generated syntax code whose files are stale.
fn run_codegen() -> CommandResult {
    let root = root();
    let outputs = codegen::outputs(&root)?;
    let stale = codegen::stale(&outputs);
    for (path, text) in outputs.iter().filter(|(path, _)| stale.contains(path)) {
        let parent = path.parent().unwrap_or(path);
        std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(path, text))
            .map_err(|err| format!("{}: {err}", path.display()))?;
        println!("codegen: wrote {}", paths::relative(&root, path));
    }
    Ok(ExitCode::SUCCESS)
}

/// Reports the generated syntax files that differ from what the generator
/// would write.
fn run_codegen_check() -> CommandResult {
    let root = root();
    let outputs = codegen::outputs(&root)?;
    let stale = codegen::stale(&outputs);
    for path in &stale {
        eprintln!(
            "codegen: {} is stale; run `cargo xtask codegen`",
            paths::relative(&root, path)
        );
    }
    if stale.is_empty() {
        println!("codegen: {} files, up to date", outputs.len());
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

/// Benchmarks a base revision and the working tree, and fails when a
/// benchmark regresses by more than the threshold.
fn run_bench_compare(args: &[&str]) -> CommandResult {
    let options = match bench_compare::parse_args(args) {
        Ok(options) => options,
        Err(err) => {
            // A bad argument is a usage error, like an unknown command.
            eprintln!("bench-compare: {err}\n{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    let metadata = metadata_command()
        .exec()
        .map_err(|err| format!("cargo metadata failed: {err}"))?;
    let target_dir = metadata.target_directory.as_std_path();
    let comparisons = bench_run::run(&root(), target_dir, &metadata, &options)?;
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
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("bench-compare: {regressions} regression(s)");
        Ok(ExitCode::FAILURE)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some((&command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match (command, rest) {
        ("layers", []) => run_layers(),
        ("error-codes", []) => run_error_codes(),
        ("spec-examples", []) => run_spec_examples(),
        ("codegen", []) => run_codegen(),
        ("codegen", ["--check"]) => run_codegen_check(),
        ("runtime", rest) => run_runtime(rest),
        ("sysroot", []) => run_sysroot(),
        ("bench-compare", rest) => run_bench_compare(rest),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    result.unwrap_or_else(|err| {
        eprintln!("{command}: {err}");
        ExitCode::FAILURE
    })
}
