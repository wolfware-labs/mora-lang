//! Helpers shared by the tests of this crate: a test whose entry point ends
//! the process, as a panic does, or writes to stdout, runs in a child
//! process that re-executes the test binary.

use std::process::{Command, Output};

/// Set in the child process of a test.
const CHILD_ENV: &str = "MORA_RT_CHILD";

/// Runs the ignored test `test` of this crate in a child process, and
/// returns what it wrote and how it ended.
pub(crate) fn child(test: &str) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args([
            test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap()
}

/// Runs `body` in the child process of [`child`] only, so that the ignored
/// test that holds it returns at once when it runs on its own.
pub(crate) fn only_in_child(body: impl FnOnce()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_body_does_not_run_in_the_parent() {
        only_in_child(|| panic!("ran outside a child process"));
    }
}
