//! The portable runtime core. `no_std`; reaches the platform only through the `mora_pal_*` interface (§13.1, §19.4).

#![no_std]
// One of the crates allowed to contain `unsafe` code (CONTRIBUTING.md).
#![allow(unsafe_code)]

pub mod alloc;
pub mod pal;
pub mod string;

/// Whether this is the debug runtime (`libmora_rt_debug.a`, §13.2), which
/// adds poisoning, leak tracking and extra assertions.
pub const DEBUG: bool = cfg!(feature = "debug");
