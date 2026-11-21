//! Repository tooling, run as `cargo xtask <command>`: the layering, error
//! code and spec example checks, code generation, the fuzz lockfile, the
//! runtime and sysroot builds, and benchmark comparison. See [`USAGE`].

// Test and tooling code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod bench_compare;
mod bench_run;
mod codegen;
mod error_codes;
mod fuzz_lock;
mod layers;
mod paths;
mod runtime;
mod spec_examples;
mod sysroot;
mod target_names;
mod tools;

use std::path::Path;
use std::process::ExitCode;

use cargo_metadata::MetadataCommand;
use mora_diag::registry;

use crate::paths::root;

const USAGE: &str = "usage: cargo xtask <layers | error-codes | spec-examples | codegen [--check] | fuzz-lock [--check] | runtime [--test] [target...] | sysroot | bench-compare [--base <rev>] [--threshold <percent>]>";

/// What a command ends with: its exit code, or the reason it could not run,
/// which `main` prints after the command's name.
type CommandResult = Result<ExitCode, String>;

/// `cargo metadata --no-deps` for the workspace at `root`.
fn metadata_command(root: &Path) -> MetadataCommand {
    let mut command = MetadataCommand::new();
    command
        .manifest_path(root.join(paths::ROOT_MANIFEST))
        .no_deps();
    command
}

fn run_layers() -> CommandResult {
    let metadata = metadata_command(&root())
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
    let out = paths::target_dir(&root).join("sysroot");
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
        let shown = paths::relative(&root, path);
        let parent = path.parent().unwrap_or(path);
        std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(path, text))
            .map_err(|err| format!("{shown}: {err}"))?;
        println!("codegen: wrote {shown}");
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

/// `cargo metadata` for the fuzz crate, which cargo resolves against
/// `fuzz/Cargo.lock`.
fn fuzz_metadata(root: &Path) -> MetadataCommand {
    let mut command = MetadataCommand::new();
    command.manifest_path(root.join(paths::FUZZ_MANIFEST));
    command
}

/// The text of a file under the repository root; errors name `file` as
/// given, relative to the root.
fn read_text(root: &Path, file: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(file)).map_err(paths::error_at(Path::new(file)))
}

/// Reads and parses a lockfile under the repository root.
fn read_lock(root: &Path, lock: &str) -> Result<fuzz_lock::Lock, String> {
    fuzz_lock::Lock::parse(&read_text(root, lock)?).map_err(paths::error_at(Path::new(lock)))
}

/// The `[patch]` entries of `file` under the repository root, with each
/// `path` resolved against `dir`, the folder cargo reads it from.
fn read_patches_in(root: &Path, file: &str, dir: &str) -> Result<Vec<fuzz_lock::Patch>, String> {
    let dir = root.join(dir);
    let resolve = |path: &str| {
        let joined = dir.join(path);
        std::fs::canonicalize(&joined).unwrap_or(joined)
    };
    fuzz_lock::patches_in(file, &read_text(root, file)?, resolve)
        .map_err(paths::error_at(Path::new(file)))
}

/// The `[patch]` entries the workspace build and the fuzz build follow: the
/// two manifests', and the repository cargo config's, which both builds
/// read.
fn read_patches(root: &Path) -> Result<fuzz_lock::Patches, String> {
    let config = if root.join(paths::CARGO_CONFIG).is_file() {
        read_patches_in(root, paths::CARGO_CONFIG, ".")?
    } else {
        Vec::new()
    };
    let mut patches = fuzz_lock::Patches {
        root: read_patches_in(root, paths::ROOT_MANIFEST, ".")?,
        fuzz: read_patches_in(root, paths::FUZZ_MANIFEST, "fuzz")?,
    };
    patches.root.extend(config.iter().cloned());
    patches.fuzz.extend(config);
    Ok(patches)
}

/// Rebuilds `fuzz/Cargo.lock` from `Cargo.lock`, keeping the entries of the
/// crates only the fuzz crate uses, and lets cargo drop the rest. A missing
/// or broken fuzz lockfile, such as one left with merge conflicts, is
/// rebuilt from `Cargo.lock` alone, and cargo resolves the rest. When cargo
/// cannot resolve the result, the fuzz lockfile is left as it was.
fn run_fuzz_lock() -> CommandResult {
    let root = root();
    let fuzz = read_lock(&root, paths::FUZZ_LOCK).unwrap_or_else(|err| {
        eprintln!(
            "fuzz-lock: rebuilding {} from {} alone: {err}",
            paths::FUZZ_LOCK,
            paths::ROOT_LOCK
        );
        fuzz_lock::Lock::default()
    });
    let patches = read_patches(&root)?;
    let text = fuzz_lock::merged(&read_lock(&root, paths::ROOT_LOCK)?, &fuzz, &patches);
    let changed = fuzz_lock::write_resolved(&root.join(paths::FUZZ_LOCK), &text, || {
        fuzz_metadata(&root)
            .exec()
            .map(drop)
            .map_err(|err| err.to_string())
    })?;
    println!("{}", fuzz_lock::rebuilt(changed));
    let problems = fuzz_lock_problems(&root)?;
    report_fuzz_lock(&root, &problems, fuzz_lock::rebuild_summary(&problems))
}

/// Each problem with `fuzz/Cargo.lock`.
fn fuzz_lock_problems(root: &Path) -> Result<Vec<fuzz_lock::Problem>, String> {
    let locked = fuzz_metadata(root)
        .other_options(["--locked".to_owned()])
        .exec()
        .map(drop)
        .map_err(|err| err.to_string());
    Ok(fuzz_lock::problems(
        locked,
        &read_lock(root, paths::ROOT_LOCK)?,
        read_lock(root, paths::FUZZ_LOCK),
        &read_patches(root)?,
    ))
}

/// Prints the problems and the summary and fails, or, with no summary,
/// prints that the fuzz lockfile is ok.
fn report_fuzz_lock(
    root: &Path,
    problems: &[fuzz_lock::Problem],
    summary: Option<String>,
) -> CommandResult {
    let Some(summary) = summary else {
        let packages = read_lock(root, paths::FUZZ_LOCK)?.packages().len();
        println!("{}", fuzz_lock::ok_summary(packages));
        return Ok(ExitCode::SUCCESS);
    };
    for problem in problems {
        eprintln!("{problem}");
    }
    eprintln!("{summary}");
    Ok(ExitCode::FAILURE)
}

/// Fails when `fuzz/Cargo.lock` cannot be read, does not match the fuzz
/// manifest, or locks a crate it shares with the workspace differently from
/// `Cargo.lock`.
fn run_fuzz_lock_check() -> CommandResult {
    let root = root();
    let problems = fuzz_lock_problems(&root)?;
    report_fuzz_lock(&root, &problems, fuzz_lock::check_summary(&problems))
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
    let metadata = metadata_command(&root())
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
        ("fuzz-lock", []) => run_fuzz_lock(),
        ("fuzz-lock", ["--check"]) => run_fuzz_lock_check(),
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
