//! The expectations written into Mora source and Markdown, read the same way
//! by the test harnesses and the repository checks: the `//~` annotations of
//! UI tests, the run expectations of run tests (§20.1), and the executable
//! examples of the spec and of the error pages (§20.4).
//!
//! An example is a `mora` code block whose first line is a run expectation,
//! `// error: MRnnnn` or `// warning: MRnnnn`. Its id is the number of the
//! section it is in, or the code of its page, and its position among the
//! examples there: `2.1#1`, `MR0300#2`.

use std::collections::HashMap;

use crate::{Code, Severity};

/// The marker of an expected line of standard output.
pub const EXPECT: &str = "// expect";

/// The marker of an expected line of standard error.
pub const STDERR: &str = "// stderr";

/// The marker of the expected exit code.
pub const EXIT: &str = "// exit";

/// The annotations of one line, in order. Text after the code is a comment.
/// A `//~` that is not followed by `ERROR` or `WARNING` and a code gives an
/// error naming what it found.
pub fn line_annotations(line: &str) -> Vec<Result<(Severity, Code), String>> {
    line.match_indices("//~")
        .map(|(at, _)| {
            let mut words = line[at + 3..].split_whitespace();
            let severity = match words.next() {
                Some("ERROR") => Severity::Error,
                Some("WARNING") => Severity::Warning,
                other => {
                    return Err(format!(
                        "`//~` must be followed by ERROR or WARNING, not `{}`",
                        other.unwrap_or("")
                    ));
                }
            };
            let Some(code) = words.next() else {
                return Err("`//~` needs a code after the severity".to_owned());
            };
            code.parse::<Code>()
                .map(|code| (severity, code))
                .map_err(|error| error.to_string())
        })
        .collect()
}

/// The name in `marker[name]:` at the start of `line`, `None` for a plain
/// `marker:`, and the text after the colon; `None` for a line that starts
/// with neither.
pub fn split_marker<'a>(line: &'a str, marker: &str) -> Option<(Option<&'a str>, &'a str)> {
    let rest = line.strip_prefix(marker)?;
    if let Some(rest) = rest.strip_prefix(':') {
        return Some((None, rest));
    }
    let (name, rest) = rest.strip_prefix('[')?.split_once("]:")?;
    Some((Some(name), rest))
}

/// Whether `line` starts with the marker of a run expectation, plain or
/// marked for a profile, as `// expect:` or `// exit[debug]:`. A name that
/// is not a profile counts too, so that the run harness reports it.
pub fn is_expectation(line: &str) -> bool {
    let line = line.trim_start();
    [EXPECT, STDERR, EXIT]
        .iter()
        .any(|marker| split_marker(line, marker).is_some())
}

/// An executable example.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    /// `2.1#1` in the spec, `MR0300#1` in an error page.
    pub id: String,
    /// `path:line` of the block's first line, relative to the repository root.
    pub at: String,
    /// What the first line says the example does.
    pub expects: Expects,
    /// The block, without its fences.
    pub text: String,
}

/// What an example's first line says it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expects {
    /// `// expect:`: the program runs and prints what its `// expect:` lines say.
    Output,
    /// `// error:` or `// warning:`: the compiler reports this code with this
    /// severity, and no error with another code.
    Diagnostic(Severity, Code),
}

/// The examples of one Markdown file. An error page passes its code as
/// `page`, which names all of its examples. Otherwise numbered headings name
/// them, and `0` names those before the first one.
pub fn extract(path: &str, page: Option<&str>, text: &str) -> Vec<Example> {
    let mut examples = Vec::new();
    let mut section = page.unwrap_or("0").to_owned();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut lines = (1usize..).zip(text.lines());
    while let Some((_, line)) = lines.next() {
        if let Some(number) = section_number(line).filter(|_| page.is_none()) {
            section = number.to_owned();
            continue;
        }
        let fence = line.trim();
        if !fence.starts_with("```") {
            continue;
        }
        if fence != "```mora" {
            // Another language: its lines are not headings.
            for (_, line) in lines.by_ref() {
                if line.trim() == "```" {
                    break;
                }
            }
            continue;
        }
        let Some((number, first)) = lines.next() else {
            break;
        };
        let mut body = vec![first];
        for (_, line) in lines.by_ref() {
            if line.trim() == "```" {
                break;
            }
            body.push(line);
        }
        let Some(expects) = expects(first) else {
            continue;
        };
        let count = counts.entry(section.clone()).or_default();
        *count += 1;
        examples.push(Example {
            id: format!("{section}#{count}"),
            at: format!("{path}:{number}"),
            expects,
            text: body.iter().map(|line| format!("{line}\n")).collect(),
        });
    }
    examples
}

/// The number of a numbered heading such as `### 10.4 Borrowing` or
/// `## 2. A tour`.
fn section_number(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    if rest.len() == line.len() {
        return None;
    }
    let number = rest.strip_prefix(' ')?.split(' ').next()?;
    let number = number.strip_suffix('.').unwrap_or(number);
    let numbered = number
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    numbered.then_some(number)
}

/// What an example's first line says it does, if it is an example: a run
/// expectation of §20.1, plain or marked for a profile, makes it a program
/// to run.
pub fn expects(first: &str) -> Option<Expects> {
    let first = first.trim();
    if is_expectation(first) {
        return Some(Expects::Output);
    }
    let markers = [
        ("// error:", Severity::Error),
        ("// warning:", Severity::Warning),
    ];
    markers.iter().find_map(|(marker, severity)| {
        let code = first.strip_prefix(marker)?.split_whitespace().next()?;
        Some(Expects::Diagnostic(*severity, code.parse().ok()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(text: &str) -> Code {
        text.parse().unwrap()
    }

    #[test]
    fn line_annotations_read_each_marker_on_the_line() {
        assert_eq!(
            line_annotations("f(b); //~ ERROR MR0320 moved //~ WARNING MR2002"),
            [
                Ok((Severity::Error, code("MR0320"))),
                Ok((Severity::Warning, code("MR2002"))),
            ]
        );
        assert_eq!(line_annotations("let a = 1;"), []);
        assert_eq!(
            line_annotations("a //~ NOTE MR0300 //~ ERROR MR300 //~"),
            [
                Err("`//~` must be followed by ERROR or WARNING, not `NOTE`".to_owned()),
                Err("`MR300` is not a diagnostic code like `MR0310`".to_owned()),
                Err("`//~` must be followed by ERROR or WARNING, not ``".to_owned()),
            ]
        );
        assert_eq!(
            line_annotations("a //~ WARNING"),
            [Err("`//~` needs a code after the severity".to_owned())]
        );
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
    fn split_marker_gives_the_profile_name_and_the_text_after_the_colon() {
        assert_eq!(split_marker("// exit: 3", EXIT), Some((None, " 3")));
        assert_eq!(
            split_marker("// expect[debug]: a", EXPECT),
            Some((Some("debug"), " a"))
        );
        assert_eq!(split_marker("// expect: a", STDERR), None);
        assert_eq!(split_marker("// expect[debug a", EXPECT), None);
    }

    const DOC: &str = "\
# Title

```mora
// expect: before any section
```

## 2. Tour

### 2.1 Hello

```mora
// expect: Hello
Console.WriteLine(\"Hello\");
```

```mora
// not an example
```

```rust
// expect: not Mora
```

```sh
# 9.9 a shell comment, not a heading
```

```mora
// error: MR0300 used after a move
var b = a;
Use(a);
```

### 2.2 More

```mora
// warning: MR2002
F();
```
";

    #[test]
    fn examples_are_numbered_within_their_section() {
        let examples = extract("doc.md", None, DOC);
        let ids: Vec<_> = examples
            .iter()
            .map(|e| (e.id.as_str(), e.at.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                ("0#1", "doc.md:4"),
                ("2.1#1", "doc.md:12"),
                ("2.1#2", "doc.md:29"),
                ("2.2#1", "doc.md:37"),
            ]
        );
        assert_eq!(
            examples[1].text,
            "// expect: Hello\nConsole.WriteLine(\"Hello\");\n"
        );
        assert_eq!(examples[1].expects, Expects::Output);
        assert_eq!(
            examples[2].expects,
            Expects::Diagnostic(Severity::Error, "MR0300".parse().unwrap())
        );
        assert_eq!(
            examples[3].expects,
            Expects::Diagnostic(Severity::Warning, "MR2002".parse().unwrap())
        );
    }

    #[test]
    fn any_run_expectation_starts_a_program_example() {
        for first in [
            "// expect: a",
            "// stderr: a",
            "// exit: 3",
            "// exit[debug]: 101",
            "// expect[release]: a",
        ] {
            assert_eq!(expects(first), Some(Expects::Output), "{first}");
        }
        assert_eq!(expects("// expected: a"), None);
        assert_eq!(expects("let x = 1;"), None);
    }

    #[test]
    fn pages_name_examples_after_their_code() {
        let page = "# MR0300: x\n\n## 1. Failing example\n\n```mora\n// error: MR0300\n```\n\n## 2. Fixed example\n\n```mora\n// expect: ok\n```\n";
        let examples = extract("docs/errors/MR0300.md", Some("MR0300"), page);
        let ids: Vec<_> = examples.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["MR0300#1", "MR0300#2"]);
    }

    #[test]
    fn headings_need_a_number_to_start_a_section() {
        assert_eq!(section_number("### 10.4 Borrowing"), Some("10.4"));
        assert_eq!(section_number("## 2. Tour"), Some("2"));
        assert_eq!(section_number("## Appendix"), None);
        assert_eq!(section_number("10.4 not a heading"), None);
    }

    #[test]
    fn other_languages_end_only_at_their_fence() {
        let text = "```sh\necho\n# 9.9 a shell comment\n```\n\n```mora\n// expect: x\n```\n";
        let examples = extract("doc.md", None, text);
        let ids: Vec<_> = examples.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["0#1"]);
    }
}
