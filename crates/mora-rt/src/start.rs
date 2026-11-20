//! Process startup (§13.1).

use std::ffi::{c_char, c_int};
use std::io::Write;

unsafe extern "C" {
    /// Emitted by the compiler: runs the program and returns its exit code
    /// (§4.6).
    fn __mora_main() -> i32;
}

/// The process entry symbol. The C runtime of each platform calls `main`.
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    // Getting stdout the first time allocates its buffer. Doing it here,
    // before the program runs, keeps that allocation off the panic path,
    // which flushes stdout and must work when memory has run out.
    let mut stdout = std::io::stdout();
    // SAFETY: the program defines `__mora_main` with this signature.
    let code = unsafe { __mora_main() };
    // Output the program buffered must not be lost, and nothing is left to
    // report it to if stdout is gone.
    let _ = stdout.flush();
    code
}
