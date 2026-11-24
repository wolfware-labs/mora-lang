//! Running the program (§13.8): the process entry point and the JIT both go
//! through [`run_entry`].

use std::io::Write;

/// Runs `main`, which calls the program's `__mora_main`, and returns its
/// exit code. Stdout and stderr are set up before the program runs, and
/// stdout is flushed after.
///
/// The panic path flushes stdout and writes to stderr, and must work when
/// memory has run out, so whatever those first allocate is allocated here,
/// before the program runs: getting stdout the first time allocates its
/// buffer, and on targets whose locks are not futexes, such as macOS, the
/// first lock of each stream allocates the lock itself.
pub fn run_entry(main: impl FnOnce() -> i32) -> i32 {
    let mut stdout = std::io::stdout();
    drop(stdout.lock());
    drop(std::io::stderr().lock());
    let code = main();
    // Output the program buffered must not be lost, and nothing is left to
    // report it to if stdout is gone.
    let _ = stdout.flush();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_code_is_the_one_main_returns() {
        for code in [0, 3, 101] {
            assert_eq!(run_entry(|| code), code);
        }
    }
}
