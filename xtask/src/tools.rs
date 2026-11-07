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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_come_from_the_environment_or_a_default() {
        assert_eq!(from_env("MORA_XTASK_NEVER_SET", "fallback"), "fallback");
        // Cargo sets `$CARGO` for the tests it runs.
        assert_eq!(cargo(), env::var_os("CARGO").unwrap());
        for (program, variable, default) in [(rustc(), "RUSTC", "rustc"), (cc(), "CC", "cc")] {
            assert_eq!(program, from_env(variable, default));
        }
    }
}
