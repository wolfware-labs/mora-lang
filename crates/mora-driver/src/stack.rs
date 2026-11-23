//! The stack the compiler runs on (§14.4).

use std::io;
use std::panic;
use std::thread;

/// The stack size, in bytes, of the thread the compiler runs on: 16 MiB.
///
/// The nesting limit of §3.1 bounds how deep each pass recurses. At the
/// limit, the pass that needs the most stack in a debug build of the
/// compiler on x86-64 Linux needs 1.9 MiB: lowering 254 nested calls to
/// HIR, and then to MIR, takes 7.3 KiB a level each. In a release build
/// MIR lowering needs the most, 550 KiB, 2.1 KiB a level. The main thread
/// of a process has 1 MiB on Windows and 8 MiB on Linux and macOS, so the
/// compiler does not run on it. 16 MiB holds eight times the debug need,
/// for targets whose frames are larger and for the passes still to come.
pub const COMPILER_STACK_SIZE: usize = 16 << 20;

/// Runs `compile` on a thread with [`COMPILER_STACK_SIZE`] bytes of stack,
/// named `compiler`, and returns what it returns. A panic in `compile`
/// continues on the calling thread.
///
/// # Errors
///
/// When the thread cannot be started.
pub fn on_compiler_stack<T: Send>(compile: impl FnOnce() -> T + Send) -> io::Result<T> {
    thread::scope(|scope| {
        let compiler = thread::Builder::new()
            .name("compiler".to_owned())
            .stack_size(COMPILER_STACK_SIZE)
            .spawn_scoped(scope, compile)?;
        match compiler.join() {
            Ok(compiled) => Ok(compiled),
            Err(payload) => panic::resume_unwind(payload),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_on_a_thread_named_compiler() {
        let name = on_compiler_stack(|| thread::current().name().map(str::to_owned));
        assert_eq!(name.ok().flatten().as_deref(), Some("compiler"));
    }

    /// Recurses, 4 KiB at a time, until the stack below `top`, the address
    /// of a local at the start, holds `need` bytes, and returns how many it
    /// holds then. Measured, so that debug and release frames of different
    /// sizes reach the same depth of stack.
    fn deep(top: usize, need: usize) -> usize {
        let frame = std::hint::black_box([1u8; 4096]);
        let used = top.abs_diff(frame.as_ptr() as usize);
        if used >= need {
            return used;
        }
        // Used after the call, so the frame stays live through it.
        deep(top, need).max(usize::from(std::hint::black_box(frame)[0]))
    }

    #[test]
    fn the_compiler_stack_holds_what_a_thread_s_default_does_not() {
        // 6 MiB is past the 2 MiB a spawned thread gets by default, the
        // 1 MiB of a Windows main thread and the 8 MiB of a Linux one less
        // what the harness uses, and well within 16 MiB. A smaller stack
        // overflows, which aborts the test process.
        const NEED: usize = 6 << 20;
        let used = on_compiler_stack(|| {
            let top = std::hint::black_box(0u8);
            deep(std::ptr::addr_of!(top) as usize, NEED)
        })
        .unwrap();
        assert!((NEED..NEED + (64 << 10)).contains(&used), "{used}");
    }

    #[test]
    fn a_panic_continues_on_the_calling_thread() {
        let caught = panic::catch_unwind(|| on_compiler_stack(|| panic!("in the compiler")));
        let payload = caught.err().and_then(|p| p.downcast::<&str>().ok());
        assert_eq!(payload.as_deref(), Some(&"in the compiler"));
    }
}
