#![allow(clippy::unwrap_used)]
// Helpers below unwrap too, and `allow-unwrap-in-tests` covers only test functions.

//! Runs the `mora` executable as a user would (§20.1, CLI layer).

use std::process::{Command, Output, Stdio};

fn mora(args: &[&str]) -> Output {
    mora_into(args, Stdio::piped())
}

/// Runs `mora` with its stdout sent to `stdout`.
fn mora_into(args: &[&str], stdout: impl Into<Stdio>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mora"))
        .args(args)
        .env_remove("NO_COLOR")
        .stdout(stdout)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The MVP commands of §17.1.
const COMMANDS: &[&str] = &[
    "new", "build", "run", "check", "test", "fmt", "repl", "explain", "lsp", "demangle", "version",
];

#[test]
fn help_lists_every_command_and_global_option() {
    let output = mora(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = stdout(&output);
    for command in COMMANDS {
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with(command)),
            "{command} missing from:\n{help}"
        );
    }
    assert!(help.contains("--color <COLOR>"), "{help}");
    assert!(help.contains("--message-format <MESSAGE_FORMAT>"), "{help}");
}

#[test]
fn version_prints_the_version_commit_and_runtime_targets() {
    let output = mora(&["version"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], format!("mora {}", env!("CARGO_PKG_VERSION")));
    let commit = lines[1].strip_prefix("commit: ").unwrap();
    assert!(!commit.is_empty(), "{text}");
    // Under target/, beside no toolchain lib/, nothing is installed.
    assert_eq!(lines[2], "runtime targets: none");
}

#[test]
fn version_in_json_is_one_object() {
    let output = mora(&["version", "--message-format", "json"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    let line = text.trim_end();
    assert!(!line.contains('\n'), "{text}");
    assert!(
        line.starts_with(&format!(
            "{{\"schema\":\"mora.version/1\",\"version\":\"{}\",\"commit\":\"",
            env!("CARGO_PKG_VERSION")
        )),
        "{text}"
    );
    assert!(line.ends_with(",\"runtime_targets\":[]}"), "{text}");
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let output = mora(&["frobnicate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("unrecognized subcommand 'frobnicate'"));
}

#[test]
fn post_mvp_commands_are_unknown() {
    for command in ["fetch", "publish", "caps"] {
        assert_eq!(mora(&[command]).status.code(), Some(2), "{command}");
    }
}

#[test]
fn a_bad_option_value_is_a_usage_error() {
    let calls: &[&[&str]] = &[
        &["--color", "purple", "version"],
        &["new", "app", "--template", "purple"],
        &["build", "--emit", "purple"],
    ];
    for args in calls {
        let output = mora(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(
            stderr(&output).contains("invalid value 'purple'"),
            "{args:?}"
        );
    }
}

#[test]
fn commands_not_built_yet_say_so_and_exit_2() {
    let calls: &[&[&str]] = &[
        &["new", "app", "--template", "web"],
        &["build", "--release", "--emit", "mir"],
        &["run", "--", "arg"],
        &["check"],
        &["test", "parses"],
        &["fmt", "--check"],
        &["repl"],
        &["explain", "MR0310"],
        &["lsp"],
        &["demangle", "_M3Foo"],
    ];
    // The line is plain text in either message format (§17.1). The option
    // goes first, because everything after `run --` belongs to the program.
    for format in ["human", "json"] {
        for args in calls {
            let mut full = vec!["--message-format", format];
            full.extend_from_slice(args);
            let output = mora(&full);
            assert_eq!(output.status.code(), Some(2), "{full:?}");
            assert_eq!(
                stderr(&output),
                format!("error: `mora {}` is not implemented yet\n", args[0]),
                "{full:?}"
            );
        }
    }
}

#[test]
fn color_follows_the_option() {
    let colored = stderr(&mora(&["--color", "always", "check"]));
    assert!(
        colored.starts_with("\u{1b}[1;31merror\u{1b}[0m:"),
        "{colored:?}"
    );
    let plain = stderr(&mora(&["--color", "never", "check"]));
    assert!(!plain.contains('\u{1b}'), "{plain:?}");
    // Not a terminal: auto means no color.
    let auto = stderr(&mora(&["check"]));
    assert!(!auto.contains('\u{1b}'), "{auto:?}");
}

#[test]
fn version_lists_the_runtimes_installed_beside_the_executable() {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolchain");
    let _ = std::fs::remove_dir_all(&root);
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_mora"));
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let installed = bin.join(exe.file_name().unwrap());
    // A hard link, not a copy: a copy is a file this test writes and then
    // runs, which fails with "Text file busy" when another test thread
    // starts a process while the copy is still open.
    std::fs::hard_link(exe, &installed)
        .or_else(|_| std::fs::copy(exe, &installed).map(drop))
        .unwrap();
    for target in ["alpha-os", "beta-os"] {
        let dir = root.join("lib/rt").join(target);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libmora_rt.a"), "").unwrap();
    }
    // A directory without the library is not an installed runtime.
    std::fs::create_dir_all(root.join("lib/rt/gamma-os")).unwrap();

    let run = |args: &[&str]| Command::new(&installed).args(args).output().unwrap();
    let text = stdout(&run(&["version"]));
    assert_eq!(
        text.lines().nth(2),
        Some("runtime targets: alpha-os, beta-os"),
        "{text}"
    );
    let json = stdout(&run(&["version", "--message-format", "json"]));
    assert!(
        json.trim_end()
            .ends_with(",\"runtime_targets\":[\"alpha-os\",\"beta-os\"]}"),
        "{json}"
    );
}

/// Command lines that write to stdout: `mora version` in both formats, and
/// the `--version` and `--help` options clap answers itself.
const WRITERS: &[&[&str]] = &[
    &["--message-format", "human", "version"],
    &["--message-format", "json", "version"],
    &["--version"],
    &["--help"],
];

#[test]
fn output_into_a_closed_pipe_exits_0_quietly() {
    for args in WRITERS {
        let (reader, writer) = std::io::pipe().unwrap();
        // Closed before the process starts, so its first write fails.
        drop(reader);
        let output = mora_into(args, writer);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(stderr(&output), "", "{args:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn output_into_a_full_disk_is_a_failure() {
    for args in WRITERS {
        let full = std::fs::File::create("/dev/full").unwrap();
        let output = mora_into(args, full);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let text = stderr(&output);
        assert!(
            text.starts_with("error: cannot write to stdout: "),
            "{args:?}: {text}"
        );
        assert!(!text.contains("panicked"), "{args:?}: {text}");
    }
}

#[test]
fn the_version_option_prints_the_first_line_only() {
    let output = mora(&["--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        format!("mora {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_usage_error_prints_nothing_on_stdout() {
    let output = mora(&["--color"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(
        stderr(&output).starts_with("error: "),
        "{}",
        stderr(&output)
    );
}

#[test]
fn help_and_usage_errors_follow_the_color_option() {
    const ESC: char = '\u{1b}';
    for color in ["--color=always", "--color always"] {
        let mut args: Vec<&str> = color.split(' ').collect();
        args.push("--help");
        let help = stdout(&mora(&args));
        assert!(help.contains(ESC), "{args:?}: {help:?}");
        args.pop();
        args.push("frobnicate");
        let error = stderr(&mora(&args));
        assert!(error.contains(ESC), "{args:?}: {error:?}");
    }
    for args in [
        &["--color", "never", "--help"][..],
        &["--help"],
        &["--color", "always", "--color", "never", "--help"],
    ] {
        let help = stdout(&mora(args));
        assert!(!help.contains(ESC), "{args:?}: {help:?}");
    }
    for args in [
        &["--color", "never", "frobnicate"][..],
        &["frobnicate"],
        // After `--` the words are arguments, not options.
        &["frobnicate", "--", "--color", "always"],
    ] {
        let error = stderr(&mora(args));
        assert!(error.starts_with("error:"), "{args:?}: {error:?}");
        assert!(!error.contains(ESC), "{args:?}: {error:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_write_error_report_follows_the_color_option() {
    let colored = stderr(&mora_into(
        &["--color", "always", "--help"],
        std::fs::File::create("/dev/full").unwrap(),
    ));
    assert!(
        colored.starts_with("\u{1b}[1;31merror\u{1b}[0m:"),
        "{colored:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_usage_error_into_a_full_stderr_still_exits_2() {
    let output = Command::new(env!("CARGO_BIN_EXE_mora"))
        .arg("nosuch")
        .stderr(std::fs::File::create("/dev/full").unwrap())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
