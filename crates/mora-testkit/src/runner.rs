//! The child process of a JIT run (§14.16). A panic in a Mora program exits
//! the process with code 101, so the harness runs each program in a
//! `jit-runner` process of its own and reads what it printed.
//!
//! The protocol is `jit-runner <shown path> <profile>`, the profile being
//! `debug` or `release`, with the text of the case on standard input. The
//! runner's standard output and error and its exit code are the program's,
//! unless the program has errors that only code generation finds: then it
//! exits with [`COMPILE_ERROR_EXIT`] and writes [`COMPILE_ERROR_PREFIX`] and
//! the diagnostics.

use std::io::{Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mora_codegen::{Cranelift, JitBackend, JitProgram, Profile, Program};

use crate::compiler::{
    Analysis, Checked, FrontEnd, Output, has_error, profile_name, profile_named,
};
use crate::harness::{panic_text, without_panic_output};
use crate::ui::rendered;

/// The exit code of the runner when the program has errors, the code a
/// build that fails exits with (§17.1).
pub const COMPILE_ERROR_EXIT: i32 = 1;

/// The first line the runner writes to standard error when the program has
/// errors; the diagnostics follow, rendered.
pub const COMPILE_ERROR_PREFIX: &str = "jit-runner: the program has errors\n";

/// Why the runner could not build a program.
pub(crate) enum BuildError {
    /// The program's own errors, which the front end or code generation
    /// reported.
    Program(Checked),
    /// A failure of the compiler or of the target.
    Internal(String),
}

/// The exit code of the runner when it could not build the program: the
/// program itself never ran.
pub const INTERNAL_ERROR_EXIT: i32 = 70;

/// What the runner writes to standard error before the reason it could not
/// build the program.
pub const INTERNAL_ERROR_PREFIX: &str = "jit-runner: internal error: ";

/// Runs the program of the case on standard input, shown as the path in the
/// first argument and built in the profile the second names, and exits with
/// its exit code. Without both arguments it exits with a usage error.
///
/// The program is compiled on a thread with the compiler's stack and runs
/// on the main thread, where an executable built ahead of time runs it too
/// (§13.10).
pub fn main() -> ! {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(profile)) = (args.next(), args.next().as_deref().and_then(profile_named))
    else {
        fail("usage: jit-runner <path> <debug|release>, with the program on standard input")
    };
    let mut text = String::new();
    let built = match std::io::stdin().read_to_string(&mut text) {
        Ok(_) => build_quietly(&path, &text, profile),
        Err(error) => Err(BuildError::Internal(format!(
            "cannot read the program: {error}"
        ))),
    };
    match built {
        Ok(jit) => std::process::exit(jit.run_main()),
        Err(BuildError::Program(checked)) => compile_error(&checked),
        Err(BuildError::Internal(reason)) => fail(&reason),
    }
}

/// Reports the errors of the program and exits.
fn compile_error(checked: &Checked) -> ! {
    let mut stderr = std::io::stderr();
    // Nothing is left to report a failed write to.
    let _ = write!(stderr, "{COMPILE_ERROR_PREFIX}{}", rendered(checked));
    let _ = stderr.flush();
    std::process::exit(COMPILE_ERROR_EXIT)
}

/// [`build`] with the panic hook silent: the reason a build fails is written
/// once, by `fail`, and the default hook would repeat it. The hook comes
/// back before the program runs, so a panic of the runtime's own code
/// prints its message.
fn build_quietly(path: &str, text: &str, profile: Profile) -> Result<JitProgram, BuildError> {
    without_panic_output(|| build(path, text, profile))
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
fn build_and_run(path: &str, text: &str) -> Result<i32, BuildError> {
    Ok(build(path, text, Profile::Debug)?.run_main())
}

/// Analyzes `text` as `path` and compiles it in `profile`, on a thread with
/// the compiler's stack. The errors of code generation that are
/// the program's, such as a type too large, join the front end's. A panic of
/// the compiler is an error, not a crash.
pub(crate) fn build(path: &str, text: &str, profile: Profile) -> Result<JitProgram, BuildError> {
    caught(|| FrontEnd::new().analyzed(path, text, |analysis| compile(analysis, profile)))
}

/// Compiles the analysis of [`build`] in `profile`, on the compiler's
/// thread.
fn compile(mut analysis: Analysis, profile: Profile) -> Result<JitProgram, BuildError> {
    if has_error(&analysis.checked.diagnostics) {
        return Err(BuildError::Program(analysis.checked));
    }
    let internal = |error: mora_codegen::CodegenError| BuildError::Internal(error.to_string());
    let program = Program::collect(&analysis.db, analysis.project).map_err(internal)?;
    Cranelift
        .jit(&program, profile)
        .map_err(|error| match error.diagnostic() {
            Some(diagnostic) => {
                analysis.checked.diagnostics.push(diagnostic);
                BuildError::Program(analysis.checked)
            }
            None => internal(error),
        })
}

/// What `compile` gives, with a panic in it as an internal error: its
/// message, or a fixed text for a payload that is not a string.
fn caught<T>(compile: impl FnOnce() -> Result<T, BuildError>) -> Result<T, BuildError> {
    catch_unwind(AssertUnwindSafe(compile)).map_err(|payload| {
        BuildError::Internal(
            panic_text(payload.as_ref()).unwrap_or_else(|| "the compiler panicked".to_owned()),
        )
    })?
}

/// Runs the case in a `runner` process, built in `profile`, and returns what
/// the program did.
///
/// # Panics
///
/// When the process cannot be started, the program runs longer than
/// `timeout`, a signal ends it, or the runner cannot build it. A panic fails
/// only the case.
///
/// # Errors
///
/// The diagnostics the runner wrote, rendered, when the program has errors
/// that only code generation finds.
pub(crate) fn run_child(
    runner: &Path,
    path: &str,
    text: &str,
    profile: Profile,
    timeout: Duration,
) -> Result<Output, String> {
    let mut child = Command::new(runner)
        .args([path, profile_name(profile)])
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
///
/// # Errors
///
/// The rendered diagnostics, when the runner reported that the program has
/// errors.
pub(crate) fn output_of(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<Output, String> {
    let stdout = String::from_utf8_lossy(stdout).into_owned();
    let stderr = String::from_utf8_lossy(stderr).into_owned();
    let Some(exit) = code else {
        panic!("the program was killed by a signal:\n{stderr}");
    };
    if exit == INTERNAL_ERROR_EXIT && stderr.contains(INTERNAL_ERROR_PREFIX) {
        panic!("{stderr}");
    }
    if exit == COMPILE_ERROR_EXIT
        && let Some(shown) = stderr.strip_prefix(COMPILE_ERROR_PREFIX)
    {
        return Err(shown.to_owned());
    }
    Ok(Output {
        stdout,
        stderr,
        exit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Compiler;
    use crate::compiler::PROFILES;
    use crate::harness::{HOOK_MARK, assert_gives_the_hook_back};

    #[test]
    fn output_passes_exit_codes_through() {
        assert_eq!(
            output_of(Some(101), b"x\n", b"panic\n"),
            Ok(Output {
                stdout: "x\n".into(),
                stderr: "panic\n".into(),
                exit: 101
            })
        );
        // A program's own exit code of 1 is not a compile error.
        assert_eq!(output_of(Some(1), b"", b"error: no\n").unwrap().exit, 1);
    }

    #[test]
    #[should_panic(expected = "killed by a signal")]
    fn a_signal_fails_the_case() {
        let _ = output_of(None, b"", b"");
    }

    #[test]
    #[should_panic(expected = "internal error: boom")]
    fn an_internal_error_fails_the_case() {
        let _ = output_of(
            Some(INTERNAL_ERROR_EXIT),
            b"",
            b"jit-runner: internal error: boom\n",
        );
    }

    #[test]
    fn exit_70_without_the_prefix_is_the_program_s_own() {
        assert_eq!(output_of(Some(70), b"", b"other\n").unwrap().exit, 70);
    }

    #[test]
    fn a_compile_error_returns_the_diagnostics() {
        let stderr = format!("{COMPILE_ERROR_PREFIX}error[MR0156]: x\n");
        assert_eq!(
            output_of(Some(COMPILE_ERROR_EXIT), b"", stderr.as_bytes()),
            Err("error[MR0156]: x\n".to_owned())
        );
    }

    #[test]
    fn a_program_runs_and_returns_its_exit_code() {
        assert!(matches!(build_and_run("a.mr", "return 3;\n"), Ok(3)));
    }

    /// A program that nests one construct `n` levels deep, the most the
    /// nesting limit accepts, and what it returns once code generation
    /// lowers it.
    struct Nested {
        name: &'static str,
        text: fn(usize) -> String,
        n: usize,
        returns: Option<i32>,
    }

    /// The constructs whose passes need the most stack a level.
    const NESTED: &[Nested] = &[
        Nested {
            name: "a sum",
            text: |n| format!("let x = 1{};\nreturn x;\n", " + 1".repeat(n)),
            n: 254,
            returns: Some(255),
        },
        Nested {
            name: "parentheses",
            text: |n| format!("return {}1{};\n", "(".repeat(n), ")".repeat(n)),
            n: 254,
            returns: Some(1),
        },
        Nested {
            name: "prefix operators",
            text: |n| format!("let x = {}1;\nreturn x;\n", "- ".repeat(n)),
            n: 254,
            returns: Some(1),
        },
        Nested {
            name: "calls",
            text: |n| {
                let calls = format!("{}1{}", "F(".repeat(n), ")".repeat(n));
                format!("F(x: int): int {{ return x; }}\nreturn {calls};\n")
            },
            n: 254,
            returns: Some(1),
        },
        Nested {
            name: "blocks",
            text: |n| format!("{}{}\nreturn 2;\n", "{ ".repeat(n), "}".repeat(n)),
            n: 256,
            returns: Some(2),
        },
        Nested {
            name: "if statements",
            text: |n| {
                format!(
                    "var x = 0;\n{}x = 1;\nreturn x;\n",
                    "if (x == 0) ".repeat(n)
                )
            },
            n: 253,
            returns: Some(1),
        },
        Nested {
            name: "tuple types",
            text: |n| {
                let ty = format!("{}int, int{}", "(int, ".repeat(n), ")".repeat(n));
                format!("F(t: {ty}) {{}}\nreturn 0;\n")
            },
            n: 255,
            returns: Some(0),
        },
        Nested {
            name: "nested types",
            text: |n| format!("{}{}\nreturn 0;\n", "struct A { ".repeat(n), "}".repeat(n)),
            n: 256,
            returns: Some(0),
        },
        Nested {
            name: "collections",
            text: |n| format!("let x = {}1{};\n", "[".repeat(n), "]".repeat(n)),
            n: 254,
            returns: None,
        },
        Nested {
            name: "lambdas",
            text: |n| format!("let f = {}1;\n", "(y: int) => ".repeat(n)),
            n: 254,
            returns: None,
        },
        Nested {
            name: "`not` patterns",
            text: |n| format!("let y = 1;\nlet x = y is {}1;\n", "not ".repeat(n)),
            n: 252,
            returns: None,
        },
        Nested {
            name: "`or` patterns",
            text: |n| format!("let y = 1;\nlet x = y is 1{};\n", " or 1".repeat(n)),
            n: 252,
            returns: None,
        },
    ];

    #[test]
    fn every_construct_compiles_at_the_nesting_limit_on_the_compiler_stack() {
        // `build` runs on a thread with exactly the compiler's stack, so a
        // pass that needs more at the limit overflows it here.
        for (nested, profile) in NESTED.iter().flat_map(|n| PROFILES.map(|p| (n, p))) {
            let built = build("a.mr", &(nested.text)(nested.n), profile);
            match (built, nested.returns) {
                (Ok(jit), Some(exit)) => assert_eq!(jit.run_main(), exit, "{}", nested.name),
                // A construct a later milestone lowers goes through every
                // pass up to the one that stops on it.
                (Err(BuildError::Internal(reason)), None) => {
                    assert!(
                        reason.contains("is not lowered until"),
                        "{}: {reason}",
                        nested.name
                    );
                }
                (Err(BuildError::Program(checked)), None) => {
                    let codes: Vec<_> = checked.diagnostics.iter().map(|d| d.code).collect();
                    assert!(
                        !codes.contains(&mora_diag::codes::MR0018),
                        "{}",
                        nested.name
                    );
                }
                _ => panic!("{} did not build as expected", nested.name),
            }
            // One more level is past the limit.
            let Err(BuildError::Program(checked)) =
                build("a.mr", &(nested.text)(nested.n + 1), profile)
            else {
                panic!("{} is not at the limit", nested.name);
            };
            assert!(
                checked
                    .diagnostics
                    .iter()
                    .any(|d| d.code == mora_diag::codes::MR0018),
                "{} is not at the limit",
                nested.name
            );
        }
    }

    #[test]
    fn checks_and_builds_leave_the_calling_thread_s_stack() {
        // 254 nested calls take far more than 256 KiB in the front end.
        let nested = NESTED.iter().find(|nested| nested.name == "calls").unwrap();
        let calls = (nested.text)(nested.n);
        let small = std::thread::Builder::new().stack_size(256 << 10);
        let (diagnostics, built) = small
            .spawn(move || {
                let checked = FrontEnd::new().check("a.mr", &calls);
                (
                    checked.diagnostics.len(),
                    build("a.mr", &calls, Profile::Debug).is_ok(),
                )
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!((diagnostics, built), (0, true));
    }

    #[test]
    fn a_question_mark_at_top_level_waits_for_results() {
        // With the `?` the generated `Main` returns a `Result`, which neither
        // the front end nor code generation takes for `()`.
        for text in ["let x = F()?;\n", "let x = F()?;\nreturn x;\n"] {
            let text = format!("F(): Result<int, string> => Ok(1);\n{text}");
            let Err(BuildError::Internal(reason)) = build_and_run("a.mr", &text) else {
                panic!("not an internal error");
            };
            assert_eq!(
                reason,
                "internal error lowering top-level: `?` is not lowered until M3.7"
            );
        }
    }

    #[test]
    fn a_program_with_errors_is_not_built() {
        let Err(BuildError::Program(checked)) = build_and_run("a.mr", "let x = y;\n") else {
            panic!("not the program's errors");
        };
        assert_eq!(checked.diagnostics.len(), 1);
    }

    #[test]
    fn a_program_without_an_entry_point_is_an_error() {
        let Err(BuildError::Internal(reason)) = build_and_run("a.mr", "") else {
            panic!("not an internal error");
        };
        assert_eq!(reason, "the project has no entry point");
    }

    #[test]
    fn a_panic_of_the_compiler_is_an_error() {
        // The panic hook is left alone: a test that changes it races with
        // `a_quiet_build_gives_the_hook_back`.
        let internal = |result: Result<(), BuildError>| match result {
            Err(BuildError::Internal(reason)) => reason,
            _ => panic!("not an internal error"),
        };
        assert_eq!(internal(caught(|| panic!("boom"))), "boom");
        assert_eq!(
            internal(caught(|| std::panic::panic_any(1))),
            "the compiler panicked"
        );
        assert_eq!(
            internal(caught(|| Err(BuildError::Internal("no".to_owned())))),
            "no"
        );
        assert!(caught(|| Ok(())).is_ok());
    }

    #[test]
    fn a_quiet_build_gives_the_hook_back() {
        // A file listed twice makes the analysis panic, on the compiler's
        // thread, with the mark in the path.
        let file = format!("//@ file src/{HOOK_MARK}.mr\n");
        let twice = format!("{file}{file}");
        assert_gives_the_hook_back(|| {
            assert!(build_quietly("a.mr", "return 3;\n", Profile::Debug).is_ok());
            let Err(BuildError::Internal(reason)) = build_quietly("a.mr", &twice, Profile::Debug)
            else {
                panic!("not an internal error");
            };
            assert_eq!(
                reason,
                format!("line 2: `src/{HOOK_MARK}.mr` is listed twice")
            );
        });
    }
}
