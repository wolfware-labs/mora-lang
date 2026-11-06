//! The programs the commands run, as the environment names them.

use std::env;
use std::ffi::OsString;

/// `$CARGO`, which cargo sets for the commands it runs, or `cargo`.
pub fn cargo() -> OsString {
    from_env("CARGO", "cargo")
}

/// `$RUSTC`, or `rustc`.
pub fn rustc() -> OsString {
    from_env("RUSTC", "rustc")
}

/// `$CC`, or `cc`.
pub fn cc() -> OsString {
    from_env("CC", "cc")
}

fn from_env(variable: &str, default: &str) -> OsString {
    env::var_os(variable).unwrap_or_else(|| default.into())
}
