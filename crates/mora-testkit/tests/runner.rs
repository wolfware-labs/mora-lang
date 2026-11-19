//! The `jit-runner` process and the front end that starts it.

use std::time::Duration;

use mora_testkit::{Backend, Compiler, FrontEnd};

fn front_end() -> FrontEnd {
    FrontEnd::with_runner(env!("CARGO_BIN_EXE_jit-runner").into())
}

#[test]
fn the_runner_runs_a_program_and_returns_its_output() {
    let output = front_end()
        .run(
            "a.mr",
            "Main(): int {\n    Console.WriteLine(1);\n    return 3;\n}\n",
            Backend::Jit,
        )
        .unwrap();
    assert_eq!(output.stdout, "1\n");
    assert_eq!(output.stderr, "");
    assert_eq!(output.exit, 3);
}

#[test]
fn a_panic_reaches_stderr_with_exit_101() {
    let text = "Console.WriteLine(\"before\");\nlet max = 2147483647;\nlet one = 1;\nlet sum = max + one;\nConsole.WriteLine(sum);\n";
    let output = front_end().run("a.mr", text, Backend::Jit).unwrap();
    assert_eq!(output.stdout, "before\n");
    assert!(
        output
            .stderr
            .starts_with("panic at src/Program.mr:4:11: arithmetic overflow"),
        "{}",
        output.stderr
    );
    assert_eq!(output.exit, 101);
}

#[test]
fn a_program_with_errors_is_not_run() {
    let checked = front_end()
        .run("a.mr", "let x = y;\n", Backend::Jit)
        .unwrap_err();
    assert_eq!(checked.diagnostics.len(), 1);
}

#[test]
#[should_panic(expected = "ran longer than")]
fn a_program_that_runs_too_long_fails_its_case() {
    let _ = front_end().with_timeout(Duration::from_millis(500)).run(
        "a.mr",
        "while (true) {}\n",
        Backend::Jit,
    );
}

#[test]
#[should_panic(expected = "cannot start")]
fn a_missing_runner_fails_the_case() {
    let _ = FrontEnd::with_runner("/nonexistent/jit-runner".into()).run(
        "a.mr",
        "return 0;\n",
        Backend::Jit,
    );
}

#[test]
#[should_panic(expected = "internal error")]
fn a_program_the_runner_cannot_build_fails_the_case() {
    // The parent only analyzes; a program with no entry point passes that and
    // the runner reports it.
    let _ = front_end().run("a.mr", "", Backend::Jit);
}

#[test]
fn input_that_is_not_text_is_an_internal_error() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_jit-runner"))
        .arg("a.mr")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&[0xff, 0xfe])
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(70));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("jit-runner: internal error: cannot read the program"),
        "{stderr}"
    );
}
