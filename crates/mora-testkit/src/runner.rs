//! The child process of a JIT run (§14.16). A panic in a Mora program exits
//! the process with code 101, so the harness runs each program in a
//! `jit-runner` process of its own and reads what it printed.
//!
//! The protocol is `jit-runner <shown path>` with the text of the case on
//! standard input. The runner's standard output and error and its exit code
//! are the program's.

use std::any::Any;
use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mora_codegen::{Cranelift, JitBackend, JitProgram, Profile, Program};

use crate::compiler::{FrontEnd, Output, has_error};

/// The exit code of the runner when it could not build the program: the
/// program itself never ran.
pub const INTERNAL_ERROR_EXIT: i32 = 70;

/// What the runner writes to standard error before the reason it could not
/// build the program.
pub const INTERNAL_ERROR_PREFIX: &str = "jit-runner: internal error: ";

/// Runs the program of the case on standard input, shown as the path in the
/// first argument, and exits with its exit code. Without the argument it
/// exits with a usage error.
pub fn main() -> ! {
    let Some(path) = std::env::args().nth(1) else {
        fail("usage: jit-runner <path>, with the program on standard input")
    };
    let mut text = String::new();
    let built = match std::io::stdin().read_to_string(&mut text) {
        Ok(_) => build_quietly(&path, &text),
        Err(error) => Err(format!("cannot read the program: {error}")),
    };
    match built {
        Ok(jit) => std::process::exit(jit.run_main()),
        Err(reason) => fail(&reason),
    }
}

/// [`build`] with the panic hook silent: the reason a build fails is written
/// once, by `fail`, and the default hook would repeat it. The hook comes
/// back before the program runs, so a panic of the runtime's own code
/// prints its message.
fn build_quietly(path: &str, text: &str) -> Result<JitProgram, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let jit = build(path, text);
    std::panic::set_hook(hook);
    jit
}

/// Reports `reason` as an internal error and exits.
fn fail(reason: &str) -> ! {
    let mut stderr = std::io::stderr();
    // Nothing is left to report a failed write to.
    let _ = writeln!(stderr, "{INTERNAL_ERROR_PREFIX}{reason}");
    let _ = stderr.flush();
    std::process::exit(INTERNAL_ERROR_EXIT)
}

/// Analyzes `text` as `path`, compiles it with the debug profile and runs it,
/// returning its exit code. A panic of the compiler is an error, not a crash.
#[cfg(test)]
fn build_and_run(path: &str, text: &str) -> Result<i32, String> {
    Ok(build(path, text)?.run_main())
}

/// Analyzes `text` as `path` and compiles it with the debug profile. A panic
/// of the compiler is an error, not a crash.
fn build(path: &str, text: &str) -> Result<JitProgram, String> {
    catch_unwind(AssertUnwindSafe(|| {
        let analysis = FrontEnd::new().analyze(path, text);
        if has_error(&analysis.checked.diagnostics) {
            return Err("the program has errors".to_owned());
        }
        let program =
            Program::collect(&analysis.db, analysis.project).map_err(|error| error.to_string())?;
        Cranelift
            .jit(&program, Profile::Debug)
            .map_err(|error| error.to_string())
    }))
    .map_err(|payload| message_of(payload.as_ref()))?
}

/// The message of a panic.
fn message_of(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "the compiler panicked".to_owned())
}

/// Runs the case in a `runner` process and returns what the program did.
///
/// # Panics
///
/// When the process cannot be started, the program runs longer than
/// `timeout`, a signal ends it, or the runner cannot build it. A panic fails
/// only the case.
pub(crate) fn run_child(runner: &Path, path: &str, text: &str, timeout: Duration) -> Output {
    let mut child = Command::new(runner)
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", runner.display()));
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // The pipes are read and written on threads of their own, so a program
    // that fills one cannot stop the others.
    let text = text.to_owned();
    let writer = thread::spawn(move || {
        let mut stdin = stdin;
        // A program that exits without reading its input closes the pipe.
        let _ = stdin.write_all(text.as_bytes());
    });
    let stdout = thread::spawn(move || read_all(stdout));
    let stderr = thread::spawn(move || read_all(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() >= timeout {
            // The program may have just exited.
            let _ = child.kill();
            let _ = child.wait();
            panic!("the program ran longer than {} s", timeout.as_secs_f64());
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = writer.join();
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    output_of(status.code(), &stdout, &stderr)
}

/// Everything `pipe` yields until it closes.
fn read_all(mut pipe: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    // A pipe that fails part way leaves what it read, which the case reports.
    let _ = pipe.read_to_end(&mut bytes);
    bytes
}

/// What a finished runner left: its exit code (`None` when a signal ended it)
/// and what it wrote to standard output and error.
///
/// # Panics
///
/// When a signal ended the runner, or it could not build the program, which
/// fails the case.
pub(crate) fn output_of(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Output {
    let stdout = String::from_utf8_lossy(stdout).into_owned();
    let stderr = String::from_utf8_lossy(stderr).into_owned();
    let Some(exit) = code else {
        panic!("the program was killed by a signal:\n{stderr}");
    };
    if exit == INTERNAL_ERROR_EXIT && stderr.contains(INTERNAL_ERROR_PREFIX) {
        panic!("{stderr}");
    }
    Output {
        stdout,
        stderr,
        exit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_passes_exit_codes_through() {
        assert_eq!(
            output_of(Some(101), b"x\n", b"panic\n"),
            Output {
                stdout: "x\n".into(),
                stderr: "panic\n".into(),
                exit: 101
            }
        );
    }

    #[test]
    #[should_panic(expected = "killed by a signal")]
    fn a_signal_fails_the_case() {
        output_of(None, b"", b"");
    }

    #[test]
    #[should_panic(expected = "internal error: boom")]
    fn an_internal_error_fails_the_case() {
        output_of(
            Some(INTERNAL_ERROR_EXIT),
            b"",
            b"jit-runner: internal error: boom\n",
        );
    }

    #[test]
    fn exit_70_without_the_prefix_is_the_program_s_own() {
        assert_eq!(output_of(Some(70), b"", b"other\n").exit, 70);
    }

    #[test]
    fn a_program_runs_and_returns_its_exit_code() {
        assert_eq!(build_and_run("a.mr", "return 3;\n"), Ok(3));
    }

    #[test]
    fn a_program_with_errors_is_not_built() {
        assert_eq!(
            build_and_run("a.mr", "let x = y;\n"),
            Err("the program has errors".to_owned())
        );
    }

    #[test]
    fn a_program_without_an_entry_point_is_an_error() {
        assert!(build_and_run("a.mr", "").is_err());
    }

    #[test]
    fn a_quiet_build_gives_the_hook_back() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static CALLED: AtomicBool = AtomicBool::new(false);
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| CALLED.store(true, Ordering::SeqCst)));
        assert!(build_quietly("a.mr", "return 3;\n").is_ok());
        assert!(build_quietly("a.mr", "").is_err());
        let _ = catch_unwind(|| panic!("after the build"));
        std::panic::set_hook(original);
        assert!(CALLED.load(Ordering::SeqCst));
    }

    #[test]
    fn the_message_of_a_panic_is_its_text() {
        assert_eq!(message_of(&"a"), "a");
        assert_eq!(message_of(&String::from("b")), "b");
        assert_eq!(message_of(&1), "the compiler panicked");
    }
}
