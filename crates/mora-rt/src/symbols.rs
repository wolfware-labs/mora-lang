//! The entry points compiled code calls, by name, for the JIT.

use mora_rt_core::float::{mora_rt_fmod, mora_rt_fmodf};

use crate::panic::{mora_rt_panic, mora_rt_panic_overflow};
use crate::standin::*;
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
        (
            "__mora_standin_write_line_i64",
            __mora_standin_write_line_i64 as *const u8,
        ),
        (
            "__mora_standin_write_line_u64",
            __mora_standin_write_line_u64 as *const u8,
        ),
        (
            "__mora_standin_write_line_f32",
            __mora_standin_write_line_f32 as *const u8,
        ),
        (
            "__mora_standin_write_line_f64",
            __mora_standin_write_line_f64 as *const u8,
        ),
        (
            "__mora_standin_write_line_bool",
            __mora_standin_write_line_bool as *const u8,
        ),
        (
            "__mora_standin_write_line_char",
            __mora_standin_write_line_char as *const u8,
        ),
        (
            "__mora_standin_write_line_string",
            __mora_standin_write_line_string as *const u8,
        ),
        (
            "__mora_standin_format_i64",
            __mora_standin_format_i64 as *const u8,
        ),
        (
            "__mora_standin_format_u64",
            __mora_standin_format_u64 as *const u8,
        ),
        (
            "__mora_standin_format_f32",
            __mora_standin_format_f32 as *const u8,
        ),
        (
            "__mora_standin_format_f64",
            __mora_standin_format_f64 as *const u8,
        ),
        (
            "__mora_standin_format_bool",
            __mora_standin_format_bool as *const u8,
        ),
        (
            "__mora_standin_format_char",
            __mora_standin_format_char as *const u8,
        ),
    ]
}
