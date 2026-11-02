//! `cargo xtask spec-examples`: runs the executable examples of the spec and
//! of the error pages (§20.4).
//!
//! An example is a `mora` code block whose first line is `// expect:`,
//! `// error: MRnnnn` or `// warning: MRnnnn`. Its id is the number of the
//! section it is in, or the code of its page, and its position among the
//! examples there: `2.1#1`, `MR0300#2`. Examples that cannot pass until a
//! roadmap deliverable lands are listed in `tests/spec-examples.pending`; the
//! check fails when one of them passes, so the list only shrinks.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use mora_diag::{Code, Severity};
use mora_testkit::Compiler;
use mora_testkit::harness::guarded;
use mora_testkit::run;

use crate::error_codes::{PAGES, SPEC};

/// The list of examples that may fail, relative to the repository root.
pub const PENDING: &str = "tests/spec-examples.pending";

const ROADMAP: &str = "ROADMAP.md";

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
    let mut lines = text.lines().enumerate();
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
        let Some((index, first)) = lines.next() else {
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
            at: format!("{path}:{}", index + 1),
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

/// What an example's first line says it does, if it is an example.
pub fn expects(first: &str) -> Option<Expects> {
    let first = first.trim();
    if first.starts_with("// expect:") {
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

/// The pending list: each example's id and the deliverable that makes it
/// pass, as the file lists them.
#[derive(Debug, Default)]
pub struct Pending {
    /// Id, deliverable and line, in file order.
    pub entries: Vec<(String, String, usize)>,
    /// Lines that are not `id deliverable`, a comment or blank.
    pub malformed: Vec<usize>,
}

impl Pending {
    pub fn parse(text: &str) -> Self {
        let mut pending = Self::default();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_whitespace().collect::<Vec<_>>()[..] {
                [id, deliverable] => {
                    pending
                        .entries
                        .push((id.to_owned(), deliverable.to_owned(), index + 1));
                }
                _ => pending.malformed.push(index + 1),
            }
        }
        pending
    }
}

/// Everything the check reads.
#[derive(Debug, Default)]
pub struct Repo {
    /// The examples of the spec, then of each page.
    pub examples: Vec<Example>,
    /// The pending list.
    pub pending: Pending,
    /// The deliverable ids of `ROADMAP.md`, such as `M1.15`.
    pub deliverables: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// A line of the pending list that is not `id deliverable`.
    Malformed { line: usize },
    /// An id the pending list names twice.
    Duplicate { id: String, line: usize },
    /// A pending id that names no example.
    NoSuchExample { id: String, line: usize },
    /// A pending entry whose deliverable is not in the roadmap.
    NoSuchDeliverable {
        id: String,
        deliverable: String,
        line: usize,
    },
    /// A pending example that passes: its line should go.
    PassesNow { id: String, line: usize },
    /// An example that is not pending and fails.
    Fails {
        id: String,
        at: String,
        problems: Vec<String>,
    },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { line } => {
                write!(
                    f,
                    "{PENDING}:{line}: write `id deliverable`, such as `2.1#1 M1.15`"
                )
            }
            Self::Duplicate { id, line } => write!(f, "{PENDING}:{line}: {id} is listed twice"),
            Self::NoSuchExample { id, line } => {
                write!(f, "{PENDING}:{line}: no example has the id {id}")
            }
            Self::NoSuchDeliverable {
                id,
                deliverable,
                line,
            } => write!(
                f,
                "{PENDING}:{line}: {id} waits for {deliverable}, which is not in {ROADMAP}"
            ),
            Self::PassesNow { id, line } => write!(
                f,
                "{PENDING}:{line}: {id} passes now; remove it from the list"
            ),
            Self::Fails { id, at, problems } => {
                write!(f, "{at}: example {id} fails:\n{}", problems.join("\n"))
            }
        }
    }
}

/// How the examples fared.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Examples that passed.
    pub passed: usize,
    /// Pending examples that failed, as expected.
    pub pending: usize,
    /// Everything that fails the check.
    pub problems: Vec<Problem>,
}

/// Runs every example with `compiler` and checks the results and the pending
/// list.
pub fn check(repo: &Repo, compiler: &dyn Compiler) -> Outcome {
    let mut outcome = Outcome::default();
    let mut listed: HashMap<&str, usize> = HashMap::new();
    for problem in &repo.pending.malformed {
        outcome.problems.push(Problem::Malformed { line: *problem });
    }
    for (id, deliverable, line) in &repo.pending.entries {
        let line = *line;
        if listed.insert(id.as_str(), line).is_some() {
            outcome.problems.push(Problem::Duplicate {
                id: id.clone(),
                line,
            });
        }
        if !repo.examples.iter().any(|example| example.id == *id) {
            outcome.problems.push(Problem::NoSuchExample {
                id: id.clone(),
                line,
            });
        }
        if !repo.deliverables.contains(deliverable) {
            outcome.problems.push(Problem::NoSuchDeliverable {
                id: id.clone(),
                deliverable: deliverable.clone(),
                line,
            });
        }
    }
    for example in &repo.examples {
        let problems = guarded(|| run_example(example, compiler));
        match (listed.get(example.id.as_str()), problems.is_empty()) {
            (None, true) => outcome.passed += 1,
            (Some(_), false) => outcome.pending += 1,
            (Some(&line), true) => outcome.problems.push(Problem::PassesNow {
                id: example.id.clone(),
                line,
            }),
            (None, false) => outcome.problems.push(Problem::Fails {
                id: example.id.clone(),
                at: example.at.clone(),
                problems,
            }),
        }
    }
    outcome
}

/// What is wrong with one example, empty when it passes.
fn run_example(example: &Example, compiler: &dyn Compiler) -> Vec<String> {
    let path = example
        .at
        .rsplit_once(':')
        .map_or(example.at.as_str(), |(path, _)| path);
    let Expects::Diagnostic(severity, code) = example.expects else {
        return run::check(compiler, path, &example.text);
    };
    let checked = compiler.check(path, &example.text);
    let mut problems = Vec::new();
    let reported = checked
        .diagnostics
        .iter()
        .any(|d| d.code == code && d.severity == severity);
    if !reported {
        problems.push(format!("{code} was not reported"));
    }
    for other in checked
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.code != code)
    {
        problems.push(format!(
            "unexpected error {}: {}",
            other.code, other.message
        ));
    }
    problems
}

/// Reads the spec, the error pages, the pending list and the roadmap.
pub fn load(root: &Path) -> io::Result<Repo> {
    let mut examples = extract(SPEC, None, &fs::read_to_string(root.join(SPEC))?);
    let mut pages: Vec<_> = fs::read_dir(root.join(PAGES))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    pages.sort();
    for page in pages {
        // The README holds the page template, not a page.
        let Some(code) = page.strip_suffix(".md").filter(|_| page != "README.md") else {
            continue;
        };
        let text = fs::read_to_string(root.join(PAGES).join(&page))?;
        examples.extend(extract(&format!("{PAGES}/{page}"), Some(code), &text));
    }
    let pending = Pending::parse(&fs::read_to_string(root.join(PENDING))?);
    let deliverables = fs::read_to_string(root.join(ROADMAP))?
        .lines()
        .filter_map(|line| line.strip_prefix("### M")?.split(' ').next())
        .map(|id| format!("M{id}"))
        .collect();
    Ok(Repo {
        examples,
        pending,
        deliverables,
    })
}

#[cfg(test)]
mod tests {
    use mora_diag::{Diagnostic, Files, Label};
    use mora_span::{Span, TextRange};
    use mora_testkit::{Backend, Checked, Output, Unbuilt};

    use super::*;

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

    /// Does what each example says: prints its `// expect:` lines and
    /// reports the code of its first line.
    struct Obedient;

    impl Compiler for Obedient {
        fn check(&self, path: &str, text: &str) -> Checked {
            let mut files = Files::new();
            let file = files.add(path, text);
            let span = Span::new(file, TextRange::default());
            let diagnostics = match expects(text.lines().next().unwrap()) {
                Some(Expects::Diagnostic(severity, code)) => {
                    vec![Diagnostic::new(
                        code,
                        severity,
                        "as told",
                        Label::unlabeled(span),
                    )]
                }
                _ => Vec::new(),
            };
            Checked {
                files,
                sources: vec![file],
                diagnostics,
            }
        }

        fn backends(&self) -> &[Backend] {
            &[Backend::Jit]
        }

        fn run(&self, _: &str, text: &str, _: Backend) -> Result<Output, Checked> {
            let expected = run::expected(text).unwrap();
            Ok(Output {
                stdout: expected.stdout,
                exit: expected.exit,
            })
        }
    }

    fn repo(pending: &str) -> Repo {
        Repo {
            examples: extract("doc.md", None, DOC),
            pending: Pending::parse(pending),
            deliverables: vec!["M1.15".to_owned(), "M2.8".to_owned()],
        }
    }

    const ALL_PENDING: &str = "# comment\n0#1 M1.15\n2.1#1 M1.15\n\n2.1#2 M2.8\n2.2#1 M2.8\n";

    #[test]
    fn examples_pass_when_the_compiler_does_what_they_say() {
        let outcome = check(&repo(""), &Obedient);
        assert_eq!((outcome.passed, outcome.pending), (4, 0));
        assert_eq!(outcome.problems, []);
    }

    #[test]
    fn pending_examples_may_fail() {
        let outcome = check(&repo(ALL_PENDING), &Unbuilt);
        assert_eq!((outcome.passed, outcome.pending), (0, 4));
        assert_eq!(outcome.problems, []);
    }

    #[test]
    fn a_failing_example_that_is_not_pending_fails() {
        let outcome = check(&repo("0#1 M1.15\n2.1#1 M1.15\n2.2#1 M2.8\n"), &Unbuilt);
        let [Problem::Fails { id, at, problems }] = &outcome.problems[..] else {
            panic!("{:?}", outcome.problems);
        };
        assert_eq!((id.as_str(), at.as_str()), ("2.1#2", "doc.md:29"));
        assert_eq!(
            problems,
            &["panicked: not implemented: the driver cannot compile yet"]
        );
    }

    #[test]
    fn a_pending_example_that_passes_fails_the_check() {
        let outcome = check(&repo(ALL_PENDING), &Obedient);
        let lines: Vec<_> = outcome.problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines[0],
            "tests/spec-examples.pending:2: 0#1 passes now; remove it from the list"
        );
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn the_pending_list_must_name_examples_and_deliverables() {
        let pending = "2.1#1 M1.15\n2.1#1 M9.98\n9.9#1 M2.8\n2.2#1 M9.99\nnonsense\n";
        let outcome = check(&repo(pending), &Obedient);
        let lines: Vec<_> = outcome.problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines[..5],
            [
                "tests/spec-examples.pending:5: write `id deliverable`, such as `2.1#1 M1.15`",
                "tests/spec-examples.pending:2: 2.1#1 is listed twice",
                "tests/spec-examples.pending:2: 2.1#1 waits for M9.98, which is not in ROADMAP.md",
                "tests/spec-examples.pending:3: no example has the id 9.9#1",
                "tests/spec-examples.pending:4: 2.2#1 waits for M9.99, which is not in ROADMAP.md",
            ]
        );
    }

    #[test]
    fn an_error_example_fails_on_another_error() {
        struct Wrong;
        impl Compiler for Wrong {
            fn check(&self, path: &str, text: &str) -> Checked {
                let mut checked = Obedient.check(path, text);
                let span = checked.diagnostics[0].primary.span;
                checked.diagnostics.push(Diagnostic::error(
                    "MR0301".parse().unwrap(),
                    "also this",
                    Label::unlabeled(span),
                ));
                checked
            }
            fn backends(&self) -> &[Backend] {
                &[]
            }
            fn run(&self, _: &str, _: &str, _: Backend) -> Result<Output, Checked> {
                unreachable!()
            }
        }
        let example = &repo("").examples[2];
        assert_eq!(
            run_example(example, &Wrong),
            ["unexpected error MR0301: also this"]
        );
    }

    #[test]
    fn an_error_example_fails_on_its_code_as_a_warning() {
        struct Mild;
        impl Compiler for Mild {
            fn check(&self, path: &str, text: &str) -> Checked {
                let mut checked = Obedient.check(path, text);
                checked.diagnostics[0].severity = Severity::Warning;
                checked
            }
            fn backends(&self) -> &[Backend] {
                &[]
            }
            fn run(&self, _: &str, _: &str, _: Backend) -> Result<Output, Checked> {
                unreachable!()
            }
        }
        let example = &repo("").examples[2];
        assert_eq!(run_example(example, &Mild), ["MR0300 was not reported"]);
    }

    #[test]
    fn other_languages_end_only_at_their_fence() {
        let text = "```sh\necho\n# 9.9 a shell comment\n```\n\n```mora\n// expect: x\n```\n";
        let examples = extract("doc.md", None, text);
        let ids: Vec<_> = examples.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["0#1"]);
    }

    #[test]
    fn repository_passes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let repo = load(&root).unwrap();
        assert!(repo.examples.len() > 1);
        assert!(repo.deliverables.contains(&"M1.4".to_owned()));
        let outcome = check(&repo, mora_testkit::compiler());
        assert_eq!(outcome.problems, []);
    }
}
