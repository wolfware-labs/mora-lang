//! Helpers shared by the tests of this crate: a test whose entry point ends
//! the process, as a panic does, or writes to stdout, runs in a child
//! process that re-executes the test binary. The test binary's allocator
//! records the layout of the last block each thread asked for, the last
//! block it resized and the last block it freed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::panic::PANIC_EXIT_CODE;
use crate::{DEBUG, block_prefix};

mod abort_status;

use abort_status::aborted;

std::thread_local! {
    /// The size and alignment of the last block this thread asked the
    /// allocator for.
    static LAST_ALLOCATION: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
    /// The address, old size and alignment of the last block this thread
    /// resized.
    static LAST_RESIZE: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
    /// The address, size and alignment of the last block this thread freed.
    static LAST_FREE: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

/// The system allocator, recording the layout of each block it hands out,
/// the old layout of each block it resizes and of each block it takes back.
struct Recording;

// SAFETY: every call goes to `System` as it came.
unsafe impl GlobalAlloc for Recording {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // A thread being torn down has no record to keep.
        let _ = LAST_ALLOCATION.try_with(|last| last.set((layout.size(), layout.align())));
        // SAFETY: the caller's contract for `alloc` is `System`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = LAST_ALLOCATION.try_with(|last| last.set((layout.size(), layout.align())));
        // SAFETY: the caller's contract for `alloc_zeroed` is `System`'s.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = LAST_ALLOCATION.try_with(|last| last.set((new_size, layout.align())));
        let _ = LAST_RESIZE.try_with(|last| last.set(memory(ptr, layout)));
        // SAFETY: the block came from `System` with this layout, and the
        // caller's contract for `realloc` is `System`'s.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = LAST_FREE.try_with(|last| last.set(memory(ptr, layout)));
        // SAFETY: the block came from `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

/// The address, size and alignment of the memory at `ptr` with `layout`.
fn memory(ptr: *mut u8, layout: Layout) -> (usize, usize, usize) {
    (ptr as usize, layout.size(), layout.align())
}

#[global_allocator]
static ALLOCATOR: Recording = Recording;

/// The size and alignment of the last block this thread asked the
/// allocator for.
pub(crate) fn last_allocation() -> (usize, usize) {
    LAST_ALLOCATION.get()
}

/// The address, old size and alignment of the last block this thread
/// resized.
pub(crate) fn last_resize() -> (usize, usize, usize) {
    LAST_RESIZE.get()
}

/// The address, size and alignment of the last block this thread freed.
pub(crate) fn last_free() -> (usize, usize, usize) {
    LAST_FREE.get()
}

/// The size and alignment of the memory the platform layer asks the global
/// allocator for to hold a block of `size` bytes aligned to `align`: the
/// block alone in the release runtime, and the block behind the prefix of
/// its record, aligned to the prefix, in the debug runtime.
pub(crate) fn memory_layout(size: usize, align: usize) -> (usize, usize) {
    let prefix = block_prefix(align);
    (prefix + size, if DEBUG { prefix } else { align })
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

/// The writes to the diagnostic stream so far in this process.
static DIAGNOSTIC_WRITES: AtomicUsize = AtomicUsize::new(0);

/// What a child process prints on stdout, before the count of its writes to
/// the diagnostic stream, when it aborts.
const WRITES_LINE: &str = "diagnostic writes: ";

/// Counts a write to the diagnostic stream.
pub(crate) fn count_diagnostic_write() {
    DIAGNOSTIC_WRITES.fetch_add(1, Ordering::Relaxed);
}

/// Prints the writes to the diagnostic stream so far on stdout, as the
/// process is about to abort, for [`aborts_with`] to read.
pub(crate) fn print_diagnostic_writes() {
    let mut stdout = std::io::stdout();
    let writes = DIAGNOSTIC_WRITES.load(Ordering::Relaxed);
    let _ = writeln!(stdout, "{WRITES_LINE}{writes}");
    let _ = stdout.flush();
}

/// Runs the ignored test `test` of this crate in a child process, and
/// checks that it aborts after printing `message` and nothing else, in a
/// single write so that no other output lands inside it, as the runtime
/// reports its own bugs (§10.11).
pub(crate) fn aborts_with(test: &str, message: &str) {
    let out = child(test);
    assert!(aborted(out.status), "{test}: {}", out.status);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("{message}\n"),
        "{test}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let writes = stdout
        .rsplit_once(WRITES_LINE)
        .map(|(_, count)| count.trim_end());
    assert_eq!(writes, Some("1"), "{test}: {stdout}");
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
