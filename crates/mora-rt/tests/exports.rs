//! Every `mora_pal_*` function of the hosted layer, called through the core
//! from a binary that links `mora-rt` as a dependency, as a program does.
//!
//! The unit tests of `mora-rt` call the same functions from a build of the
//! crate of their own. A coverage report keeps, for each exported function,
//! the record of the first binary whose build the profile holds, and every
//! binary that links `mora-rt` as a dependency holds the functions of that
//! build. Calling each one here counts its line whichever build's record is
//! kept.

// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Calls into `mora-rt`, a crate allowed `unsafe` code (CONTRIBUTING.md).
#![allow(unsafe_code)]

use std::process::Command;

use mora_rt_core::pal;
// Defines the `mora_pal_*` functions the core calls.
use mora_rt as _;

#[path = "../src/test_util/abort_status.rs"]
mod abort_status;

use abort_status::aborted;

/// Set in the child process of [`a_diagnostic_then_an_abort_end_the_process`].
const CHILD_ENV: &str = "MORA_RT_EXPORTS_CHILD";

#[test]
fn memory_round_trips() {
    let size = pal::good_size(24, 8);
    assert!(size >= 24);
    // SAFETY: non-zero sizes and a power-of-two alignment; the block is used
    // only within its size and freed with the size of its last resize.
    unsafe {
        let block = pal::alloc(size, 8).unwrap();
        block.as_ptr().write_bytes(7, size);
        let block = pal::realloc(block, size, 8, 2 * size).unwrap();
        assert_eq!(*block.as_ptr().add(size - 1), 7);
        pal::free(block, 2 * size, 8);
    }
}

#[test]
fn the_clock_reads_across_a_wait() {
    let first = pal::now().unwrap();
    pal::wait(Some(0));
    assert!(pal::now().unwrap() >= first);
}

#[test]
fn a_diagnostic_then_an_abort_end_the_process() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "child_writes_and_aborts",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap();
    assert!(aborted(out.status), "{}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "written\n");
}

#[test]
#[ignore = "runs in a child process"]
fn child_writes_and_aborts() {
    if std::env::var_os(CHILD_ENV).is_some() {
        pal::write_diagnostic(b"written\n");
        pal::abort();
    }
}
