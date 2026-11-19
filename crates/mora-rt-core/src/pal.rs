//! The platform interface (§19.4 rule 3): the only way the core reaches the
//! platform. Each platform layer defines these symbols; `mora-rt` does for
//! Linux, macOS and Windows.

use core::ptr::NonNull;

/// The signature of each `mora_pal_*` function, so that a platform layer
/// written in Rust can check its definitions against them.
pub mod signatures {
    pub type Alloc = unsafe extern "C" fn(size: usize, align: usize) -> *mut u8;
    pub type Realloc = unsafe extern "C" fn(
        ptr: *mut u8,
        old_size: usize,
        align: usize,
        new_size: usize,
    ) -> *mut u8;
    pub type Free = unsafe extern "C" fn(ptr: *mut u8, size: usize, align: usize);
    pub type Abort = extern "C" fn() -> !;
    pub type WriteDiagnostic = unsafe extern "C" fn(ptr: *const u8, len: usize);
    pub type Now = extern "C" fn() -> u64;
    pub type Wait = extern "C" fn(timeout_ns: u64);
}

/// `mora_pal_now` returns this on a platform without a clock.
pub const NO_CLOCK: u64 = u64::MAX;

/// `mora_pal_wait` waits without a timeout when given this.
pub const FOREVER: u64 = u64::MAX;

unsafe extern "C" {
    fn mora_pal_alloc(size: usize, align: usize) -> *mut u8;
    fn mora_pal_realloc(ptr: *mut u8, old_size: usize, align: usize, new_size: usize) -> *mut u8;
    fn mora_pal_free(ptr: *mut u8, size: usize, align: usize);
    fn mora_pal_abort() -> !;
    fn mora_pal_write_diagnostic(ptr: *const u8, len: usize);
    fn mora_pal_now() -> u64;
    fn mora_pal_wait(timeout_ns: u64);
}

/// Allocates `size` bytes aligned to `align`, or returns `None` when the
/// platform is out of memory.
///
/// # Safety
///
/// `size` is not zero and `align` is a power of two.
pub unsafe fn alloc(size: usize, align: usize) -> Option<NonNull<u8>> {
    // SAFETY: the caller upholds the platform's preconditions.
    NonNull::new(unsafe { mora_pal_alloc(size, align) })
}

/// Resizes an allocation to `new_size` bytes, keeping its contents up to the
/// smaller size, or returns `None` and leaves it untouched when out of memory.
///
/// # Safety
///
/// `ptr` came from [`alloc`] or [`realloc`] with `old_size` and `align`, is
/// not used after a successful call, and `new_size` is not zero.
pub unsafe fn realloc(
    ptr: NonNull<u8>,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> Option<NonNull<u8>> {
    // SAFETY: the caller upholds the platform's preconditions.
    NonNull::new(unsafe { mora_pal_realloc(ptr.as_ptr(), old_size, align, new_size) })
}

/// Frees an allocation.
///
/// # Safety
///
/// `ptr` came from [`alloc`] or [`realloc`] with `size` and `align`, and is
/// not used afterwards.
pub unsafe fn free(ptr: NonNull<u8>, size: usize, align: usize) {
    // SAFETY: the caller upholds the platform's preconditions.
    unsafe { mora_pal_free(ptr.as_ptr(), size, align) }
}

/// Ends the process at once, without running destructors or flushing output.
pub fn abort() -> ! {
    // SAFETY: the platform's abort has no preconditions.
    unsafe { mora_pal_abort() }
}

/// Writes text where panics and runtime errors go: stderr on hosted
/// platforms.
pub fn write_diagnostic(text: &[u8]) {
    // SAFETY: the pointer and length describe a live slice.
    unsafe { mora_pal_write_diagnostic(text.as_ptr(), text.len()) }
}

/// Monotonic nanoseconds since an arbitrary start, or `None` on a platform
/// without a clock.
pub fn now() -> Option<u64> {
    // SAFETY: the platform's clock has no preconditions.
    let now = unsafe { mora_pal_now() };
    (now != NO_CLOCK).then_some(now)
}

/// The executor's idle hook: blocks until an event the platform watches
/// arrives or `timeout_ns` passes, without a timeout when it is `None`. It
/// may return early, so the caller checks its queues again after each call.
pub fn wait(timeout_ns: Option<u64>) {
    // SAFETY: the platform's wait has no preconditions.
    unsafe { mora_pal_wait(timeout_ns.unwrap_or(FOREVER)) }
}

/// The core's unit tests link against this fake platform instead of
/// `mora-rt`. Each test thread has its own clock and records its own calls.
#[cfg(test)]
mod tests {
    extern crate std;

    use std::cell::{Cell, RefCell};
    use std::vec::Vec;

    use super::*;

    std::thread_local! {
        static CLOCK: Cell<u64> = const { Cell::new(0) };
        static WAITED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
        static WRITTEN: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }

    /// Hands out real blocks; a request the allocator cannot take is null.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mora_pal_alloc(size: usize, align: usize) -> *mut u8 {
        match std::alloc::Layout::from_size_align(size, align) {
            // SAFETY: the caller guarantees a non-zero size.
            Ok(layout) => unsafe { std::alloc::alloc(layout) },
            Err(_) => core::ptr::null_mut(),
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mora_pal_realloc(_: *mut u8, _: usize, _: usize, _: usize) -> *mut u8 {
        core::ptr::null_mut()
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn mora_pal_free(ptr: *mut u8, size: usize, align: usize) {
        // SAFETY: the caller passes the size and alignment the block was
        // allocated with, which `mora_pal_alloc` already took as a valid
        // layout.
        unsafe {
            std::alloc::dealloc(
                ptr,
                std::alloc::Layout::from_size_align_unchecked(size, align),
            )
        }
    }

    #[unsafe(no_mangle)]
    extern "C" fn mora_pal_abort() -> ! {
        std::process::abort()
    }

    /// # Safety
    ///
    /// `ptr` and `len` describe readable memory.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mora_pal_write_diagnostic(ptr: *const u8, len: usize) {
        // SAFETY: the caller guarantees the bytes are readable.
        let text = unsafe { core::slice::from_raw_parts(ptr, len) };
        WRITTEN.with_borrow_mut(|written| written.extend_from_slice(text));
    }

    #[unsafe(no_mangle)]
    extern "C" fn mora_pal_now() -> u64 {
        CLOCK.get()
    }

    #[unsafe(no_mangle)]
    extern "C" fn mora_pal_wait(timeout_ns: u64) {
        WAITED.with_borrow_mut(|waited| waited.push(timeout_ns));
    }

    #[test]
    fn write_diagnostic_passes_the_bytes_through() {
        write_diagnostic(b"panicked: ");
        write_diagnostic(b"boom\n");
        assert_eq!(WRITTEN.take(), b"panicked: boom\n");
    }

    #[test]
    fn now_reads_the_platform_clock() {
        CLOCK.set(0);
        assert_eq!(now(), Some(0));
        CLOCK.set(1_234_567);
        assert_eq!(now(), Some(1_234_567));
        CLOCK.set(NO_CLOCK - 1);
        assert_eq!(now(), Some(NO_CLOCK - 1));
    }

    #[test]
    fn now_is_none_without_a_clock() {
        CLOCK.set(NO_CLOCK);
        assert_eq!(now(), None);
    }

    #[test]
    fn wait_passes_the_timeout_or_forever() {
        wait(Some(1_000));
        wait(None);
        wait(Some(0));
        assert_eq!(WAITED.take(), [1_000, FOREVER, 0]);
    }
}
