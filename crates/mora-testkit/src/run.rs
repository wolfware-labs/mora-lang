//! Run tests: a `.mr` program with `// expect:` lines giving its standard
//! output, one line each, and an optional `// exit:` line giving its exit
//! code (0 by default).

use crate::compiler::Compiler;
use crate::diff::diff;
use crate::ui::rendered;

const EXPECT: &str = "// expect:";
const EXIT: &str = "// exit:";

/// What a run test expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub stdout: String,
    pub exit: i32,
}

/// Reads the `// expect:` and `// exit:` lines. The text after the marker,
/// less one space, is a line of output.
pub fn expected(text: &str) -> Result<Expected, String> {
    let mut stdout = String::new();
    let mut exit = None;
    for (number, line) in (1usize..).zip(text.lines()) {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix(EXPECT) {
            stdout.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            stdout.push('\n');
        } else if let Some(rest) = line.strip_prefix(EXIT) {
            if exit.is_some() {
                return Err(format!("line {number}: a second `{EXIT}` line"));
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
        exit: exit.unwrap_or(0),
    })
}

/// Checks one case: builds and runs `text` as `path` with every backend the
/// compiler has, and compares each run's output and exit code with the
/// expected ones. Returns the problems, empty when the case passes.
pub fn check(compiler: &dyn Compiler, path: &str, text: &str) -> Vec<String> {
    let expected = match expected(text) {
        Ok(expected) => expected,
        Err(problem) => return vec![problem],
    };
    let backends = compiler.backends();
    if backends.is_empty() {
        return vec!["the compiler has no backend to run programs with".to_owned()];
    }
    let mut problems = Vec::new();
    for &backend in backends {
        let name = backend.name();
        let output = match compiler.run(path, text, backend) {
            Ok(output) => output,
            Err(checked) => {
                problems.push(format!("{name}: the build failed:\n{}", rendered(&checked)));
                continue;
            }
        };
        // A program that prints with `\r\n` on Windows passes the same test.
        let stdout = output.stdout.replace("\r\n", "\n");
        if stdout != expected.stdout {
            problems.push(format!(
                "{name}: stdout differs (- expected, + actual):\n{}",
                diff(&expected.stdout, &stdout)
            ));
        }
        if output.exit != expected.exit {
            problems.push(format!(
                "{name}: exit code {}, expected {}",
                output.exit, expected.exit
            ));
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
    /// not compile.
    pub hir: Option<String>,
    /// The MIR of the program, for its `.mir` snapshot; `None` when it does
    /// not compile.
    pub mir: Option<String>,
}

/// Checks one case of the run suite: the program must compile without
/// errors, and its HIR and MIR are returned for the snapshots beside it. Then it runs
/// through every backend the compiler has, as [`check`] does; while there
/// are none, compiling is the whole test.
pub fn check_case(compiler: &dyn Compiler, path: &str, text: &str) -> Outcome {
    if let Err(problem) = expected(text) {
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
                hir: Some(hir),
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
            expected(text),
            Ok(Expected {
                stdout: "Hello\n\n two spaces\n".to_owned(),
                exit: 3,
            })
        );
    }

    #[test]
    fn exit_defaults_to_zero_and_must_be_a_number() {
        assert_eq!(expected("f();").map(|e| e.exit), Ok(0));
        assert_eq!(
            expected("// exit: one"),
            Err("line 1: `one` is not an exit code".to_owned())
        );
        assert_eq!(
            expected("// exit: 1\n// exit: 2"),
            Err("line 2: a second `// exit:` line".to_owned())
        );
    }
}
