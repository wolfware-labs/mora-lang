//! Strings (§13.4): the entry points compiled code calls to build and compare
//! `string` values. The encoding itself is defined in `mora_rt_core::string`.

use core::slice;

use mora_rt_core::alloc::mora_rt_alloc;
use mora_rt_core::string::{BYTES_OFFSET, INLINE_MAX, MoraString, OBJECT_ALIGN, object_header};

use crate::panic::mora_rt_panic;

/// The most bytes a string holds (§13.4).
const MAX_LEN: usize = i32::MAX as usize;

/// Panics with `message`, which has no source location.
fn raise(message: &str) -> ! {
    // SAFETY: the message pointer and length come from a live string; a file
    // of length 0 is not read.
    unsafe { mora_rt_panic(core::ptr::null(), 0, 0, 0, message.as_ptr(), message.len()) }
}

/// The total length of strings of the lengths `lens`, or `None` when it is
/// above `i32::MAX`, which `concat` and `new_string` report as a panic.
pub fn concat_len(lens: impl IntoIterator<Item = usize>) -> Option<usize> {
    lens.into_iter()
        .try_fold(0usize, |total, len| total.checked_add(len))
        .filter(|total| *total <= MAX_LEN)
}

/// The total of `lens`; a string that would be too long panics.
fn total_len(lens: impl IntoIterator<Item = usize>) -> usize {
    concat_len(lens).unwrap_or_else(|| raise("string too long"))
}

/// `object`, which `mora_rt_alloc` returned for `size` bytes; a null one
/// panics: the program is out of memory.
fn checked(object: *mut u8, size: usize) -> *mut u8 {
    if object.is_null() {
        raise(&format!("out of memory allocating {size} bytes"));
    }
    object
}

/// Copies `parts` one after the other into `to`, which is as long as they
/// are together.
fn copy_parts<'a>(to: &mut [u8], parts: impl Iterator<Item = &'a [u8]>) {
    let mut at = 0;
    for part in parts {
        to[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
}

/// The string of `total` bytes that `parts` make: inline when it fits, and
/// otherwise one object of `BYTES_OFFSET + total` bytes with one strong and
/// one weak count.
fn join<'a>(total: usize, parts: impl Iterator<Item = &'a [u8]> + Clone) -> MoraString {
    let mut small = [0; INLINE_MAX];
    let filled = small
        .get_mut(..total)
        .map(|to| copy_parts(to, parts.clone()));
    if let Some(string) = filled.and_then(|()| MoraString::inline(&small[..total])) {
        return string;
    }
    let size = BYTES_OFFSET + total;
    // SAFETY: the size is not zero and the alignment is a power of two.
    let object = checked(unsafe { mora_rt_alloc(size, OBJECT_ALIGN) }, size);
    // `total` is at most `i32::MAX`.
    let len = total as u32;
    let header = object_header(1, 1, len, cfg!(target_endian = "big"));
    // SAFETY: the object holds `size` bytes, the header and `total` more, and
    // nothing else refers to it yet.
    let bytes = unsafe {
        object.copy_from_nonoverlapping(header.as_ptr(), BYTES_OFFSET);
        slice::from_raw_parts_mut(object.add(BYTES_OFFSET), total)
    };
    copy_parts(bytes, parts);
    MoraString::heap(object, len)
}

/// The string of the text `bytes`: inline when it fits, and otherwise a
/// counted heap object (§13.4). Running out of memory, or a text above
/// `i32::MAX` bytes, panics.
pub fn new_string(bytes: &[u8]) -> MoraString {
    join(total_len([bytes.len()]), core::iter::once(bytes))
}

/// Writes to `out` the concatenation of the `count` strings at `parts`,
/// built with one allocation at most (§13.4). `out` may be one of the parts.
/// A result above `i32::MAX` bytes panics with `string too long`.
///
/// # Safety
///
/// `parts` points to `count` strings, each of which is inline or has a live
/// object, and `out` is valid for a write of a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_rt_string_concat(
    parts: *const MoraString,
    count: usize,
    out: *mut MoraString,
) {
    // SAFETY: the caller guarantees `count` strings at `parts`.
    let parts = unsafe { slice::from_raw_parts(parts, count) };
    let total = total_len(parts.iter().map(MoraString::len));
    // SAFETY: each string's object is alive and holds its bytes.
    let bytes = parts.iter().map(|part| unsafe { part.as_bytes() });
    let string = join(total, bytes);
    // SAFETY: the caller guarantees `out` is valid for a write.
    unsafe { out.write(string) };
}

/// Whether the strings at `a` and `b` hold the same text: their lengths
/// first, then their bytes.
///
/// # Safety
///
/// Each points to a string that is inline or has a live object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_rt_string_eq(a: *const MoraString, b: *const MoraString) -> bool {
    // SAFETY: the caller guarantees both strings and their objects.
    unsafe { (*a).as_bytes() == (*b).as_bytes() }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Output};

    use mora_rt_core::string::{
        BYTES_OFFSET, CAPACITY_OFFSET, IMMORTAL, MoraString, STRONG_OFFSET, WEAK_OFFSET,
    };

    use super::*;

    /// Set in the child process of a test that exits the process.
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

    /// What a process that ends in the panic `test` runs prints.
    fn panics_with(test: &str, message: &str) {
        let out = child(&format!("string::tests::{test}"));
        assert_eq!(out.status.code(), Some(101), "{test}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            format!("panic: {message}\n"),
            "{test}"
        );
    }

    /// `a + b`, built by the entry point generated code calls.
    fn concat(parts: &[MoraString]) -> MoraString {
        let mut out = MoraString::EMPTY;
        // SAFETY: `parts` holds `parts.len()` strings, whose objects are
        // alive, and `out` is a string to write.
        unsafe { mora_rt_string_concat(parts.as_ptr(), parts.len(), &mut out) };
        out
    }

    /// The `u32` at `offset` of the heap object of `string`.
    fn word(string: &MoraString, offset: usize) -> u32 {
        // SAFETY: a heap object starts with 12 readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(string.object().add(offset), 4) };
        u32::from_ne_bytes(bytes.try_into().unwrap())
    }

    #[test]
    fn short_concatenations_stay_inline() {
        let s = concat(&[new_string(b"Hello"), new_string(b", ")]);
        assert!(s.is_inline());
        assert_eq!(s.len(), 7);
        assert_eq!(s.raw(), MoraString::inline(b"Hello, ").unwrap().raw());
        // SAFETY: an inline string needs no object.
        assert_eq!(unsafe { s.as_bytes() }, b"Hello, ");
    }

    #[test]
    fn long_concatenations_allocate_one_counted_object() {
        let s = concat(&[new_string(b"0123456789"), new_string(b"abcdefghij")]);
        assert!(!s.is_inline());
        assert_eq!(s.len(), 20);
        assert_eq!(word(&s, STRONG_OFFSET), 1);
        assert_eq!(word(&s, WEAK_OFFSET), 1);
        assert_eq!(word(&s, CAPACITY_OFFSET), 20);
        // SAFETY: the object holds 20 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(s.object().add(BYTES_OFFSET), 20) };
        assert_eq!(bytes, b"0123456789abcdefghij");
    }

    #[test]
    fn many_parts_and_empty_parts_join_in_order() {
        let parts = [
            new_string(b"ab"),
            MoraString::EMPTY,
            new_string(b"cdefgh"),
            new_string(b"ijklmnop"),
        ];
        let s = concat(&parts);
        // SAFETY: the object holds 16 bytes.
        assert_eq!(unsafe { s.as_bytes() }, b"abcdefghijklmnop");
        assert_eq!(concat(&[]).raw(), MoraString::EMPTY.raw());
    }

    #[test]
    fn the_inline_limit_counts_bytes() {
        let accents = "ééééééé";
        assert_eq!(accents.len(), 14);
        assert_eq!("é".len(), 2);
        let fits = concat(&[new_string(accents.as_bytes()), new_string(b"a")]);
        assert!(fits.is_inline());
        assert_eq!(fits.len(), 15);
        let spills = concat(&[new_string(accents.as_bytes()), new_string(b"ab")]);
        assert!(!spills.is_inline());
        assert_eq!(spills.len(), 16);
    }

    #[test]
    fn new_strings_are_inline_when_they_fit() {
        assert!(new_string(&[b'a'; 15]).is_inline());
        let long = new_string(&[b'a'; 16]);
        assert!(!long.is_inline());
        assert_eq!(word(&long, CAPACITY_OFFSET), 16);
        assert_eq!(word(&long, STRONG_OFFSET), 1);
        assert_eq!(new_string(b"").raw(), MoraString::EMPTY.raw());
        assert_ne!(IMMORTAL, 1);
    }

    #[test]
    fn concat_len_stops_at_int_max() {
        assert_eq!(concat_len([i32::MAX as usize, 1]), None);
        assert_eq!(concat_len([3, 4]), Some(7));
        assert_eq!(concat_len([i32::MAX as usize]), Some(i32::MAX as usize));
        assert_eq!(concat_len([usize::MAX, 1]), None);
        assert_eq!(concat_len([]), Some(0));
    }

    #[test]
    fn equality_compares_text() {
        let eq = |a: &MoraString, b: &MoraString| {
            // SAFETY: both strings are alive.
            unsafe { mora_rt_string_eq(a, b) }
        };
        let (a, b) = (new_string(b"Ada"), new_string(b"Ada"));
        assert!(eq(&a, &b));
        assert!(!eq(&a, &new_string(b"Adb")));
        assert!(!eq(&a, &new_string(b"Ad")));
        let (x, y) = (new_string(&[b'x'; 20]), new_string(&[b'x'; 20]));
        assert_ne!(x.object(), y.object());
        assert!(eq(&x, &y));
        let mut z = [b'x'; 20];
        z[19] = b'y';
        assert!(!eq(&x, &new_string(&z)));
        assert!(!eq(&x, &new_string(&[b'x'; 21])));
        // An inline string and a heap string never hold the same text.
        assert!(!eq(&a, &x));
    }

    #[test]
    fn a_concatenation_too_long_panics() {
        panics_with("child_too_long", "string too long");
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_too_long() {
        only_in_child(|| {
            // The objects are never read: the length is checked first.
            let nowhere = std::ptr::null();
            let parts = [
                MoraString::heap(nowhere, i32::MAX as u32),
                MoraString::heap(nowhere, 1),
            ];
            concat(&parts);
        });
    }

    #[test]
    fn an_allocation_that_fails_panics_with_its_size() {
        panics_with("child_out_of_memory", "out of memory allocating 52 bytes");
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_out_of_memory() {
        only_in_child(|| {
            checked(std::ptr::null_mut(), 52);
        });
    }

    #[test]
    fn the_platform_refuses_a_request_too_large_for_a_layout() {
        // SAFETY: the size is not zero and the alignment is a power of two.
        assert!(unsafe { mora_rt_alloc(usize::MAX - 4, OBJECT_ALIGN) }.is_null());
    }

    #[test]
    fn a_block_that_came_back_is_kept() {
        let mut byte = 0u8;
        assert_eq!(checked(&mut byte, 1), &raw mut byte);
    }
}
