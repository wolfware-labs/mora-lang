//! The platform interface of §19.4 on the Rust standard library.

use std::alloc::{self, Layout};
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{process, ptr, thread};

use mora_rt_core::pal::{FOREVER, NO_CLOCK, signatures};

// The definitions must match the declarations the core links against.
const _: signatures::Alloc = mora_pal_alloc;
const _: signatures::Realloc = mora_pal_realloc;
const _: signatures::Free = mora_pal_free;
const _: signatures::Abort = mora_pal_abort;
const _: signatures::WriteDiagnostic = mora_pal_write_diagnostic;
const _: signatures::Now = mora_pal_now;
const _: signatures::Wait = mora_pal_wait;

/// Allocates from the global allocator; see [`mora_rt_core::pal::alloc`].
///
/// # Safety
///
/// `size` is not zero and `align` is a power of two.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_alloc(size: usize, align: usize) -> *mut u8 {
    match Layout::from_size_align(size, align) {
        // SAFETY: the caller guarantees a non-zero size.
        Ok(layout) => unsafe { alloc::alloc(layout) },
        Err(_) => ptr::null_mut(),
    }
}

/// Resizes through the global allocator; see [`mora_rt_core::pal::realloc`].
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
    // `realloc` needs the new size to form a valid layout with the same
    // alignment; a size too large for one is out of memory.
    match (
        Layout::from_size_align(old_size, align),
        Layout::from_size_align(new_size, align),
    ) {
        // SAFETY: the caller guarantees the allocation's layout and a
        // non-zero new size, and the new layout is valid.
        (Ok(layout), Ok(_)) => unsafe { alloc::realloc(ptr, layout, new_size) },
        _ => ptr::null_mut(),
    }
}

/// Frees through the global allocator; see [`mora_rt_core::pal::free`].
///
/// # Safety
///
/// `ptr` came from `mora_pal_alloc` or `mora_pal_realloc` with `size` and
/// `align`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_free(ptr: *mut u8, size: usize, align: usize) {
    if let Ok(layout) = Layout::from_size_align(size, align) {
        // SAFETY: the caller guarantees the allocation's layout.
        unsafe { alloc::dealloc(ptr, layout) }
    }
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
    use std::time::Duration;

    use mora_rt_core::pal::{self, NO_CLOCK};

    #[test]
    fn an_overflowing_clock_stops_short_of_no_clock() {
        assert_eq!(super::nanos(Duration::from_nanos(7)), 7);
        assert_eq!(super::nanos(Duration::MAX), NO_CLOCK - 1);
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
