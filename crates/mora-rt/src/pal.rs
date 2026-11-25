//! The platform interface of §19.4 on the Rust standard library.

use std::alloc::{self, Layout};
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use std::{fmt, mem, process, ptr, thread};

use mora_rt_core::DEBUG;
use mora_rt_core::pal::{FOREVER, NO_CLOCK, signatures};

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

/// The bytes in front of a block aligned to `align` that hold its
/// [`Record`]: the record, padded to the alignment so that the block stays
/// aligned.
const fn record_prefix(align: usize) -> usize {
    if align > RECORD { align } else { RECORD }
}

/// The bytes the hosted platform layer puts in front of each block aligned
/// to `align`, which the global allocator is asked for on top of the block:
/// the record's prefix in the debug runtime (§13.2), none in the release
/// runtime.
pub const fn block_prefix(align: usize) -> usize {
    if DEBUG { record_prefix(align) } else { 0 }
}

/// Where a block of `size` bytes aligned to `align` starts in memory that
/// holds its [`Record`] in front of it, in a prefix of [`record_prefix`]
/// bytes, and the layout of that memory; `None` when no such layout exists.
/// The record lets `mora_pal_free` and `mora_pal_realloc` check the size and
/// alignment they are given.
fn recorded(size: usize, align: usize) -> Option<(usize, Layout)> {
    Layout::from_size_align(size, align).ok()?;
    let prefix = record_prefix(align);
    let whole = Layout::from_size_align(prefix.checked_add(size)?, prefix).ok()?;
    Some((prefix, whole))
}

/// The block in the memory at `start`, which the global allocator handed
/// out for [`recorded`]`(size, align)`, after writing the record of `size`
/// and `align` right before it; null when `start` is.
///
/// # Safety
///
/// `start` is null, or points to memory of the layout [`recorded`] gives for
/// `size` and `align`, whose prefix is `prefix` bytes.
unsafe fn hand_out(start: *mut u8, prefix: usize, size: usize, align: usize) -> *mut u8 {
    if start.is_null() {
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

/// Where the record of the block at `block` is kept.
///
/// # Safety
///
/// `block` is preceded by at least [`RECORD`] bytes of its own memory, as
/// every block [`hand_out`] returns is.
unsafe fn record(block: *mut u8) -> *mut Record {
    // SAFETY: the caller guarantees the bytes before the block.
    unsafe { block.cast::<Record>().sub(1) }
}

/// Checks that the record of the block at `ptr` says `size` and `align`, and
/// ends the process when it does not: freeing the block with another size or
/// alignment would corrupt the heap. `what` is what the caller was about to
/// do with it. The debug runtime checks each free and resize this way.
///
/// # Safety
///
/// `ptr` came from [`hand_out`].
unsafe fn check(ptr: *mut u8, size: usize, align: usize, what: &str) {
    // SAFETY: the caller guarantees the record before the block.
    let [kept_size, kept_align] = unsafe { record(ptr).read() };
    if kept_size != size || kept_align != align {
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
/// (§10.11): writes `internal error: <message>` to the diagnostic stream and
/// aborts. A user panic would run the panic path, where a test that expects
/// a panic would take the bug for it. The line is formatted into a [`Line`]
/// on the stack, since this runs inside the allocator, and written at once,
/// so that no other output lands inside it.
fn misuse(message: fmt::Arguments<'_>) -> ! {
    let mut line = Line::new();
    // A message too long for the line is cut, which `finish` marks.
    let _ = fmt::Write::write_fmt(&mut line, format_args!("internal error: {message}"));
    mora_rt_core::pal::write_diagnostic(line.finish());
    mora_pal_abort()
}

/// The bytes of a [`Line`], its newline included.
const LINE_BYTES: usize = 256;

/// What ends a [`Line`] whose text did not fit, in place of the newline.
const CUT: &str = "...\n";

/// The bytes of text a [`Line`] holds, which leaves room for [`CUT`].
const LINE_TEXT: usize = LINE_BYTES - CUT.len();

/// A line of text formatted into a fixed buffer, with no heap. Text that
/// does not fit is cut between two characters, and nothing is added after
/// the cut.
struct Line {
    /// The text, then room for [`CUT`].
    bytes: [u8; LINE_BYTES],
    /// The bytes of text written.
    len: usize,
    /// Whether some text did not fit.
    cut: bool,
}

impl Line {
    /// An empty line.
    const fn new() -> Self {
        Self {
            bytes: [0; LINE_BYTES],
            len: 0,
            cut: false,
        }
    }

    /// The text, ended by a newline, or by [`CUT`] when some text did not
    /// fit.
    fn finish(&mut self) -> &[u8] {
        let end: &[u8] = if self.cut { CUT.as_bytes() } else { b"\n" };
        let len = self.len + end.len();
        self.bytes[self.len..len].copy_from_slice(end);
        &self.bytes[..len]
    }
}

impl fmt::Write for Line {
    /// Adds what fits of `text`; an error once some of it does not, which
    /// stops the formatting.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.cut {
            return Err(fmt::Error);
        }
        let room = LINE_TEXT - self.len;
        let fits = text.floor_char_boundary(room);
        self.bytes[self.len..self.len + fits].copy_from_slice(&text.as_bytes()[..fits]);
        self.len += fits;
        self.cut = fits < text.len();
        if self.cut { Err(fmt::Error) } else { Ok(()) }
    }
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
    // SAFETY: the caller's contract is the same.
    unsafe { alloc_block(size, align) }
}

/// The body of [`mora_pal_alloc`]. Each `mora_pal_*` function only calls a
/// body under a name of its own. A coverage report adds up the counts of a
/// mangled function across the test binaries, but keeps those of the first
/// binary it reads for an unmangled symbol. The unit tests of this crate
/// build the symbol apart from every binary that links the crate, so
/// `tests/exports.rs` calls each export from such a binary, and its one line
/// counts whichever binary comes first. Inlined into the exported function,
/// so the export costs no extra call.
///
/// # Safety
///
/// As for [`mora_pal_alloc`].
#[inline]
unsafe fn alloc_block(size: usize, align: usize) -> *mut u8 {
    if refuses_zero(size, "allocated") {
        return ptr::null_mut();
    }
    if DEBUG {
        // SAFETY: the caller's contract is the same.
        return unsafe { alloc_recorded(size, align) };
    }
    let Ok(layout) = Layout::from_size_align(size, align) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout is not zero-sized.
    unsafe { alloc::alloc(layout) }
}

/// The debug runtime's `mora_pal_alloc` past the zero-size check: allocates
/// a block of `size` bytes aligned to `align` with its [`Record`] in front;
/// null when the global allocator refuses or no such layout exists. Public,
/// and hidden, for the benchmarks, which build the release runtime.
///
/// # Safety
///
/// `size` is not zero.
#[doc(hidden)]
pub unsafe fn alloc_recorded(size: usize, align: usize) -> *mut u8 {
    let Some((prefix, layout)) = recorded(size, align) else {
        return ptr::null_mut();
    };
    // SAFETY: the layout is not zero-sized, and the memory it gets has the
    // layout `recorded` gives.
    unsafe { hand_out(alloc::alloc(layout), prefix, size, align) }
}

/// The debug runtime's `mora_pal_realloc` past the zero-size check: checks
/// the block's record, then resizes it with its record in front; null, with
/// the block untouched, when the global allocator refuses or no layout of
/// `new_size` exists. Public, and hidden, for the benchmarks.
///
/// # Safety
///
/// `ptr` came from [`alloc_recorded`] or `realloc_recorded`, and `new_size`
/// is not zero.
#[doc(hidden)]
pub unsafe fn realloc_recorded(
    ptr: *mut u8,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    // SAFETY: the caller guarantees where the block came from.
    unsafe { check(ptr, old_size, align, "resized") };
    // Both layouts have the same prefix, which depends on the alignment
    // alone.
    let (Some((prefix, old)), Some((_, new))) =
        (recorded(old_size, align), recorded(new_size, align))
    else {
        return ptr::null_mut();
    };
    // SAFETY: the record matched, so the block's memory starts `prefix`
    // bytes before it with the old layout; the new layout is valid and not
    // zero-sized, and the memory it gets has the layout `recorded` gives.
    unsafe {
        let start = alloc::realloc(ptr.sub(prefix), old, new.size());
        hand_out(start, prefix, new_size, align)
    }
}

/// The debug runtime's `mora_pal_free`: checks the block's record, then
/// frees the block with it. Public, and hidden, for the benchmarks.
///
/// # Safety
///
/// `ptr` came from [`alloc_recorded`] or `realloc_recorded`.
#[doc(hidden)]
pub unsafe fn free_recorded(ptr: *mut u8, size: usize, align: usize) {
    // SAFETY: the caller guarantees where the block came from.
    unsafe { check(ptr, size, align, "freed") };
    if let Some((prefix, layout)) = recorded(size, align) {
        // SAFETY: the record matched, so the block's memory starts `prefix`
        // bytes before it with this layout.
        unsafe { alloc::dealloc(ptr.sub(prefix), layout) }
    }
}

/// Resizes through the global allocator; see [`mora_rt_core::pal::realloc`].
/// A zero new size, which the contract rules out, is null, with the block
/// untouched, in the release runtime and a misuse in the debug runtime,
/// which also checks the old size and the alignment.
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
    // SAFETY: the caller's contract is the same.
    unsafe { realloc_block(ptr, old_size, align, new_size) }
}

/// The body of [`mora_pal_realloc`]; see [`alloc_block`] for why it is
/// apart.
///
/// # Safety
///
/// As for [`mora_pal_realloc`].
#[inline]
unsafe fn realloc_block(ptr: *mut u8, old_size: usize, align: usize, new_size: usize) -> *mut u8 {
    if refuses_zero(new_size, "resized a block to") {
        return ptr::null_mut();
    }
    if DEBUG {
        // SAFETY: the caller's contract is the same.
        return unsafe { realloc_recorded(ptr, old_size, align, new_size) };
    }
    // `realloc` needs the new size to form a valid layout with the same
    // alignment; a size too large for one is out of memory.
    let (Ok(old), Ok(_)) = (
        Layout::from_size_align(old_size, align),
        Layout::from_size_align(new_size, align),
    ) else {
        return ptr::null_mut();
    };
    // SAFETY: the caller guarantees the block's layout; the new one is valid
    // and not zero-sized.
    unsafe { alloc::realloc(ptr, old, new_size) }
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
    // SAFETY: the caller's contract is the same.
    unsafe { free_block(ptr, size, align) }
}

/// The body of [`mora_pal_free`]; see [`alloc_block`] for why it is apart.
///
/// # Safety
///
/// As for [`mora_pal_free`].
#[inline]
unsafe fn free_block(ptr: *mut u8, size: usize, align: usize) {
    if DEBUG {
        // SAFETY: the caller's contract is the same.
        return unsafe { free_recorded(ptr, size, align) };
    }
    if let Ok(layout) = Layout::from_size_align(size, align) {
        // SAFETY: the caller guarantees the block's layout.
        unsafe { alloc::dealloc(ptr, layout) }
    }
}

/// The usable size of a block; see [`mora_rt_core::pal::good_size`]. The
/// global allocator has no usable-size query, so this is `size`, the answer
/// §19.4 gives a platform that does not know. The bundled allocator of §13.1,
/// which arrives in M2.3, answers with the size class it hands out.
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_good_size(size: usize, align: usize) -> usize {
    usable_size(size, align)
}

/// The body of [`mora_pal_good_size`]; see [`alloc_block`] for why it is
/// apart.
#[inline]
fn usable_size(size: usize, _align: usize) -> usize {
    size
}

/// Aborts the process; see [`mora_rt_core::pal::abort`].
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_abort() -> ! {
    abort_process()
}

/// The body of [`mora_pal_abort`]; see [`alloc_block`] for why it is apart.
#[inline]
fn abort_process() -> ! {
    // The tests read how many writes came before the abort.
    #[cfg(test)]
    crate::test_util::print_diagnostic_writes();
    // A build instrumented for coverage writes its counts at exit, which an
    // abort skips: write them first, since the tests of the paths that
    // abort run them in a child process. Kept inside this function, which
    // cargo-mutants leaves alone, as no `cargo test` build compiles it.
    #[cfg(coverage)]
    {
        unsafe extern "C" {
            /// The profiler runtime's writer of the counts, linked into
            /// every instrumented build.
            fn __llvm_profile_write_file() -> i32;
        }
        // SAFETY: the writer has no preconditions; a failure loses only
        // counts.
        let _ = unsafe { __llvm_profile_write_file() };
    }
    process::abort()
}

/// Writes to stderr; see [`mora_rt_core::pal::write_diagnostic`].
///
/// # Safety
///
/// `ptr` and `len` describe readable memory.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_pal_write_diagnostic(ptr: *const u8, len: usize) {
    // SAFETY: the caller's contract is the same.
    unsafe { write_stderr(ptr, len) }
}

/// The body of [`mora_pal_write_diagnostic`]; see [`alloc_block`] for why it
/// is apart.
///
/// # Safety
///
/// As for [`mora_pal_write_diagnostic`].
#[inline]
unsafe fn write_stderr(ptr: *const u8, len: usize) {
    // The tests check that an internal error takes one write.
    #[cfg(test)]
    crate::test_util::count_diagnostic_write();
    // SAFETY: the caller guarantees the bytes are readable.
    let text = unsafe { std::slice::from_raw_parts(ptr, len) };
    // Nothing useful can be done when stderr is gone.
    let _ = std::io::stderr().write_all(text);
}

/// Nanoseconds since the first call; see [`mora_rt_core::pal::now`].
#[unsafe(no_mangle)]
pub extern "C" fn mora_pal_now() -> u64 {
    nanos_since_first_call()
}

/// The body of [`mora_pal_now`]; see [`alloc_block`] for why it is apart.
#[inline]
fn nanos_since_first_call() -> u64 {
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
    park_or_sleep(timeout_ns)
}

/// The body of [`mora_pal_wait`]; see [`alloc_block`] for why it is apart.
#[inline]
fn park_or_sleep(timeout_ns: u64) {
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
    use crate::test_util::{aborts_with, last_allocation, last_free, only_in_child};

    #[test]
    fn a_record_takes_a_prefix_padded_to_the_alignment() {
        let expect = |prefix, size, align| (prefix, Layout::from_size_align(size, align).unwrap());
        assert_eq!(record_prefix(1), RECORD);
        assert_eq!(record_prefix(RECORD), RECORD);
        assert_eq!(record_prefix(64), 64);
        assert_eq!(recorded(24, 8), Some(expect(RECORD, RECORD + 24, RECORD)));
        assert_eq!(recorded(24, 64), Some(expect(64, 64 + 24, 64)));
        assert_eq!(recorded(usize::MAX - 4, 8), None);
        assert_eq!(recorded(24, 3), None);
    }

    #[test]
    fn only_the_debug_runtime_asks_for_the_record() {
        for align in [1, 8, 64] {
            let expected = if DEBUG { record_prefix(align) } else { 0 };
            assert_eq!(block_prefix(align), expected);
            // SAFETY: the size is not zero and the alignment a power of two;
            // the block is freed with them.
            unsafe {
                let block = pal::alloc(24, align).unwrap();
                assert_eq!(last_allocation(), expected + 24);
                pal::free(block, 24, align);
            }
        }
    }

    /// Hands out a recorded block of `size` bytes aligned to `align` from
    /// memory filled with `0xAA`, runs `body` on it, and frees the memory.
    fn with_recorded(size: usize, align: usize, body: impl FnOnce(*mut u8, *mut u8)) {
        let (prefix, layout) = recorded(size, align).unwrap();
        // SAFETY: the layout is not zero-sized; the memory is freed with it.
        unsafe {
            let start = alloc::alloc(layout);
            assert!(!start.is_null());
            start.write_bytes(0xAA, layout.size());
            let block = hand_out(start, prefix, size, align);
            body(start, block);
            alloc::dealloc(start, layout);
        }
    }

    #[test]
    fn the_record_is_the_two_words_right_before_the_block() {
        for align in [1, 8, 16, 32, 64, 128] {
            with_recorded(24, align, |start, block| {
                assert_eq!(block as usize - start as usize, record_prefix(align));
                assert_eq!(block as usize % align, 0);
                // SAFETY: the prefix holds at least the two words before
                // the block, and the memory at `start` its first word.
                unsafe {
                    let words = block.cast::<usize>().sub(2);
                    assert_eq!([words.read(), words.add(1).read()], [24, align]);
                    assert_eq!(record(block).read(), [24, align]);
                    if record_prefix(align) > RECORD {
                        assert_eq!(
                            start.cast::<usize>().read(),
                            usize::from_ne_bytes([0xAA; mem::size_of::<usize>()])
                        );
                    }
                }
            });
        }
    }

    #[test]
    fn the_record_path_round_trips_in_every_build() {
        for align in [8, 32, 64, 4096] {
            // SAFETY: non-zero sizes and a power-of-two alignment; the block
            // is used only within its size and freed with the size and
            // alignment of its last resize.
            unsafe {
                let block = alloc_recorded(24, align);
                assert_eq!(last_allocation(), record_prefix(align) + 24);
                assert_eq!(block as usize % align, 0);
                assert_eq!(record(block).read(), [24, align]);
                block.write_bytes(7, 24);
                let block = realloc_recorded(block, 24, align, 3000);
                assert_eq!(last_allocation(), record_prefix(align) + 3000);
                assert_eq!(block as usize % align, 0);
                assert_eq!(record(block).read(), [3000, align]);
                assert_eq!(*block.add(23), 7);
                block.add(24).write_bytes(9, 3000 - 24);
                let block = realloc_recorded(block, 3000, align, 40);
                assert_eq!(record(block).read(), [40, align]);
                assert_eq!((*block.add(23), *block.add(39)), (7, 9));
                free_recorded(block, 40, align);
                let prefix = record_prefix(align);
                assert_eq!(last_free(), (block as usize - prefix, prefix + 40));
            }
        }
    }

    #[test]
    fn the_record_path_refuses_what_it_cannot_lay_out() {
        // SAFETY: the alignment and the new size are the cases under test;
        // the block is kept when the resize fails and freed with its layout.
        unsafe {
            assert!(alloc_recorded(16, 3).is_null());
            let block = alloc_recorded(16, 8);
            block.write_bytes(7, 16);
            assert!(realloc_recorded(block, 16, 8, usize::MAX - 4).is_null());
            assert_eq!((record(block).read(), *block.add(15)), ([16, 8], 7));
            free_recorded(block, 16, 8);
        }
    }

    #[test]
    fn the_record_path_checks_each_free_and_resize() {
        aborts_with(
            "pal::tests::child_free_recorded_with_another_size",
            "internal error: freed a block of 16 bytes aligned to 8 as 24 bytes aligned to 8",
        );
        aborts_with(
            "pal::tests::child_realloc_recorded_with_another_alignment",
            "internal error: resized a block of 16 bytes aligned to 8 as 16 bytes aligned to 64",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_free_recorded_with_another_size() {
        // SAFETY: the check ends the process before the free.
        only_in_child(|| unsafe { free_recorded(alloc_recorded(16, 8), 24, 8) });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_realloc_recorded_with_another_alignment() {
        // SAFETY: the check ends the process before the resize.
        only_in_child(|| unsafe {
            let _ = realloc_recorded(alloc_recorded(16, 8), 16, 64, 32);
        });
    }

    #[test]
    fn handing_out_no_memory_is_null() {
        // SAFETY: a null start is passed through untouched.
        assert!(unsafe { hand_out(ptr::null_mut(), 16, 24, 8) }.is_null());
    }

    #[test]
    fn a_check_of_the_recorded_size_and_alignment_passes() {
        // SAFETY: the block was handed out with its record.
        with_recorded(24, 64, |_, block| unsafe { check(block, 24, 64, "freed") });
    }

    #[test]
    fn a_check_of_another_size_or_alignment_aborts() {
        aborts_with(
            "pal::tests::child_check_another_size",
            "internal error: freed a block of 24 bytes aligned to 64 as 32 bytes aligned to 64",
        );
        aborts_with(
            "pal::tests::child_check_another_alignment",
            "internal error: resized a block of 24 bytes aligned to 64 as 24 bytes aligned to 8",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_check_another_size() {
        only_in_child(|| {
            // SAFETY: the block was handed out with its record.
            with_recorded(24, 64, |_, block| unsafe { check(block, 32, 64, "freed") });
        });
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_check_another_alignment() {
        only_in_child(|| {
            // SAFETY: the block was handed out with its record.
            with_recorded(24, 64, |_, block| unsafe { check(block, 24, 8, "resized") });
        });
    }

    #[test]
    fn a_misuse_writes_an_internal_error_and_aborts() {
        aborts_with(
            "pal::tests::child_misuse",
            "internal error: misused 3 times",
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_misuse() {
        only_in_child(|| misuse(format_args!("misused {} times", 3)));
    }

    /// The text of `line` once finished.
    fn finished(mut line: Line) -> String {
        String::from_utf8(line.finish().to_vec()).unwrap()
    }

    #[test]
    fn a_line_that_fits_ends_with_a_newline() {
        let mut line = Line::new();
        fmt::Write::write_fmt(&mut line, format_args!("misused {} times", 3)).unwrap();
        assert_eq!(finished(line), "misused 3 times\n");
    }

    #[test]
    fn a_line_that_fills_the_buffer_exactly_is_not_cut() {
        let text = "x".repeat(LINE_TEXT);
        let mut line = Line::new();
        fmt::Write::write_str(&mut line, &text).unwrap();
        assert_eq!(finished(line), format!("{text}\n"));
    }

    #[test]
    fn a_line_too_long_is_cut_and_marked() {
        let mut line = Line::new();
        assert!(fmt::Write::write_str(&mut line, &"x".repeat(LINE_TEXT + 1)).is_err());
        assert_eq!(finished(line), format!("{}{CUT}", "x".repeat(LINE_TEXT)));
    }

    #[test]
    fn nothing_is_added_to_a_line_once_it_is_cut() {
        let mut line = Line::new();
        let _ = fmt::Write::write_str(&mut line, &"x".repeat(LINE_TEXT - 2));
        assert!(fmt::Write::write_str(&mut line, "yyy").is_err());
        assert!(fmt::Write::write_str(&mut line, "z").is_err());
        assert_eq!(
            finished(line),
            format!("{}yy{CUT}", "x".repeat(LINE_TEXT - 2))
        );
    }

    #[test]
    fn a_line_is_cut_between_characters() {
        // A three-byte character that would straddle the end of the room.
        let mut line = Line::new();
        let _ = fmt::Write::write_str(&mut line, &"x".repeat(LINE_TEXT - 2));
        assert!(fmt::Write::write_str(&mut line, "\u{20ac}").is_err());
        // The two bytes left would hold this, but the line is cut.
        assert!(fmt::Write::write_str(&mut line, "z").is_err());
        assert_eq!(
            finished(line),
            format!("{}{CUT}", "x".repeat(LINE_TEXT - 2))
        );
    }

    #[test]
    fn a_misuse_too_long_for_a_line_is_cut() {
        let head = "internal error: ";
        aborts_with(
            "pal::tests::child_misuse_too_long",
            &format!(
                "{head}{}{}",
                "m".repeat(LINE_TEXT - head.len()),
                CUT.trim_end()
            ),
        );
    }

    #[test]
    #[ignore = "runs in a child process"]
    fn child_misuse_too_long() {
        only_in_child(|| misuse(format_args!("{}", "m".repeat(LINE_BYTES))));
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
            assert_eq!(last_allocation(), block_prefix(ALIGN) + capacity * ELEMENT);
            buffer.as_ptr().write_bytes(1, capacity * ELEMENT);
            pal::free(buffer, capacity * ELEMENT, ALIGN);
        }
    }

    #[test]
    fn a_wrong_size_or_alignment_aborts_in_the_debug_runtime() {
        if !DEBUG {
            return;
        }
        aborts_with(
            "pal::tests::child_free_with_another_size",
            "internal error: freed a block of 16 bytes aligned to 8 as 24 bytes aligned to 8",
        );
        aborts_with(
            "pal::tests::child_free_with_another_alignment",
            "internal error: freed a block of 16 bytes aligned to 8 as 16 bytes aligned to 16",
        );
        aborts_with(
            "pal::tests::child_realloc_with_another_size",
            "internal error: resized a block of 16 bytes aligned to 8 as 32 bytes aligned to 8",
        );
        aborts_with(
            "pal::tests::child_realloc_with_another_alignment",
            "internal error: resized a block of 16 bytes aligned to 8 as 16 bytes aligned to 64",
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
    #[ignore = "runs in a child process"]
    fn child_realloc_with_another_alignment() {
        // SAFETY: the debug runtime ends the process before the resize.
        only_in_child(|| unsafe {
            let _ = pal::realloc(pal::alloc(16, 8).unwrap(), 16, 64, 32);
        });
    }

    #[test]
    fn a_zero_size_is_null_in_release_and_aborts_in_debug() {
        if DEBUG {
            aborts_with(
                "pal::tests::child_alloc_zero",
                "internal error: allocated 0 bytes",
            );
            aborts_with(
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
    fn memory_round_trips_above_the_record_alignment() {
        for align in [32, 64, 4096] {
            // SAFETY: non-zero sizes and a power-of-two alignment; the block
            // is used only within its size and freed with the size and
            // alignment of its last resize.
            unsafe {
                let block = pal::alloc(24, align).unwrap();
                assert_eq!(block.as_ptr() as usize % align, 0);
                block.as_ptr().write_bytes(7, 24);
                let block = pal::realloc(block, 24, align, 3000).unwrap();
                assert_eq!(block.as_ptr() as usize % align, 0);
                assert_eq!(*block.as_ptr().add(23), 7);
                block.as_ptr().add(24).write_bytes(9, 3000 - 24);
                let block = pal::realloc(block, 3000, align, 40).unwrap();
                assert_eq!(block.as_ptr() as usize % align, 0);
                assert_eq!((*block.as_ptr().add(23), *block.as_ptr().add(39)), (7, 9));
                pal::free(block, 40, align);
            }
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
        // A wait that parked instead would return at once.
        thread::current().unpark();
        pal::wait(Some(1_000_000));
        assert!(pal::now().unwrap() > first);
    }

    #[test]
    fn a_timed_wait_sleeps_for_the_timeout() {
        // A wait that parked instead would return at once.
        thread::current().unpark();
        let start = std::time::Instant::now();
        pal::wait(Some(20_000_000));
        assert!(start.elapsed() >= std::time::Duration::from_millis(20));
    }

    #[test]
    fn a_wait_without_a_timeout_parks_until_unparked() {
        let (done, finished) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            pal::wait(None);
            done.send(()).unwrap();
        });
        waiter.thread().unpark();
        // A wait that slept instead would never end; the test then fails
        // here, and the process ends with the thread still asleep.
        finished
            .recv_timeout(Duration::from_secs(10))
            .expect("the wait ended");
        waiter.join().unwrap();
    }
}
