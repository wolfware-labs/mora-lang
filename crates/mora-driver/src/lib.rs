//! Projects, profiles, targets, the build graph and standard library loading (§14.15), and the
//! order of the passes that check and compile a project (§14.1).

mod compile;
mod stack;

pub use compile::{BuildError, check, jit};
pub use stack::{COMPILER_STACK_SIZE, on_compiler_stack};
