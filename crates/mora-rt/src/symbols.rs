//! The entry points compiled code calls, by name, for the JIT.

use mora_rt_core::float::{mora_rt_fmod, mora_rt_fmodf};

use crate::panic::{mora_rt_panic, mora_rt_panic_overflow};
use crate::string::{mora_rt_string_concat, mora_rt_string_eq};

/// Every entry point generated code calls with its address. A JIT resolves
/// calls through this list; an object file leaves them to the linker.
pub fn jit_symbols() -> Vec<(&'static str, *const u8)> {
    vec![
        ("mora_rt_panic", mora_rt_panic as *const u8),
        (
            "mora_rt_panic_overflow",
            mora_rt_panic_overflow as *const u8,
        ),
        ("mora_rt_fmod", mora_rt_fmod as *const u8),
        ("mora_rt_fmodf", mora_rt_fmodf as *const u8),
        ("mora_rt_string_concat", mora_rt_string_concat as *const u8),
        ("mora_rt_string_eq", mora_rt_string_eq as *const u8),
    ]
}
