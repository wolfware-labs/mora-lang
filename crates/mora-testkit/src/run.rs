//! Run tests: a `.mr` program with `// expect:` lines giving its standard
//! output, one line each, `// stderr:` lines giving its standard error the
//! same way (empty by default), and an optional `// exit:` line giving its
//! exit code (0 by default). The program runs in both profiles, and a line
//! written `// expect[debug]:`, `// stderr[release]:` and so on holds only
//! for that profile.

use crate::Profile;
use crate::compiler::{Compiler, PROFILES, profile_name, profile_named};
use crate::diff::diff;
use crate::ui::rendered;

const EXPECT: &str = "// expect";
const STDERR: &str = "// stderr";
const EXIT: &str = "// exit";

/// What a run test expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
}

/// Reads the `// expect:`, `// stderr:` and `// exit:` lines that hold for
/// `profile`: those without a profile and those marked with its name, as
/// `// exit[release]: 0`. The text after the marker, less one space, is a
/// line of output.
///
/// # Errors
///
/// A line marked with a name that is not a profile, an exit code that is
/// not a number, and a second exit code for one profile: two plain
/// `// exit:` lines, or a plain one and one marked for the profile. The
/// `// expect:` and `// stderr:` lines, plain or marked, add up in order.
pub fn expected(text: &str, profile: Profile) -> Result<Expected, String> {
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit = None;
    for (number, line) in (1usize..).zip(text.lines()) {
        let line = line.trim_start();
        if let Some(rest) = marked(line, EXPECT, profile, number)? {
            push_line(&mut stdout, rest);
        } else if let Some(rest) = marked(line, STDERR, profile, number)? {
            push_line(&mut stderr, rest);
        } else if let Some(rest) = marked(line, EXIT, profile, number)? {
            if exit.is_some() {
                let written = &line[..line.len() - rest.len()];
                let profile = profile_name(profile);
                return Err(format!(
                    "line {number}: `{written}` gives the {profile} profile a second exit code"
                ));
            }
            let code = rest.trim();
            exit = Some(
                code.parse()
                    .map_err(|_| format!("line {number}: `{code}` is not an exit code"))?,
            );
        }
    }
    Ok(Expected {
        stdout,
        stderr,
        exit: exit.unwrap_or(0),
    })
}

/// The text after `marker:` on `line`, or after `marker[name]:` when `name`
/// is the name of `profile`; `None` for another line, and for a line marked
/// for the other profile.
///
/// # Errors
///
/// When `name` is not the name of a profile.
fn marked<'a>(
    line: &'a str,
    marker: &str,
    profile: Profile,
    number: usize,
) -> Result<Option<&'a str>, String> {
    let Some((name, rest)) = split_marker(line, marker) else {
        return Ok(None);
    };
    let Some(name) = name else {
        return Ok(Some(rest));
    };
    match profile_named(name) {
        Some(named) => Ok((named == profile).then_some(rest)),
        None => Err(format!("line {number}: `{name}` is not a profile")),
    }
}

/// The name in `marker[name]:` at the start of `line`, `None` for a plain
/// `marker:`, and the text after the colon; `None` for a line that starts
/// with neither.
fn split_marker<'a>(line: &'a str, marker: &str) -> Option<(Option<&'a str>, &'a str)> {
    let rest = line.strip_prefix(marker)?;
    if let Some(rest) = rest.strip_prefix(':') {
        return Some((None, rest));
    }
    let (name, rest) = rest.strip_prefix('[')?.split_once("]:")?;
    Some((Some(name), rest))
}

/// Whether `line` starts with the marker of a run expectation, plain or
/// marked for a profile, as `// expect:` or `// exit[debug]:`. A name that
/// is not a profile counts too, so that [`expected`] reports it.
pub fn is_expectation(line: &str) -> bool {
    let line = line.trim_start();
    [EXPECT, STDERR, EXIT]
        .iter()
        .any(|marker| split_marker(line, marker).is_some())
}

/// Adds the line of output `rest`, the text after a marker, to `out`: less
/// the one space after the marker, and with its line ending.
fn push_line(out: &mut String, rest: &str) {
    out.push_str(rest.strip_prefix(' ').unwrap_or(rest));
    out.push('\n');
}

/// What `text` expects in each profile, in the order of [`PROFILES`].
///
/// # Errors
///
/// As [`expected`].
fn expectations(text: &str) -> Result<Vec<(Profile, Expected)>, String> {
    PROFILES
        .iter()
        .map(|&profile| Ok((profile, expected(text, profile)?)))
        .collect()
}

/// Checks one case: builds and runs `text` as `path` with every backend the
/// compiler has, in each profile, and compares each run's output, error
/// output and exit code with the ones expected in that profile. Returns the
/// problems, empty when the case passes.
pub fn check(compiler: &dyn Compiler, path: &str, text: &str) -> Vec<String> {
    let expectations = match expectations(text) {
        Ok(expectations) => expectations,
        Err(problem) => return vec![problem],
    };
    let backends = compiler.backends();
    if backends.is_empty() {
        return vec!["the compiler has no backend to run programs with".to_owned()];
    }
    let mut problems = Vec::new();
    for &backend in backends {
        for (profile, expected) in &expectations {
            let name = format!("{} {}", backend.name(), profile_name(*profile));
            let output = match compiler.run(path, text, backend, *profile) {
                Ok(output) => output,
                Err(checked) => {
                    problems.push(format!("{name}: the build failed:\n{}", rendered(&checked)));
                    continue;
                }
            };
            // A program that prints with `\r\n` on Windows passes the same
            // test.
            let stdout = output.stdout.replace("\r\n", "\n");
            if stdout != expected.stdout {
                problems.push(format!(
                    "{name}: stdout differs (- expected, + actual):\n{}",
                    diff(&expected.stdout, &stdout)
                ));
            }
            let stderr = output.stderr.replace("\r\n", "\n");
            if stderr != expected.stderr {
                problems.push(format!(
                    "{name}: stderr differs (- expected, + actual):\n{}",
                    diff(&expected.stderr, &stderr)
                ));
            }
            if output.exit != expected.exit {
                problems.push(format!(
                    "{name}: exit code {}, expected {}",
                    output.exit, expected.exit
                ));
            }
        }
    }
    problems
}

/// What one case of the run suite found.
#[derive(Debug, Default)]
pub struct Outcome {
    /// What is wrong, empty when the case passes.
    pub problems: Vec<String>,
    /// The HIR of the program, for its `.hir` snapshot; `None` when it does
    /// not compile or does not lower.
    pub hir: Option<String>,
    /// The MIR of the program, for its `.mir` snapshot; `None` when it does
    /// not compile or does not lower.
    pub mir: Option<String>,
}

/// Checks one case of the run suite: the program must compile without
/// errors, and its HIR and MIR are returned for the snapshots beside it.
/// Then it runs through every backend the compiler has, as [`check`] does;
/// while there are none, compiling is the whole test.
pub fn check_case(compiler: &dyn Compiler, path: &str, text: &str) -> Outcome {
    if let Err(problem) = expectations(text) {
        return Outcome {
            problems: vec![problem],
            hir: None,
            mir: None,
        };
    }
    let hir = match compiler.hir(path, text) {
        Ok(hir) => hir,
        Err(checked) => {
            return Outcome {
                problems: vec![format!(
                    "the program does not compile:\n{}",
                    rendered(&checked)
                )],
                hir: None,
                mir: None,
            };
        }
    };
    // The same program compiled for the HIR compiles for the MIR, so a
    // failure here is a bug in the compiler, not in the case.
    let mir = match compiler.mir(path, text) {
        Ok(mir) => mir,
        Err(checked) => {
            return Outcome {
                problems: vec![format!(
                    "the program does not lower:\n{}",
                    rendered(&checked)
                )],
                hir: None,
                mir: None,
            };
        }
    };
    let problems = if compiler.backends().is_empty() {
        Vec::new()
    } else {
        check(compiler, path, text)
    };
    Outcome {
        problems,
        hir: Some(hir),
        mir: Some(mir),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expect_lines_build_stdout_in_order() {
        let text = "// expect: Hello\n// expect:\n//expect: not a marker\nf(); // exit: is not first\n    // expect:  two spaces\n// exit: 3\n";
        assert_eq!(
            expected(text, Profile::Debug),
            Ok(Expected {
                stdout: "Hello\n\n two spaces\n".to_owned(),
                stderr: String::new(),
                exit: 3,
            })
        );
    }

    #[test]
    fn stderr_lines_build_expected_stderr() {
        assert_eq!(
            expected("// stderr: panic at a\n// stderr: b\n", Profile::Release)
                .unwrap()
                .stderr,
            "panic at a\nb\n"
        );
    }

    #[test]
    fn exit_defaults_to_zero_and_must_be_a_number() {
        assert_eq!(expected("f();", Profile::Debug).map(|e| e.exit), Ok(0));
        assert_eq!(
            expected("// exit: one", Profile::Debug),
            Err("line 1: `one` is not an exit code".to_owned())
        );
        assert_eq!(
            expected("// exit: 1\n// exit: 2", Profile::Debug),
            Err("line 2: `// exit:` gives the debug profile a second exit code".to_owned())
        );
    }

    #[test]
    fn a_marked_line_holds_only_for_its_profile() {
        let text = "// expect: both\n// expect[debug]: debug\n// expect[release]: release\n\
                    // stderr[debug]: panic\n// exit[debug]: 101\n// expect: last\n";
        assert_eq!(
            expected(text, Profile::Debug),
            Ok(Expected {
                stdout: "both\ndebug\nlast\n".to_owned(),
                stderr: "panic\n".to_owned(),
                exit: 101,
            })
        );
        assert_eq!(
            expected(text, Profile::Release),
            Ok(Expected {
                stdout: "both\nrelease\nlast\n".to_owned(),
                stderr: String::new(),
                exit: 0,
            })
        );
        // Not markers: no colon after the name.
        let text = "// expect[debug] x\n// expected: y\n// expect[debug x\n";
        assert_eq!(expected(text, Profile::Debug).unwrap().stdout, "");
    }

    #[test]
    fn expectation_markers_are_plain_or_marked() {
        for line in [
            "// expect: a",
            "// stderr: a",
            "// exit: 1",
            "  // exit[debug]: 101",
            "// expect[release]: a",
            "// stderr[fast]: a",
        ] {
            assert!(is_expectation(line), "{line}");
        }
        for line in [
            "// expected: a",
            "// expect[debug] a",
            "// error: MR0300",
            "let x = 1;",
        ] {
            assert!(!is_expectation(line), "{line}");
        }
    }

    #[test]
    fn a_marked_line_must_name_a_profile_and_one_exit_each() {
        assert_eq!(
            expected("// expect[fast]: x\n", Profile::Release),
            Err("line 1: `fast` is not a profile".to_owned())
        );
        let text = "// exit: 1\n// exit[release]: 2\n";
        assert_eq!(expected(text, Profile::Debug).map(|e| e.exit), Ok(1));
        assert_eq!(
            expected(text, Profile::Release),
            Err(
                "line 2: `// exit[release]:` gives the release profile a second exit code"
                    .to_owned()
            )
        );
        assert_eq!(
            expectations("// stderr[fast]: x\n"),
            Err("line 1: `fast` is not a profile".to_owned())
        );
    }
}
