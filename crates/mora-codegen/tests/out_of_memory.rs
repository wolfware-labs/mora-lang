//! A program whose first allocation fails panics with `out of memory` and
//! exits with code 101: stdout is set up before the program runs, because
//! the panic flushes it and getting it the first time allocates. The case
//! `jit` runs a program through the JIT, and the case `entry` runs
//! `mora_rt::run_entry`, which the process entry of a built executable
//! calls too; the process entry itself is built only into the runtime
//! libraries, and `cargo xtask runtime --test` checks only its exit code.
//!
//! Each case runs in a child process that re-executes this binary, which
//! has no standard harness, so nothing touches stdout before the program
//! does. The child's allocator refuses a block of [`REFUSED`] bytes once
//! the case arms it, and every block after that. It writes a line for each
//! block it refuses straight to the file of stderr, which allocates
//! nothing, and the parent checks that the one refused block is the
//! string's: when another allocation is on the panic path, its size shows.

// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// The allocator that refuses blocks, in a crate allowed `unsafe` code
// (CONTRIBUTING.md).
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::io::Write;
use std::mem::ManuallyDrop;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

use mora_codegen::{Cranelift, JitBackend, Profile, Program};
use mora_hir::test_support::{TestDb, app};
use mora_rt_core::string::{BYTES_OFFSET, OBJECT_ALIGN};

/// The length of the string each case builds, too long to be inline.
const LEN: usize = 1200;

/// The size of the heap object of that string, which the panic names.
const OBJECT: usize = BYTES_OFFSET + LEN;

/// The block the allocator refuses: the object, after the bytes the
/// runtime's platform layer puts in front of it.
const REFUSED: usize = mora_rt::block_prefix(OBJECT_ALIGN) + OBJECT;

/// How the child's line for a refused block starts.
const REFUSED_LINE: &str = "out_of_memory: refused ";

/// Names the case a child process runs.
const CHILD_ENV: &str = "MORA_OUT_OF_MEMORY_CASE";

/// Set when a case is about to run its program.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Set once the allocator has refused a block.
static REFUSING: AtomicBool = AtomicBool::new(false);

/// The system allocator, which refuses every block from the first one of
/// [`REFUSED`] bytes asked for while [`ARMED`] is set.
struct Refusing;

impl Refusing {
    fn refuses(size: usize) -> bool {
        if ARMED.load(Ordering::SeqCst) && size == REFUSED {
            REFUSING.store(true, Ordering::SeqCst);
        }
        let refuses = REFUSING.load(Ordering::SeqCst);
        if refuses {
            report_refused(size);
        }
        refuses
    }
}

/// Writes `out_of_memory: refused <size> bytes` to stderr, formatted on the
/// stack and written to the file under stderr, past the lock and the buffer
/// of `std::io::stderr`, which may allocate.
fn report_refused(size: usize) {
    let mut line = [0u8; 64];
    let mut rest = &mut line[..];
    // The line fits the buffer.
    let _ = writeln!(rest, "{REFUSED_LINE}{size} bytes");
    let left = rest.len();
    let len = line.len() - left;
    let stderr = ManuallyDrop::new(stderr_file());
    // Nothing is left to report a failed write to.
    let _ = (&*stderr).write_all(&line[..len]);
}

/// The file of stderr, to write to without `std::io::stderr`; never closed,
/// since it is wrapped in a `ManuallyDrop`.
#[cfg(unix)]
fn stderr_file() -> File {
    use std::os::fd::FromRawFd;

    // SAFETY: descriptor 2 is stderr, open for the whole process, and the
    // caller never closes it.
    unsafe { File::from_raw_fd(2) }
}

/// The file of stderr, to write to without `std::io::stderr`; never closed,
/// since it is wrapped in a `ManuallyDrop`.
#[cfg(windows)]
fn stderr_file() -> File {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};

    // SAFETY: the handle is the process's stderr, open for the whole
    // process, and the caller never closes it.
    unsafe { File::from_raw_handle(std::io::stderr().as_raw_handle()) }
}

// SAFETY: every block comes from `System` with the caller's layout, or is
// null, which tells the caller that the allocation failed.
unsafe impl GlobalAlloc for Refusing {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if Self::refuses(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller's contract for `alloc` is `System`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the block came from `System` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if Self::refuses(new_size) {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller's contract for `realloc` is `System`'s.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Refusing = Refusing;

/// A case: runs a program whose first allocation is the refused block and
/// returns its exit code, unless the panic ended the process first.
type Case = fn() -> i32;

/// The cases, by name.
const CASES: &[(&str, Case)] = &[("jit", jit_program), ("entry", entry)];

/// A program compiled through the JIT that builds the string by joining
/// two halves.
fn jit_program() -> i32 {
    let half = "a".repeat(LEN / 2);
    let text = format!("Twice(s: string): string => s + s;\nlet t = Twice(\"{half}\");\n");
    let db = TestDb::default();
    let project = app(&db, &[("src/Program.mr", &text)]);
    let program = Program::collect(&db, project).unwrap();
    let jit = Cranelift.jit(&program, Profile::Debug).unwrap();
    ARMED.store(true, Ordering::SeqCst);
    jit.run_main()
}

/// The runtime's entry point, with an entry that builds the string itself.
fn entry() -> i32 {
    mora_rt::run_entry(|| {
        ARMED.store(true, Ordering::SeqCst);
        let _ = mora_rt::string::new_string(&[b'a'; LEN]);
        0
    })
}

fn main() -> ExitCode {
    if let Ok(name) = std::env::var(CHILD_ENV) {
        let (_, case) = CASES.iter().find(|(case, _)| *case == name).unwrap();
        // A program that returns did not panic; its code tells the parent.
        std::process::exit(case());
    }
    let mut failed = false;
    for (name, _) in CASES {
        let output = Command::new(std::env::current_exe().unwrap())
            .env(CHILD_ENV, name)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let (refused, rest): (Vec<_>, Vec<_>) = stderr
            .split_inclusive('\n')
            .partition(|line| line.starts_with(REFUSED_LINE));
        let rest: String = rest.concat();
        let expected = format!("panic: out of memory allocating {OBJECT} bytes\n");
        let only_the_string = refused == [format!("{REFUSED_LINE}{REFUSED} bytes\n")];
        if output.status.code() == Some(101)
            && rest == expected
            && only_the_string
            && output.stdout.is_empty()
        {
            println!("out of memory: {name} ... ok");
        } else {
            println!(
                "out of memory: {name} ... FAILED: {} with stderr {stderr:?}",
                output.status
            );
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
