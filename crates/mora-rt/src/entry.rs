//! Running the program (§13.8): the process entry point and the JIT both go
//! through [`run_entry`].

use std::io::Write;

/// Runs `main`, which calls the program's `__mora_main`, and returns its
/// exit code. Stdout is set up before the program runs and flushed after.
///
/// Getting stdout the first time allocates its buffer. Doing it here, before
/// the program runs, keeps that allocation off the panic path, which flushes
/// stdout and must work when memory has run out.
pub fn run_entry(main: impl FnOnce() -> i32) -> i32 {
    let mut stdout = std::io::stdout();
    let code = main();
    // Output the program buffered must not be lost, and nothing is left to
    // report it to if stdout is gone.
    let _ = stdout.flush();
    code
}
