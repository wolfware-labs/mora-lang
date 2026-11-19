//! The hosted runtime layer for Linux, macOS and Windows: process startup, I/O, the reactor and backtraces (§13.1).
// One of the crates allowed to contain `unsafe` code (CONTRIBUTING.md).
#![allow(unsafe_code)]

mod pal;
pub mod panic;
// Left out of this crate's unit tests, whose harness has its own `main`.
#[cfg(all(feature = "start", not(test)))]
mod start;

pub mod symbols;

pub use mora_rt_core::DEBUG;
pub use symbols::jit_symbols;
