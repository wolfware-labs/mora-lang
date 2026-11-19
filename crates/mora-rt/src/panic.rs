//! Panics (§11.4): the entry points compiled code calls when it must stop.

use std::io::Write;
use std::{fmt, process, slice};

/// The exit code of a panic (§11.4, D-26).
pub const PANIC_EXIT_CODE: i32 = 101;

/// The operation of an overflow panic. Generated code passes it as a
/// `u32`, so the values are part of the interface.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverflowOp {
    /// `+`.
    Add = 0,
    /// `-`.
    Sub = 1,
    /// `*`.
    Mul = 2,
    /// `/`, of the minimum of a signed type by -1.
    Div = 3,
    /// `%`, of the minimum of a signed type by -1.
    Rem = 4,
    /// Unary `-`.
    Neg = 5,
}

/// Each operation with the operator it prints.
const OPERATIONS: [(OverflowOp, &str); 6] = [
    (OverflowOp::Add, "+"),
    (OverflowOp::Sub, "-"),
    (OverflowOp::Mul, "*"),
    (OverflowOp::Div, "/"),
    (OverflowOp::Rem, "%"),
    (OverflowOp::Neg, "-"),
];

/// What a panic prints: `panic at <file>:<line>:<column>: <message>`, or
/// `panic: <message>` when it has no source location, which `file` being
/// empty says.
pub fn panic_text(file: &str, line: u32, column: u32, message: &str) -> String {
    if file.is_empty() {
        format!("panic: {message}\n")
    } else {
        format!("panic at {file}:{line}:{column}: {message}\n")
    }
}

/// The message of an overflow of the operation `op` (an [`OverflowOp`]),
/// the text constant evaluation reports too. Each operand arrives
/// extended to 64 bits, and prints as an `i64` when `signed` and as a `u64`
/// otherwise.
pub fn overflow_message(op: u32, signed: bool, left: u64, right: u64) -> String {
    let operand = |bits: u64| {
        if signed {
            (bits as i64).to_string()
        } else {
            bits.to_string()
        }
    };
    let (left, right) = (operand(left), operand(right));
    let symbol = match OPERATIONS.iter().find(|(known, _)| *known as u32 == op) {
        Some((OverflowOp::Neg, _)) => return format!("arithmetic overflow: -({left})"),
        Some((_, symbol)) => symbol,
        None => "?",
    };
    format!("arithmetic overflow: {left} {symbol} {right}")
}

/// Flushes stdout, prints `text` to stderr and ends the process with
/// [`PANIC_EXIT_CODE`]. Nothing is left to report a failed write to.
fn raise(text: &str) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(text.as_bytes());
    process::exit(PANIC_EXIT_CODE)
}

/// Panics with `message`, which has no source location: flushes stdout,
/// prints `panic: <message>` to stderr as `message` is formatted, building
/// nothing on the heap, so that it works when memory has run out, and ends
/// the process with [`PANIC_EXIT_CODE`].
pub(crate) fn panic_without_location(message: fmt::Arguments<'_>) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr()
        .lock()
        .write_fmt(format_args!("panic: {message}\n"));
    process::exit(PANIC_EXIT_CODE)
}

/// The text of `len` bytes at `ptr`; an invalid sequence becomes U+FFFD.
///
/// # Safety
///
/// `ptr` points to `len` readable bytes, or `len` is 0.
unsafe fn text<'a>(ptr: *const u8, len: usize) -> std::borrow::Cow<'a, str> {
    if len == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    // SAFETY: the caller guarantees `len` readable bytes at `ptr`.
    String::from_utf8_lossy(unsafe { slice::from_raw_parts(ptr, len) })
}

/// Panics with `message` at the source location `file:line:column`.
///
/// # Safety
///
/// `file` points to `file_len` readable bytes and `message` to
/// `message_len` readable bytes, or the length is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_rt_panic(
    file: *const u8,
    file_len: usize,
    line: u32,
    column: u32,
    message: *const u8,
    message_len: usize,
) -> ! {
    // SAFETY: the caller upholds the pointers and lengths.
    let (file, message) = unsafe { (text(file, file_len), text(message, message_len)) };
    raise(&panic_text(&file, line, column, &message))
}

/// Panics with the overflow of the operation `op`, an [`OverflowOp`], on the
/// operands `left` and `right` extended to 64 bits, which are signed when
/// `signed` is not 0.
///
/// # Safety
///
/// `file` points to `file_len` readable bytes, or the length is 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_rt_panic_overflow(
    file: *const u8,
    file_len: usize,
    line: u32,
    column: u32,
    op: u32,
    signed: u32,
    left: u64,
    right: u64,
) -> ! {
    // SAFETY: the caller upholds the pointer and length.
    let file = unsafe { text(file, file_len) };
    let message = overflow_message(op, signed != 0, left, right);
    raise(&panic_text(&file, line, column, &message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{child, only_in_child};

    #[test]
    fn panic_text_names_the_location() {
        assert_eq!(
            panic_text("src/Program.mr", 3, 9, "boom"),
            "panic at src/Program.mr:3:9: boom\n"
        );
        assert_eq!(panic_text("", 0, 0, "boom"), "panic: boom\n");
    }

    #[test]
    fn overflow_messages_print_unsigned_and_64_bit_operands() {
        let message =
            |op: OverflowOp, signed, left, right| overflow_message(op as u32, signed, left, right);
        assert_eq!(
            message(OverflowOp::Add, true, 2147483647, 1),
            "arithmetic overflow: 2147483647 + 1"
        );
        assert_eq!(
            message(OverflowOp::Add, false, u64::MAX, 1),
            "arithmetic overflow: 18446744073709551615 + 1"
        );
        assert_eq!(
            message(OverflowOp::Sub, true, (-3i64) as u64, 4),
            "arithmetic overflow: -3 - 4"
        );
        assert_eq!(
            message(OverflowOp::Mul, true, 5, (-6i64) as u64),
            "arithmetic overflow: 5 * -6"
        );
        assert_eq!(
            message(
                OverflowOp::Div,
                true,
                (-2147483648i64) as u64,
                (-1i64) as u64
            ),
            "arithmetic overflow: -2147483648 / -1"
        );
        assert_eq!(
            message(OverflowOp::Rem, false, 7, 0),
            "arithmetic overflow: 7 % 0"
        );
        assert_eq!(
            message(OverflowOp::Neg, true, (-2147483648i64) as u64, 0),
            "arithmetic overflow: -(-2147483648)"
        );
        assert_eq!(
            overflow_message(99, true, 1, 2),
            "arithmetic overflow: 1 ? 2"
        );
    }

    #[test]
    fn a_panic_flushes_stdout_and_exits_with_101() {
        let out = child("panic::tests::child_panics");
        assert_eq!(out.status.code(), Some(101));
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "panic at src/Program.mr:3:9: boom\n"
        );
        assert!(String::from_utf8_lossy(&out.stdout).ends_with("before"));
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_panics() {
        only_in_child(|| {
            // No newline: only a flush gets the text out of the buffer.
            print!("before");
            let (file, message) = ("src/Program.mr", "boom");
            // SAFETY: both pointers and lengths come from live string slices.
            unsafe {
                mora_rt_panic(
                    file.as_ptr(),
                    file.len(),
                    3,
                    9,
                    message.as_ptr(),
                    message.len(),
                )
            }
        });
    }

    #[test]
    fn a_panic_without_a_location_prints_only_the_message() {
        let out = child("panic::tests::child_panics_without_location");
        assert_eq!(out.status.code(), Some(101));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "panic: boom\n");
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_panics_without_location() {
        only_in_child(|| {
            let message = "boom";
            // SAFETY: the message pointer and length come from a live slice;
            // a file of length 0 is not read.
            unsafe { mora_rt_panic(std::ptr::null(), 0, 0, 0, message.as_ptr(), message.len()) }
        });
    }

    #[test]
    fn an_overflow_panic_formats_its_operands() {
        let out = child("panic::tests::child_overflows");
        assert_eq!(out.status.code(), Some(101));
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "panic at a.mr:1:2: arithmetic overflow: 2147483647 + 1\n"
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_overflows() {
        only_in_child(|| {
            let file = "a.mr";
            // SAFETY: the file pointer and length come from a live slice.
            unsafe {
                mora_rt_panic_overflow(
                    file.as_ptr(),
                    file.len(),
                    1,
                    2,
                    OverflowOp::Add as u32,
                    1,
                    2147483647,
                    1,
                )
            }
        });
    }

    #[test]
    fn an_overflow_panic_prints_signed_and_unsigned_operands() {
        let out = child("panic::tests::child_overflows_signed");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "panic at a.mr:1:2: arithmetic overflow: -1 - 5\n"
        );
        let out = child("panic::tests::child_overflows_unsigned");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "panic at a.mr:1:2: arithmetic overflow: 18446744073709551615 - 5\n"
        );
    }

    /// Overflows a subtraction of `-1` as 64 bits and 5, in the signedness
    /// `signed`.
    fn overflow_minus_one(signed: u32) -> ! {
        let file = "a.mr";
        // SAFETY: the file pointer and length come from a live slice.
        unsafe {
            mora_rt_panic_overflow(
                file.as_ptr(),
                file.len(),
                1,
                2,
                OverflowOp::Sub as u32,
                signed,
                u64::MAX,
                5,
            )
        }
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_overflows_signed() {
        only_in_child(|| overflow_minus_one(1));
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_overflows_unsigned() {
        only_in_child(|| overflow_minus_one(0));
    }

    #[test]
    fn jit_symbols_are_unique_and_non_null() {
        let symbols = crate::symbols::jit_symbols();
        let mut names: Vec<&str> = symbols.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), symbols.len());
        assert!(symbols.iter().all(|(_, address)| !address.is_null()));
        for name in [
            "mora_rt_panic",
            "mora_rt_panic_overflow",
            "mora_rt_fmod",
            "mora_rt_fmodf",
            "mora_rt_string_concat",
            "mora_rt_string_eq",
        ] {
            assert!(names.contains(&name), "{name}");
        }
    }
}
