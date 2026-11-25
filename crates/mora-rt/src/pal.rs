//! The platform interface of §19.4 on the Rust standard library.

use std::alloc::{self, Layout};
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{fmt, mem, process, ptr, thread};

use mora_rt_core::DEBUG;
use mora_rt_core::pal::{FOREVER, NO_CLOCK, signatures};

use crate::panic::panic_without_location;

// The definitions must match the declarations the core links against.
const _: signatures::Alloc = mora_pal_alloc;
const _: signatures::Realloc = mora_pal_realloc;
const _: signatures::Free = mora_pal_free;
const _: signatures::GoodSize = mora_pal_good_size;
const _: signatures::Abort = mora_pal_abort;
const _: signatures::WriteDiagnostic = mora_pal_write_diagnostic;
const _: signatures::Now = mora_pal_now;
const _: signatures::Wait = mora_pal_wait;

/// What the debug runtime keeps right before each block it hands out: the
/// block's size, then its alignment.
type Record = [usize; 2];

/// The bytes of a [`Record`].
const RECORD: usize = mem::size_of::<Record>();

/// Where a block of `size` bytes aligned to `align` starts in the memory the
/// global allocator hands out, and the layout of that memory; `None` when no
/// such layout exists. The release runtime asks for the block alone. The
/// debug runtime puts its record in front of it, in a prefix rounded up to
/// the alignment, so that `mora_pal_free` and `mora_pal_realloc` can check
/// the size and alignment they are given (§13.2).
pub(crate) fn block(size: usize, align: usize) -> Option<(usize, Layout)> {
    let layout = Layout::from_size_align(size, align).ok()?;
    if !DEBUG {
        return Some((0, layout));
    }
    let prefix = align.max(RECORD);
    let whole = Layout::from_size_align(prefix.checked_add(size)?, prefix).ok()?;
    Some((prefix, whole))
}

/// The block in the memory at `start`, which the global allocator handed
/// out for [`block`]`(size, align)`, after writing the debug runtime's record
/// of `size` and `align` in front of it.
///
/// # Safety
///
/// `start` is null, or points to memory of the layout [`block`] gives for
/// `size` and `align`, which starts `prefix` bytes before the block.
unsafe fn hand_out(start: *mut u8, prefix: usize, size: usize, align: usize) -> *mut u8 {
    if start.is_null() || prefix == 0 {
        return start;
    }
    // SAFETY: the prefix is at least `RECORD` bytes and a multiple of the
    // word size, and `start` is aligned to it, so the record is in the
    // memory and aligned.
    unsafe {
        let block = start.add(prefix);
        record(block).write([size, align]);
        block
    }
}

/// Where the debug runtime keeps the record of the block at `block`.
///
/// # Safety
///
/// `block` is preceded by at least [`RECORD`] bytes of its own memory, as
/// every block the debug runtime hands out is.
unsafe fn record(block: *mut u8) -> *mut Record {
    // SAFETY: the caller guarantees the bytes before the block.
    unsafe { block.cast::<Record>().sub(1) }
}

/// In the debug runtime, checks that the block at `ptr` was allocated with
/// `size` and `align`, and ends the process when it was not: freeing it
/// with another size or alignment would corrupt the heap. `what` is what
/// the caller was about to do with it.
///
/// # Safety
///
/// In the debug runtime, `ptr` came from `mora_pal_alloc` or
/// `mora_pal_realloc`.
unsafe fn check(ptr: *mut u8, size: usize, align: usize, what: &str) {
    if !DEBUG {
        return;
    }
    // SAFETY: the debug runtime keeps the record right before each block.
    let [kept_size, kept_align] = unsafe { record(ptr).read() };
    if (kept_size, kept_align) != (size, align) {
        misuse(format_args!(
            "{what} a block of {kept_size} bytes aligned to {kept_align} as \
             {size} bytes aligned to {align}"
        ));
    }
}

/// Whether `size` is zero, which the contract of `mora_pal_alloc` and
/// `mora_pal_realloc` rules out: the release runtime then answers null, and
/// the debug runtime ends the process. `what` is what the caller was about to
/// do.
fn refuses_zero(size: usize, what: &str) -> bool {
    if size != 0 {
        return false;
    }
    if DEBUG {
        misuse(format_args!("{what} 0 bytes"));
    }
    true
}

/// Reports a misuse of the platform interface, a compiler or runtime bug
/// (§10.11), as a panic. It is found before the heap is touched, so the
/// panic path, which builds nothing on the heap, can still run.
fn misuse(message: fmt::Arguments<'_>) -> ! {
    panic_without_location(format_args!("internal error: {message}"))
}

/// Allocates from the global allocator; see [`mora_rt_core::pal::alloc`].
/// A zero size, which the contract rules out, is null in the release
/// runtime and a misuse in the debug runtime.
///
/// # Safety
///
/// `size` is not zero and `align` is a power of two.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_alloc(size: usize, align: usize) -> *mut u8 {
    if refuses_zero(size, "allocated") {
        return ptr::null_mut();
    }
    match block(size, align) {
        // SAFETY: the layout is not zero-sized, and the memory it gets has
        // that layout.
        Some((prefix, layout)) => unsafe { hand_out(alloc::alloc(layout), prefix, size, align) },
        None => ptr::null_mut(),
    }
}

/// Resizes through the global allocator; see [`mora_rt_core::pal::realloc`].
/// A zero new size, which the contract rules out, is null, with the block
/// untouched, in the release runtime and a misuse in the debug runtime.
///
/// # Safety
///
/// `ptr` came from `mora_pal_alloc` or `mora_pal_realloc` with `old_size` and
/// `align`, and `new_size` is not zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_realloc(
    ptr: *mut u8,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    if refuses_zero(new_size, "resized a block to") {
        return ptr::null_mut();
    }
    // SAFETY: the caller guarantees where the block came from.
    unsafe { check(ptr, old_size, align, "resized") };
    // `realloc` needs the new size to form a valid layout with the same
    // alignment; a size too large for one is out of memory. Both layouts
    // have the same prefix, which depends on the alignment alone.
    match (block(old_size, align), block(new_size, align)) {
        // SAFETY: the caller guarantees the block's size and alignment, so
        // its memory starts `prefix` bytes before it with the old layout;
        // the new layout is valid and not zero-sized.
        (Some((prefix, old)), Some((_, new))) => unsafe {
            let start = alloc::realloc(ptr.sub(prefix), old, new.size());
            hand_out(start, prefix, new_size, align)
        },
        _ => ptr::null_mut(),
    }
}

/// Frees through the global allocator; see [`mora_rt_core::pal::free`].
/// The debug runtime first checks the size and alignment.
///
/// # Safety
///
/// `ptr` came from `mora_pal_alloc` or `mora_pal_realloc` with `size` and
/// `align`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_free(ptr: *mut u8, size: usize, align: usize) {
    // SAFETY: the caller guarantees where the block came from.
    unsafe { check(ptr, size, align, "freed") };
    if let Some((prefix, layout)) = block(size, align) {
        // SAFETY: the caller guarantees the block's size and alignment, so
        // its memory starts `prefix` bytes before it with this layout.
        unsafe { alloc::dealloc(ptr.sub(prefix), layout) }
    }
}

/// The usable size of a block; see [`mora_rt_core::pal::good_size`]. The
/// global allocator has no usable-size query, so this is `size`, the answer
/// §19.4 gives a platform that does not know. The bundled allocator of §13.1,
/// which arrives in M2.3, answers with the size class it hands out.
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_good_size(size: usize, _align: usize) -> usize {
    size
}

/// Aborts the process; see [`mora_rt_core::pal::abort`].
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_abort() -> ! {
    process::abort()
}

/// Writes to stderr; see [`mora_rt_core::pal::write_diagnostic`].
///
/// # Safety
///
/// `ptr` and `len` describe readable memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_write_diagnostic(ptr: *const u8, len: usize) {
    // SAFETY: the caller guarantees the bytes are readable.
    let text = unsafe { std::slice::from_raw_parts(ptr, len) };
    // Nothing useful can be done when stderr is gone.
    let _ = std::io::stderr().write_all(text);
}

/// Nanoseconds since the first call; see [`mora_rt_core::pal::now`].
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_now() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    nanos(START.get_or_init(Instant::now).elapsed())
}

/// `elapsed` in nanoseconds. A process would need to run for 584 years to
/// overflow; even then the clock must not read as `NO_CLOCK`.
fn nanos(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(NO_CLOCK - 1)
}

/// Sleeps until the timeout passes, or parks the thread when there is none;
/// a park may end early, which the contract allows. The reactor that wakes
/// it on I/O arrives with the executor (M6); until then there is nothing
/// else to wait for.
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_wait(timeout_ns: u64) {
    if timeout_ns == FOREVER {
        thread::park();
    } else {
        thread::sleep(Duration::from_nanos(timeout_ns));
    }
}

#[cfg(test)]
mod tests {
    use mora_rt_core::pal;

    use super::*;
    use crate::test_util::{last_allocation, only_in_child, panics_with};

    #[test]
    fn the_debug_runtime_keeps_a_record_in_front_of_each_block() {
        let expect = |prefix, size, align| (prefix, Layout::from_size_align(size, align).unwrap());
        if DEBUG {
            assert_eq!(block(24, 8), Some(expect(RECORD, RECORD + 24, RECORD)));
            assert_eq!(block(24, 64), Some(expect(64, 64 + 24, 64)));
            assert_eq!(block(usize::MAX - 4, 8), None);
        } else {
            assert_eq!(block(24, 8), Some(expect(0, 24, 8)));
            assert_eq!(block(24, 64), Some(expect(0, 24, 64)));
        }
        assert_eq!(block(24, 3), None);
    }

    #[test]
    fn good_size_is_the_size_without_a_usable_size_query() {
        assert_eq!(pal::good_size(1, 1), 1);
        assert_eq!(pal::good_size(60, 4), 60);
        assert_eq!(pal::good_size(4096, 64), 4096);
    }

    #[test]
    fn a_buffer_sized_by_good_size_frees_with_the_size_it_allocated() {
        // A buffer of 12-byte elements aligned to 4 that needs room for 5
        // takes the largest capacity whose size fits the usable size, and
        // allocates exactly the size of that capacity.
        const ELEMENT: usize = 12;
        const ALIGN: usize = 4;
        let capacity = pal::good_size(5 * ELEMENT, ALIGN) / ELEMENT;
        assert!(capacity >= 5);
        // SAFETY: the size is not zero and the alignment a power of two; the
        // buffer is used within its size and freed with the size and
        // alignment it was allocated with, recomputed from the capacity.
        unsafe {
            let buffer = pal::alloc(capacity * ELEMENT, ALIGN).unwrap();
            let (_, layout) = block(capacity * ELEMENT, ALIGN).unwrap();
            assert_eq!(last_allocation(), layout.size());
            buffer.as_ptr().write_bytes(1, capacity * ELEMENT);
            pal::free(buffer, capacity * ELEMENT, ALIGN);
        }
    }

    #[test]
    fn a_wrong_size_or_alignment_panics_in_the_debug_runtime() {
        if !DEBUG {
            return;
        }
        panics_with(
            "pal::tests::child_free_with_another_size",
            "internal error: freed a block of 16 bytes aligned to 8 as 24 bytes aligned to 8",
        );
        panics_with(
            "pal::tests::child_free_with_another_alignment",
            "internal error: freed a block of 16 bytes aligned to 8 as 16 bytes aligned to 16",
        );
        panics_with(
            "pal::tests::child_realloc_with_another_size",
            "internal error: resized a block of 16 bytes aligned to 8 as 32 bytes aligned to 8",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_free_with_another_size() {
        // SAFETY: the debug runtime ends the process before the free.
        only_in_child(|| unsafe { pal::free(pal::alloc(16, 8).unwrap(), 24, 8) });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_free_with_another_alignment() {
        // SAFETY: the debug runtime ends the process before the free.
        only_in_child(|| unsafe { pal::free(pal::alloc(16, 8).unwrap(), 16, 16) });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_realloc_with_another_size() {
        // SAFETY: the debug runtime ends the process before the resize.
        only_in_child(|| unsafe {
            let _ = pal::realloc(pal::alloc(16, 8).unwrap(), 32, 8, 64);
        });
    }

    #[test]
    fn a_zero_size_is_null_in_release_and_panics_in_debug() {
        if DEBUG {
            panics_with(
                "pal::tests::child_alloc_zero",
                "internal error: allocated 0 bytes",
            );
            panics_with(
                "pal::tests::child_realloc_to_zero",
                "internal error: resized a block to 0 bytes",
            );
            return;
        }
        // SAFETY: the zero sizes are the case under test; the block is kept
        // when the resize fails and freed with the layout it was allocated
        // with.
        unsafe {
            assert!(pal::alloc(0, 8).is_none());
            let block = pal::alloc(16, 8).unwrap();
            block.as_ptr().write_bytes(7, 16);
            assert!(pal::realloc(block, 16, 8, 0).is_none());
            assert_eq!(*block.as_ptr().add(15), 7);
            pal::free(block, 16, 8);
        }
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_alloc_zero() {
        only_in_child(|| {
            // SAFETY: the zero size is the case under test.
            let _ = unsafe { pal::alloc(0, 8) };
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_realloc_to_zero() {
        // SAFETY: the zero size is the case under test.
        only_in_child(|| unsafe {
            let _ = pal::realloc(pal::alloc(16, 8).unwrap(), 16, 8, 0);
        });
    }

    #[test]
    fn an_overflowing_clock_stops_short_of_no_clock() {
        assert_eq!(nanos(Duration::from_nanos(7)), 7);
        assert_eq!(nanos(Duration::MAX), NO_CLOCK - 1);
    }

    #[test]
    fn memory_round_trips_through_the_core() {
        // SAFETY: a non-zero size and a power-of-two alignment; the block is
        // used only within its size and freed with the same layout.
        unsafe {
            let block = pal::alloc(16, 8).unwrap();
            assert_eq!(block.as_ptr() as usize % 8, 0);
            block.as_ptr().write_bytes(7, 16);
            let block = pal::realloc(block, 16, 8, 64).unwrap();
            assert_eq!(*block.as_ptr().add(15), 7);
            pal::free(block, 64, 8);
        }
    }

    #[test]
    fn an_oversized_realloc_fails_and_keeps_the_block() {
        // SAFETY: the block is allocated, kept on failure and freed with the
        // layout it was allocated with.
        unsafe {
            let block = pal::alloc(16, 8).unwrap();
            assert!(pal::realloc(block, 16, 8, usize::MAX - 4).is_none());
            pal::free(block, 16, 8);
        }
    }

    #[test]
    fn a_bad_alignment_fails_instead_of_allocating() {
        // SAFETY: the size is non-zero; the alignment is the case under test.
        assert!(unsafe { pal::alloc(16, 3) }.is_none());
    }

    #[test]
    fn a_request_the_allocator_refuses_is_null() {
        // No address space holds a quarter of the addressable bytes, so the
        // global allocator refuses the request.
        let size = usize::MAX / 4;
        // SAFETY: the size is not zero and the alignment a power of two; the
        // block is kept when the resize fails and freed with its layout.
        unsafe {
            assert!(pal::alloc(size, 8).is_none());
            let block = pal::alloc(16, 8).unwrap();
            assert!(pal::realloc(block, 16, 8, size).is_none());
            pal::free(block, 16, 8);
        }
    }

    #[test]
    fn the_clock_moves_forward() {
        let first = pal::now().unwrap();
        pal::wait(Some(1_000_000));
        assert!(pal::now().unwrap() > first);
    }

    #[test]
    fn a_timed_wait_sleeps_for_the_timeout() {
        let start = std::time::Instant::now();
        pal::wait(Some(20_000_000));
        assert!(start.elapsed() >= std::time::Duration::from_millis(20));
    }
}
