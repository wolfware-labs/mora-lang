//! Projects, profiles, targets, the build graph and standard library loading (§14.15).

mod stack;

pub use stack::{COMPILER_STACK_SIZE, on_compiler_stack};
