//! HIR checks and lints: definite assignment, exhaustiveness, mutability, naming and strong cycles (§14.2).
//!
//! [`diagnostics`] reports what the control flow of a project's bodies
//! breaks: a local read before it is definitely assigned (`MR0181`), a
//! `let` local assigned again (`MR0182`), `break` or `continue` outside a
//! loop (`MR0183`), and a function that can end without returning a value
//! (`MR0184`). Each file's bodies are checked by one query, so an edit
//! inside a body checks only that body's file again.

mod flow;
mod report;

pub use report::diagnostics;

#[cfg(test)]
mod tests;
