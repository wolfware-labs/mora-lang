//! The `Backend` trait and its Cranelift implementation: object files, JIT and debug info (§14.13).
// One of the crates allowed to contain `unsafe` code (CONTRIBUTING.md).
#![allow(unsafe_code)]

mod abi;
mod error;
mod layout;
mod mangle;
mod program;
#[cfg(test)]
mod test_util;

pub use abi::{FnAbi, PassMode};
pub use error::CodegenError;
pub use layout::{Layout, Layouts, Scalar, Shape};
pub use mangle::mangle;
pub use program::{BodyKey, Function, FunctionId, Program};
