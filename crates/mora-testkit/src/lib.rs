//! UI, run, spec-example and leak test harnesses (§20).
//!
//! A harness asks a [`Compiler`] to check or run each case of a suite and
//! compares what comes back with what the case expects: [`ui`] matches
//! diagnostics against `//~` annotations and a rendered snapshot, [`run`]
//! matches a program's output against `// expect:` and `// exit:` lines.
//! [`harness`] finds the cases, runs them in parallel and reports.

// Test and tooling code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod case;
mod compiler;
mod diff;
pub mod harness;
pub mod run;
pub mod runner;
pub mod ui;

pub use compiler::{Backend, Checked, Compiler, FrontEnd, Output, Unbuilt, compiler};
pub use mora_codegen::Profile;
