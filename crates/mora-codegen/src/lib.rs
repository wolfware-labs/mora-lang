//! The `Backend` trait and its Cranelift implementation: object files, JIT and debug info (§14.13).

mod abi;
mod cranelift;
mod error;
// One of the crates allowed to contain `unsafe` code (CONTRIBUTING.md), and
// only where it calls JIT-compiled code.
#[expect(unsafe_code, reason = "runs JIT-compiled code and frees its memory")]
mod jit;
mod layout;
mod mangle;
mod program;
mod size;
#[cfg(test)]
#[expect(
    unsafe_code,
    reason = "calls JIT-compiled functions and reads string objects"
)]
mod test_util;

pub use abi::{FnAbi, PassMode};
pub use cranelift::Cranelift;
pub use error::CodegenError;
pub use jit::JitProgram;
pub use layout::{Layout, Layouts, Scalar, Shape};
pub use mangle::mangle;
pub use program::{BodyKey, Function, FunctionId, Program};
pub use size::{check_sizes, check_sizes_on_host};

use mora_target::Target;

/// How much the generated code is optimized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Not optimized, for debugging.
    Debug,
    /// Optimized for speed.
    Release,
}

/// A code generator that writes object files.
pub trait Backend {
    /// The object file of `program` for `target`.
    ///
    /// # Errors
    ///
    /// [`CodegenError::UnsupportedTarget`] for a target the backend cannot
    /// generate for; [`CodegenError::TypeTooLarge`] for a type larger than
    /// 2^31 bytes on it; [`CodegenError::FrameTooLarge`] for a function
    /// whose stack frame would pass 2^30 bytes; and
    /// [`CodegenError::Cranelift`] when code generation fails otherwise.
    fn object(
        &self,
        program: &Program<'_>,
        target: &Target,
        profile: Profile,
    ) -> Result<Vec<u8>, CodegenError>;
}

/// A code generator that also runs code in this process (§14.13).
pub trait JitBackend {
    /// `program` compiled for the host, ready to run.
    ///
    /// # Errors
    ///
    /// [`CodegenError::UnsupportedTarget`] when the host has no built-in
    /// target; [`CodegenError::TypeTooLarge`] for a type larger than 2^31
    /// bytes; [`CodegenError::FrameTooLarge`] for a function whose stack
    /// frame would pass 2^30 bytes; and [`CodegenError::Cranelift`] when
    /// code generation fails otherwise.
    fn jit(&self, program: &Program<'_>, profile: Profile) -> Result<JitProgram, CodegenError>;
}
