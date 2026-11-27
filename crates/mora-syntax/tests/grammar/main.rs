// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Parser tests, one module per grammar area. The snapshot tests and the
//! checks of the spec's examples are in `tests/parser.rs`.

// `tests/common` sits beside this folder, so the default path would miss it.
#[path = "../common/mod.rs"]
mod common;
mod support;

mod expressions;
mod items;
mod patterns;
mod recovery;
mod statements;
mod types;
