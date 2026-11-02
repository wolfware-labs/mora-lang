//! The part of `cargo xtask bench-compare` that runs other programs: it
//! checks the base revision out with `git worktree` and runs `cargo bench`
//! on it and on the working tree. [`crate::bench_compare`] reads and judges
//! the results.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use cargo_metadata::Metadata;

use crate::bench_compare::{
    BASELINE, Comparison, Options, Target, clear_new, collect, split_by_base, targets,
};

/// Runs the comparison and returns each benchmark's result. `target_dir` is
/// the working tree's cargo target directory.
pub fn run(
    root: &Path,
    target_dir: &Path,
    metadata: &Metadata,
    options: &Options,
) -> Result<Vec<Comparison>, String> {
    let targets = targets(metadata);
    if targets.is_empty() {
        return Err("the workspace has no bench targets".to_owned());
    }
    let work = target_dir.join("bench-compare");
    let checkout = work.join("base");
    let base_target = work.join("base-target");
    let home = work.join("criterion");
    if home.exists() {
        fs::remove_dir_all(&home).map_err(|err| format!("{}: {err}", home.display()))?;
    }
    fs::create_dir_all(&work).map_err(|err| format!("{}: {err}", work.display()))?;
    add_worktree(root, &checkout, &options.base)?;
    let result = (|| {
        let (present, missing) = split_by_base(&checkout, &targets);
        for target in missing {
            println!(
                "bench-compare: {} has no {}/{}; its results are new",
                options.base, target.package, target.name
            );
        }
        for target in present {
            println!("bench-compare: {} on {}", target.name, options.base);
            bench(&checkout, &base_target, &home, target, "--save-baseline")?;
        }
        if home.exists() {
            clear_new(&home)?;
        }
        for target in &targets {
            println!("bench-compare: {} on the working tree", target.name);
            bench(root, target_dir, &home, target, "--baseline-lenient")?;
        }
        collect(&home)
    })();
    remove_worktree(root, &checkout);
    result
}

fn git(root: &Path, args: &[&str]) -> Result<(), String> {
    run_git(root, args, Stdio::inherit())
}

/// Runs git without its error output, for cleanup that may find nothing to
/// clean up.
fn git_quiet(root: &Path, args: &[&str]) {
    let _ = run_git(root, args, Stdio::null());
}

fn run_git(root: &Path, args: &[&str], stderr: Stdio) -> Result<(), String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stderr(stderr)
        .status()
        .map_err(|err| format!("git: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed", args.join(" ")))
    }
}

fn add_worktree(root: &Path, checkout: &Path, base: &str) -> Result<(), String> {
    remove_worktree(root, checkout);
    let path = checkout.to_string_lossy();
    git(
        root,
        &["worktree", "add", "--detach", "--force", &path, base],
    )
}

/// Removes the base checkout. A failure leaves a directory that the next
/// run removes before it checks out again, so it is not an error.
fn remove_worktree(root: &Path, checkout: &Path) {
    let path = checkout.to_string_lossy();
    git_quiet(root, &["worktree", "remove", "--force", &path]);
    let _ = fs::remove_dir_all(checkout);
    git_quiet(root, &["worktree", "prune"]);
}

fn bench(
    dir: &Path,
    target_dir: &Path,
    home: &Path,
    target: &Target,
    baseline_flag: &str,
) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CRITERION_HOME", home)
        .args([
            "bench",
            "--locked",
            "-p",
            &target.package,
            "--bench",
            &target.name,
        ])
        .args(["--", baseline_flag, BASELINE, "--noplot"])
        .status()
        .map_err(|err| format!("cargo bench: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "cargo bench -p {} --bench {} failed in {}",
            target.package,
            target.name,
            dir.display()
        ))
    }
}
