//! Records the commit `mora version` prints: `MORA_COMMIT` when set and not
//! empty, as a release build sets it, or else the checkout's `HEAD`.

// Cargo reads the script's stdout, and a build that cannot reach Cargo
// should fail, so a panic on a write error is right here.
#![allow(clippy::disallowed_macros)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=MORA_COMMIT");
    let commit = std::env::var("MORA_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .or_else(head)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo::rustc-env=MORA_COMMIT={commit}");
}

/// The short hash of `HEAD`. The script runs again when `HEAD` or a ref
/// changes, in a plain checkout, a worktree or a submodule alike.
fn head() -> Option<String> {
    // Cargo reruns a script on every build while a watched path is missing,
    // so only existing paths are watched. `refs` is a directory, which Cargo
    // scans, so a branch that moves or is first written loose is seen.
    for name in ["HEAD", "refs", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]).map(PathBuf::from) {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
            if path.exists() {
                println!("cargo::rerun-if-changed={}", path.display());
            }
        }
    }
    git(&["rev-parse", "--short=12", "HEAD"])
}

/// The trimmed output of a successful `git` command run in this crate.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}
