//! Helpers shared by the tests of this crate: a test whose entry point ends
//! the process, as a panic does, or writes to stdout, runs in a child
//! process that re-executes the test binary. The test binary's allocator
//! records the size of the last block each thread asked for.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::process::{Command, Output};

use crate::panic::PANIC_EXIT_CODE;

std::thread_local! {
    /// The size of the last block this thread asked the allocator for.
    static LAST_ALLOCATION: Cell<usize> = const { Cell::new(0) };
}

/// The system allocator, recording the size of each block it hands out.
struct Recording;

// SAFETY: every call goes to `System` as it came.
unsafe impl GlobalAlloc for Recording {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // A thread being torn down has no record to keep.
        let _ = LAST_ALLOCATION.try_with(|last| last.set(layout.size()));
        // SAFETY: the caller's contract for `alloc` is `System`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = LAST_ALLOCATION.try_with(|last| last.set(layout.size()));
        // SAFETY: the caller's contract for `alloc_zeroed` is `System`'s.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = LAST_ALLOCATION.try_with(|last| last.set(new_size));
        // SAFETY: the block came from `System` with this layout, and the
        // caller's contract for `realloc` is `System`'s.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the block came from `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Recording = Recording;

/// The size of the last block this thread asked the allocator for.
pub(crate) fn last_allocation() -> usize {
    LAST_ALLOCATION.get()
}

/// Set in the child process of a test.
const CHILD_ENV: &str = "MORA_RT_CHILD";

/// Runs the ignored test `test` of this crate in a child process, and
/// returns what it wrote and how it ended.
pub(crate) fn child(test: &str) -> Output {
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

/// Runs the ignored test `test` of this crate in a child process, and
/// checks that it ends in a panic with no source location that prints
/// `message`.
pub(crate) fn panics_with(test: &str, message: &str) {
    let out = child(test);
    assert_eq!(out.status.code(), Some(PANIC_EXIT_CODE), "{test}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("panic: {message}\n"),
        "{test}"
    );
}

/// Runs `body` in the child process of [`child`] only, so that the ignored
/// test that holds it returns at once when it runs on its own.
pub(crate) fn only_in_child(body: impl FnOnce()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_body_does_not_run_in_the_parent() {
        only_in_child(|| panic!("ran outside a child process"));
    }
}
