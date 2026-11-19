//! Temporary stand-ins for `Console.WriteLine` and for the formatting of
//! `$"..."` holes (§13.4). M1.15 (#43) removes every item of this module: the
//! standard library brings the real output and formatting, and with it the
//! formatting into a stack buffer, which allocates once for a whole
//! interpolated string. These stand-ins instead allocate for a formatted
//! part longer than 15 bytes, and once more for the concatenation; M1.15
//! brings the stack-buffer formatting.

use std::io::Write;

use mora_rt_core::string::MoraString;

use crate::string::new_string;

/// The text of `v`, in the shortest form that reads back as the same
/// `float`. Removed by M1.15 (#43).
pub fn format_f32(v: f32) -> String {
    v.to_string()
}

/// The text of `v`, in the shortest form that reads back as the same
/// `double`. Removed by M1.15 (#43).
pub fn format_f64(v: f64) -> String {
    v.to_string()
}

/// The text of the `char` with the code point `v`; a value that is not one
/// gives U+FFFD. Removed by M1.15 (#43).
fn char_text(v: u32) -> String {
    char::from_u32(v)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
        .to_string()
}

/// The text of the `bool` `v`, which is 0 or 1. Removed by M1.15 (#43).
fn bool_text(v: u32) -> &'static str {
    if v == 0 { "false" } else { "true" }
}

/// Writes `bytes` and a newline to stdout, which is flushed at exit and
/// before a panic. Nothing is left to report a failed write to.
fn write_line(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.write_all(b"\n");
}

/// Prints `v` and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_i64(v: i64) {
    write_line(v.to_string().as_bytes());
}

/// Prints `v` and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_u64(v: u64) {
    write_line(v.to_string().as_bytes());
}

/// Prints `v` and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_f32(v: f32) {
    write_line(format_f32(v).as_bytes());
}

/// Prints `v` and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_f64(v: f64) {
    write_line(format_f64(v).as_bytes());
}

/// Prints `true` or `false` and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_bool(v: u32) {
    write_line(bool_text(v).as_bytes());
}

/// Prints the `char` `v`, in UTF-8, and a newline. Removed by M1.15 (#43).
#[unsafe(no_mangle)]
pub extern "C" fn __mora_standin_write_line_char(v: u32) {
    write_line(char_text(v).as_bytes());
}

/// Prints the text of `s` and a newline. Removed by M1.15 (#43).
///
/// # Safety
///
/// `s` points to a string that is inline or has a live object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_write_line_string(s: *const MoraString) {
    // SAFETY: the caller guarantees the string and its object.
    write_line(unsafe { (*s).as_bytes() });
}

/// Writes to `out` the string of the text `text`.
///
/// # Safety
///
/// `out` is valid for a write of a string.
unsafe fn put(text: &[u8], out: *mut MoraString) {
    // SAFETY: the caller guarantees `out` is valid for a write.
    unsafe { out.write(new_string(text)) };
}

/// Writes to `out` the text of `v`. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_i64(v: i64, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(v.to_string().as_bytes(), out) };
}

/// Writes to `out` the text of `v`. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_u64(v: u64, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(v.to_string().as_bytes(), out) };
}

/// Writes to `out` the text of `v`. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_f32(v: f32, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(format_f32(v).as_bytes(), out) };
}

/// Writes to `out` the text of `v`. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_f64(v: f64, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(format_f64(v).as_bytes(), out) };
}

/// Writes to `out` `true` or `false`. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_bool(v: u32, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(bool_text(v).as_bytes(), out) };
}

/// Writes to `out` the `char` `v`, in UTF-8. Removed by M1.15 (#43).
///
/// # Safety
///
/// `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __mora_standin_format_char(v: u32, out: *mut MoraString) {
    // SAFETY: forwarded from the caller.
    unsafe { put(char_text(v).as_bytes(), out) };
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Output};

    use super::*;

    /// Set in the child process of a test that writes to stdout.
    const CHILD_ENV: &str = "MORA_RT_CHILD";

    /// Runs the ignored test `test` of this crate in a child process.
    fn child(test: &str) -> Output {
        Command::new(std::env::current_exe().unwrap())
            .args([
                test,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap()
    }

    /// Runs `body` in the child process of [`child`] only.
    fn only_in_child(body: impl FnOnce()) {
        if std::env::var_os(CHILD_ENV).is_some() {
            body();
        }
    }

    /// The text of `string`.
    fn text(string: &MoraString) -> String {
        // SAFETY: a built string is alive.
        String::from_utf8(unsafe { string.as_bytes() }.to_vec()).unwrap()
    }

    #[test]
    fn floats_print_in_shortest_round_trip_form() {
        assert_eq!(format_f32(0.1), "0.1");
        assert_eq!(format_f64(2.5), "2.5");
        assert_eq!(format_f64(1.0), "1");
        assert_eq!(format_f64(0.25), "0.25");
    }

    /// The string the entry point `$function` writes for `$value`.
    macro_rules! formatted {
        ($function:ident, $value:expr) => {{
            let mut out = MoraString::EMPTY;
            // SAFETY: `out` is a string to write.
            unsafe { $function($value, &mut out) };
            out
        }};
    }

    #[test]
    fn format_functions_build_strings() {
        let text_of = |s: MoraString| text(&s);
        assert_eq!(text_of(formatted!(__mora_standin_format_i64, -7)), "-7");
        assert_eq!(
            text_of(formatted!(__mora_standin_format_u64, u64::MAX)),
            "18446744073709551615"
        );
        assert_eq!(text_of(formatted!(__mora_standin_format_f32, 0.1)), "0.1");
        assert_eq!(text_of(formatted!(__mora_standin_format_f64, 2.5)), "2.5");
        assert_eq!(text_of(formatted!(__mora_standin_format_bool, 1)), "true");
        assert_eq!(text_of(formatted!(__mora_standin_format_bool, 0)), "false");
        assert_eq!(
            text_of(formatted!(__mora_standin_format_char, 'é' as u32)),
            "é"
        );
        assert_eq!(
            text_of(formatted!(__mora_standin_format_char, 0xD800)),
            "\u{FFFD}"
        );
        // Longer than an inline string holds: a heap string.
        let long = formatted!(__mora_standin_format_f64, 0.1 + 0.2);
        assert_eq!(text(&long), "0.30000000000000004");
        assert!(!long.is_inline());
        assert_eq!(long.len(), 19);
    }

    #[test]
    fn write_line_functions_print_one_line_each() {
        let out = child("standin::tests::child_write_lines");
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines = "-7\n18446744073709551615\n0.1\n2.5\ntrue\nfalse\né\nAda\n";
        assert!(stdout.contains(lines), "{stdout}");
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_write_lines() {
        only_in_child(|| {
            __mora_standin_write_line_i64(-7);
            __mora_standin_write_line_u64(u64::MAX);
            __mora_standin_write_line_f32(0.1);
            __mora_standin_write_line_f64(2.5);
            __mora_standin_write_line_bool(1);
            __mora_standin_write_line_bool(0);
            __mora_standin_write_line_char('é' as u32);
            let name = new_string(b"Ada");
            // SAFETY: `name` is an inline string.
            unsafe { __mora_standin_write_line_string(&name) };
        });
    }

    #[test]
    fn a_child_body_does_not_run_in_the_parent() {
        only_in_child(|| panic!("ran outside a child process"));
    }
}
