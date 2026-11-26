//! How a process that aborts ends on each host, for the tests that run one
//! in a child process: the unit tests (`test_util::aborts_with`) and
//! `tests/exports.rs`, which includes this file by its path.

use std::process::ExitStatus;

/// Whether a process ended in an abort: `SIGABRT`, signal 6 on Linux and
/// macOS.
#[cfg(unix)]
pub(crate) fn aborted(status: ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    status.signal() == Some(6)
}

/// Whether a process ended in an abort: a fast fail, whose exit code is
/// `STATUS_STACK_BUFFER_OVERRUN`.
#[cfg(windows)]
pub(crate) fn aborted(status: ExitStatus) -> bool {
    status.code() == Some(0xC000_0409_u32 as i32)
}
