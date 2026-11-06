//! `cargo xtask error-codes`: checks the code registry against the spec,
//! the error pages and every place that uses a code (§17.6, §20.4).
//!
//! A code is *used* when a crate refers to `codes::MRnnnn`, a UI test
//! expects it with `//~ ERROR MRnnnn` or `//~ WARNING MRnnnn`, or a `mora`
//! example in the spec or a page starts with `// error: MRnnnn` or
//! `// warning: MRnnnn`. Every used code needs a registry entry and a page.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use mora_diag::Code;
use mora_diag::registry::{CodeRange, Entry};
use mora_testkit::ui;

use crate::paths::{self, PAGE_TEMPLATE, PAGES, SPEC};
use crate::spec_examples::{self, Expects};

/// A code at a place in the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    /// The code used.
    pub code: Code,
    /// `path:line`, relative to the repository root.
    pub at: String,
}

/// The parts of the repository the check reads.
#[derive(Debug, Default)]
pub struct Repo {
    /// The text of `docs/design/mora.md`.
    pub spec: String,
    /// File name and text of every file in `docs/errors/`.
    pub pages: Vec<(String, String)>,
    /// Every code that crate sources, UI tests and executable examples use.
    pub uses: Vec<Use>,
    /// Lines in crate sources that name a code in a way the check cannot
    /// follow, with the reason.
    pub bypasses: Vec<(String, &'static str)>,
}

impl Repo {
    /// The pages, without the template.
    pub fn code_pages(&self) -> impl Iterator<Item = &(String, String)> {
        self.pages.iter().filter(|(file, _)| file != PAGE_TEMPLATE)
    }
}

/// The crate that owns the registry, where constructing codes is allowed.
const REGISTRY_CRATE: &str = "mora-diag";

/// Ways of naming a code that would hide a use from the check.
const BYPASSES: &[(&str, &str)] = &[
    (
        "Code::new(",
        "report registered codes as `codes::MRnnnn`, not `Code::new`",
    ),
    (
        "codes::{",
        "name each code as `codes::MRnnnn`; a braced import hides the use",
    ),
    (
        "codes::*",
        "name each code as `codes::MRnnnn`; a glob import hides the use",
    ),
];

/// What the check found wrong, one problem per place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The range table of §17.6 and the registry's ranges differ.
    RangeTable {
        spec: Vec<String>,
        registry: Vec<String>,
    },
    /// A registered code that does not come after the one before it.
    Unsorted { code: Code },
    /// A registered code outside every range.
    OutOfRange { code: Code, at: String },
    /// A code the spec or a use names, missing from the registry.
    Unregistered { code: Code, at: String },
    /// A registered code the spec never mentions.
    NotInSpec { code: Code },
    /// A used code without a page in `docs/errors/`.
    MissingPage { code: Code, at: String },
    /// A file in `docs/errors/` that is not a page for a registered code.
    StrayPage { file: String },
    /// A page whose title, sections or examples do not follow the template.
    BadPage { file: String, reason: String },
    /// A crate names a code in a way that hides the use.
    Bypass { at: String, reason: &'static str },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::RangeTable { spec, registry } => write!(
                f,
                "{SPEC}: the §17.6 range table does not match RANGES in crates/mora-diag/src/registry.rs\n  spec:     {}\n  registry: {}",
                spec.join("; "),
                registry.join("; ")
            ),
            Problem::Unsorted { code } => {
                write!(f, "registry: {code} is out of order or listed twice")
            }
            Problem::OutOfRange { code, at } => {
                write!(f, "{at}: {code} is outside every range of §17.6")
            }
            Problem::Unregistered { code, at } => {
                write!(f, "{at}: {code} is not in crates/mora-diag/src/registry.rs")
            }
            Problem::NotInSpec { code } => write!(
                f,
                "registry: {code} is registered but {SPEC} never mentions it"
            ),
            Problem::MissingPage { code, at } => {
                write!(
                    f,
                    "{at}: {code} is used but {PAGES}/{code}.md does not exist"
                )
            }
            Problem::StrayPage { file } => write!(
                f,
                "{PAGES}/{file}: not {PAGE_TEMPLATE} or a page for a registered code"
            ),
            Problem::BadPage { file, reason } => write!(f, "{PAGES}/{file}: {reason}"),
            Problem::Bypass { at, reason } => write!(f, "{at}: {reason}"),
        }
    }
}

/// Checks a repository against a registry.
pub fn check(repo: &Repo, ranges: &[CodeRange], codes: &[Entry]) -> Vec<Problem> {
    let mut problems = Vec::new();
    let registered = |code: Code| codes.iter().any(|entry| entry.code == code);
    let in_range = |code: Code| ranges.iter().any(|range| range.contains(code));

    let spec_ranges = range_rows(&repo.spec);
    let registry_ranges: Vec<String> = ranges
        .iter()
        .map(|r| range_text(r.first, r.last, r.kind))
        .collect();
    if spec_ranges != registry_ranges {
        problems.push(Problem::RangeTable {
            spec: spec_ranges,
            registry: registry_ranges,
        });
    }

    for pair in codes.windows(2) {
        if pair[0].code >= pair[1].code {
            problems.push(Problem::Unsorted { code: pair[1].code });
        }
    }
    for entry in codes {
        if !in_range(entry.code) {
            problems.push(Problem::OutOfRange {
                code: entry.code,
                at: "registry".into(),
            });
        }
    }

    let mentions = spec_mentions(&repo.spec);
    for mention in &mentions {
        if !in_range(mention.code) {
            problems.push(Problem::OutOfRange {
                code: mention.code,
                at: mention.at.clone(),
            });
        } else if !registered(mention.code) {
            problems.push(Problem::Unregistered {
                code: mention.code,
                at: mention.at.clone(),
            });
        }
    }
    for entry in codes {
        if !mentions.iter().any(|m| m.code == entry.code) {
            problems.push(Problem::NotInSpec { code: entry.code });
        }
    }

    let page_codes: Vec<Code> = repo
        .pages
        .iter()
        .filter_map(|(file, _)| page_code(file))
        .collect();
    let mut uses = repo.uses.clone();
    uses.extend(examples(SPEC, &repo.spec));
    // The template holds placeholder examples, not uses.
    for (file, text) in repo.code_pages() {
        uses.extend(examples(&format!("{PAGES}/{file}"), text));
    }
    for used in &uses {
        if !registered(used.code) {
            problems.push(Problem::Unregistered {
                code: used.code,
                at: used.at.clone(),
            });
        } else if !page_codes.contains(&used.code) {
            problems.push(Problem::MissingPage {
                code: used.code,
                at: used.at.clone(),
            });
        }
    }

    for (file, text) in repo.code_pages() {
        match page_code(file) {
            Some(code) if registered(code) => {
                if let Err(reason) = check_page(code, text) {
                    problems.push(Problem::BadPage {
                        file: file.clone(),
                        reason,
                    });
                }
            }
            _ => problems.push(Problem::StrayPage { file: file.clone() }),
        }
    }
    for (at, reason) in &repo.bypasses {
        problems.push(Problem::Bypass {
            at: at.clone(),
            reason,
        });
    }
    problems.dedup();
    problems
}

/// The code a page file name stands for: `MR0310.md` gives `MR0310`.
fn page_code(file: &str) -> Option<Code> {
    file.strip_suffix(".md")?.parse().ok()
}

/// The rows of the §17.6 range table as [`range_text`] shows them.
fn range_rows(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(range_row)
        .map(|(first, last, kind)| range_text(first, last, kind))
        .collect()
}

/// A range as the range-table problem shows it: `MR0001-MR0099 Lexical`.
fn range_text(first: Code, last: Code, kind: &str) -> String {
    format!("{first}-{last} {kind}")
}

/// The first code, last code and kind of a row of the §17.6 range table.
fn range_row(line: &str) -> Option<(Code, Code, &str)> {
    let rest = line.strip_prefix("| `")?;
    let (first, rest) = rest.split_once("`-`")?;
    let (last, rest) = rest.split_once("` | ")?;
    let kind = rest.strip_suffix(" |")?;
    Some((first.parse().ok()?, last.parse().ok()?, kind))
}

/// Every code the spec names outside the range table.
fn spec_mentions(spec: &str) -> Vec<Use> {
    let mut mentions = Vec::new();
    for (number, line) in (1usize..).zip(spec.lines()) {
        if range_row(line).is_some() {
            continue;
        }
        for code in codes_in(line) {
            mentions.push(Use {
                code,
                at: format!("{SPEC}:{number}"),
            });
        }
    }
    mentions
}

/// Every `MR` followed by exactly four digits, with no letter or digit
/// on either side.
pub fn codes_in(line: &str) -> Vec<Code> {
    let bytes = line.as_bytes();
    let mut codes = Vec::new();
    let mut start = 0;
    while let Some(found) = line[start..].find("MR") {
        let at = start + found;
        start = at + 2;
        let end = at + 6;
        let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
        let after_ok = bytes.get(end).is_none_or(|b| !b.is_ascii_alphanumeric());
        if before_ok
            && after_ok
            && let Some(code) = line.get(at..end).and_then(|text| text.parse().ok())
        {
            codes.push(code);
        }
    }
    codes
}

/// Codes expected by the executable examples of a Markdown file: `mora`
/// blocks whose first line is `// error: MRnnnn` or `// warning: MRnnnn`.
fn examples(path: &str, text: &str) -> Vec<Use> {
    spec_examples::extract(path, None, text)
        .into_iter()
        .filter_map(|example| match example.expects {
            Expects::Diagnostic(_, code) => Some(Use {
                code,
                at: example.at,
            }),
            Expects::Output => None,
        })
        .collect()
}

/// The template sections of `docs/errors/README.md`, in order.
const SECTIONS: &[&str] = &["Failing example", "Fixed example"];

/// A page has a title naming its code, the sections of [`SECTIONS`] in
/// order, and an executable example in each example section.
fn check_page(code: Code, text: &str) -> Result<(), String> {
    let title = format!("# {code}: ");
    if !text
        .lines()
        .next()
        .is_some_and(|line| line.starts_with(&title))
    {
        return Err(format!("the first line must start with `{title}`"));
    }
    let sections = sections(text);
    let headings: Vec<&str> = sections.iter().map(|(heading, _)| *heading).collect();
    if headings != SECTIONS {
        return Err(format!(
            "the sections must be `## {}`, in that order",
            SECTIONS.join("`, `## ")
        ));
    }
    let first_line = |body: &str| -> Option<String> {
        let block = body.split("```mora\n").nth(1)?;
        block.lines().next().map(str::to_owned)
    };
    let failing = first_line(&sections[0].1);
    if !matches!(
        failing.as_deref().and_then(spec_examples::expects),
        Some(Expects::Diagnostic(_, found)) if found == code
    ) {
        return Err(format!(
            "the failing example must be a `mora` block whose first line is `// error: {code}` or `// warning: {code}`"
        ));
    }
    if !first_line(&sections[1].1).is_some_and(|line| line.starts_with("// expect:")) {
        return Err(
            "the fixed example must be a `mora` block whose first line is `// expect:`".into(),
        );
    }
    Ok(())
}

/// The `## ` sections of a Markdown text, with the text under each.
fn sections(text: &str) -> Vec<(&str, String)> {
    let mut sections: Vec<(&str, String)> = Vec::new();
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            sections.push((heading, String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    sections
}

/// Reads the spec, the pages and the uses in crate sources and UI tests.
pub fn load(root: &Path) -> io::Result<Repo> {
    let spec = fs::read_to_string(root.join(SPEC))?;
    let mut pages = Vec::new();
    let pages_dir = root.join(PAGES);
    if pages_dir.is_dir() {
        for entry in fs::read_dir(&pages_dir)? {
            let path = entry?.path();
            if path.is_file()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                pages.push((name.to_owned(), fs::read_to_string(&path)?));
            }
        }
    }
    pages.sort();

    let mut uses = Vec::new();
    let mut bypasses = Vec::new();
    let crates = root.join("crates");
    for file in paths::files_with_extension(&crates, "rs")? {
        let Some(crate_name) = source_crate(&crates, &file) else {
            continue;
        };
        let at = paths::relative(root, &file);
        let text = fs::read_to_string(&file)?;
        if crate_name != REGISTRY_CRATE {
            for (number, line) in (1usize..).zip(text.lines()) {
                for (pattern, reason) in BYPASSES {
                    if path_matches(line, pattern).next().is_some() {
                        bypasses.push((format!("{at}:{number}"), *reason));
                    }
                }
            }
        }
        scan(&at, &text, &mut uses, |line| {
            path_matches(line, "codes::")
                .filter_map(|rest| {
                    let boundary = rest
                        .as_bytes()
                        .get(6)
                        .is_none_or(|b| !b.is_ascii_alphanumeric());
                    rest.get(..6).filter(|_| boundary)?.parse().ok()
                })
                .collect()
        });
    }
    for file in paths::files_with_extension(&root.join("tests/ui"), "mr")? {
        let text = fs::read_to_string(&file)?;
        scan(&paths::relative(root, &file), &text, &mut uses, |line| {
            ui::line_annotations(line)
                .into_iter()
                .filter_map(|annotation| annotation.ok().map(|(_, code)| code))
                .collect()
        });
    }
    Ok(Repo {
        spec,
        pages,
        uses,
        bypasses,
    })
}

/// The crate a file belongs to when it is in that crate's `src`
/// directory: `crates/<name>/src/...`. Only the path below `crates` counts,
/// so a checkout inside a directory named `src` changes nothing.
fn source_crate<'a>(crates: &Path, file: &'a Path) -> Option<&'a str> {
    let mut components = file.strip_prefix(crates).ok()?.components();
    let name = components.next()?.as_os_str().to_str()?;
    (components.next()?.as_os_str() == "src").then_some(name)
}

/// The text after each match of `pattern` in `line` that starts a path
/// segment: `codes::` in `mora_diag::codes::MR0001`, but not in
/// `barcodes::X`.
fn path_matches<'a>(line: &'a str, pattern: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    line.match_indices(pattern)
        .filter_map(move |(index, found)| {
            let before = line[..index].chars().next_back();
            let starts_segment = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            starts_segment.then(|| &line[index + found.len()..])
        })
}

/// Adds to `uses` the codes that `codes` finds on each line of `text`, the
/// file at `at`.
fn scan(at: &str, text: &str, uses: &mut Vec<Use>, codes: impl Fn(&str) -> Vec<Code>) {
    for (number, line) in (1usize..).zip(text.lines()) {
        for code in codes(line) {
            uses.push(Use {
                code,
                at: format!("{at}:{number}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mora_diag::registry::{CODES, RANGES};

    use super::*;

    const LEXICAL: &[CodeRange] = &[CodeRange::new(1, 99, "Lexical")];

    const MR0001: Entry = Entry {
        code: Code::new(1),
        summary: "a source file is not UTF-8",
    };

    const SPEC_TEXT: &str = "\
| `MR0001`-`MR0099` | Lexical |

A file that is not UTF-8 is error `MR0001`.
";

    const PAGE: &str = include_str!("../fixtures/error-codes/unregistered/docs/errors/MR0001.md");

    fn repo(spec: &str, pages: &[(&str, &str)], uses: &[(u16, &str)]) -> Repo {
        Repo {
            spec: spec.into(),
            pages: pages
                .iter()
                .map(|(file, text)| ((*file).into(), (*text).into()))
                .collect(),
            uses: uses
                .iter()
                .map(|(code, at)| Use {
                    code: Code::new(*code),
                    at: (*at).into(),
                })
                .collect(),
            ..Repo::default()
        }
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/error-codes/unregistered")
    }

    #[test]
    fn fixture_with_an_unregistered_code_fails() {
        let repo = load(&fixture()).unwrap();
        let problems: Vec<String> = check(&repo, LEXICAL, &[MR0001])
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            problems,
            [
                "docs/design/mora.md:7: MR0004 is not in crates/mora-diag/src/registry.rs",
                "tests/ui/encoding.mr:2: MR2002 is not in crates/mora-diag/src/registry.rs",
                "tests/ui/nested/unknown.mr:2: MR0042 is not in crates/mora-diag/src/registry.rs",
                "crates/demo/src/bypass.rs:1: name each code as `codes::MRnnnn`; a braced import hides the use",
                "crates/demo/src/bypass.rs:2: name each code as `codes::MRnnnn`; a glob import hides the use",
                "crates/demo/src/bypass.rs:5: report registered codes as `codes::MRnnnn`, not `Code::new`",
            ]
        );
    }

    #[test]
    fn fixture_uses_come_from_sources_and_ui_tests_only() {
        let repo = load(&fixture()).unwrap();
        let uses: Vec<String> = repo
            .uses
            .iter()
            .map(|u| format!("{} {}", u.at, u.code))
            .collect();
        assert_eq!(
            uses,
            [
                "crates/demo/src/lib.rs:2 MR0001",
                "crates/demo/src/lib.rs:7 MR0001",
                "tests/ui/encoding.mr:1 MR0001",
                "tests/ui/encoding.mr:2 MR0001",
                "tests/ui/encoding.mr:2 MR2002",
                "tests/ui/nested/unknown.mr:2 MR0042",
            ]
        );
    }

    #[test]
    fn only_paths_below_crates_decide_what_is_a_source() {
        let crates = Path::new("/home/dev/src/mora/crates");
        let file = |rest: &str| crates.join(rest);
        assert_eq!(source_crate(crates, &file("demo/src/lib.rs")), Some("demo"));
        assert_eq!(source_crate(crates, &file("demo/src/a/b.rs")), Some("demo"));
        assert_eq!(source_crate(crates, &file("demo/tests/t.rs")), None);
        assert_eq!(source_crate(crates, &file("demo/benches/src/x.rs")), None);
        assert_eq!(source_crate(crates, &file("lib.rs")), None);
    }

    #[test]
    fn patterns_must_start_a_path_segment() {
        let after = |line: &'static str, pattern: &'static str| -> Vec<&'static str> {
            path_matches(line, pattern).collect()
        };
        assert_eq!(after("mora_diag::codes::MR0001;", "codes::"), ["MR0001;"]);
        assert_eq!(after("codes::MR0001", "codes::"), ["MR0001"]);
        assert_eq!(
            after("barcodes::{A} my_codes::B", "codes::"),
            Vec::<&str>::new()
        );
        assert_eq!(
            after("ErrorCode::new(1) Code::new(2)", "Code::new("),
            ["2)"]
        );
        assert_eq!(after("mora_diag::Code::new(3)", "Code::new("), ["3)"]);
    }

    #[test]
    fn clean_repo_passes() {
        let repo = repo(SPEC_TEXT, &[("MR0001.md", PAGE)], &[(1, "src/lib.rs:3")]);
        assert_eq!(check(&repo, LEXICAL, &[MR0001]), []);
    }

    #[test]
    fn used_code_without_a_page_fails() {
        let repo = repo(SPEC_TEXT, &[], &[(1, "src/lib.rs:3")]);
        assert_eq!(
            check(&repo, LEXICAL, &[MR0001]),
            [Problem::MissingPage {
                code: Code::new(1),
                at: "src/lib.rs:3".into()
            }]
        );
    }

    #[test]
    fn example_in_the_spec_counts_as_a_use() {
        let spec = format!("{SPEC_TEXT}\n```mora\n// error: MR0001\nlet x = 1;\n```\n");
        let problems = check(&repo(&spec, &[], &[]), LEXICAL, &[MR0001]);
        assert_eq!(
            problems,
            [Problem::MissingPage {
                code: Code::new(1),
                at: "docs/design/mora.md:6".into()
            }]
        );
    }

    #[test]
    fn page_without_a_registered_code_fails() {
        let repo = repo(
            SPEC_TEXT,
            &[
                ("MR0001.md", PAGE),
                ("MR0002.md", PAGE),
                ("notes.md", ""),
                ("README.md", ""),
            ],
            &[],
        );
        assert_eq!(
            check(&repo, LEXICAL, &[MR0001]),
            [
                Problem::StrayPage {
                    file: "MR0002.md".into()
                },
                Problem::StrayPage {
                    file: "notes.md".into()
                },
            ]
        );
    }

    #[test]
    fn code_outside_its_range_fails() {
        let spec = format!("{SPEC_TEXT}Unknown escape is `MR0150`.\n");
        let stray = Entry {
            code: Code::new(150),
            summary: "x",
        };
        let problems = check(&repo(&spec, &[], &[]), LEXICAL, &[MR0001, stray]);
        assert_eq!(
            problems,
            [
                Problem::OutOfRange {
                    code: Code::new(150),
                    at: "registry".into()
                },
                Problem::OutOfRange {
                    code: Code::new(150),
                    at: "docs/design/mora.md:4".into()
                },
            ]
        );
    }

    #[test]
    fn registry_and_spec_must_agree() {
        let unsorted = [
            MR0001,
            Entry {
                code: Code::new(5),
                summary: "x",
            },
            MR0001,
        ];
        let problems = check(
            &repo("| `MR0001`-`MR0050` | Lexical |\n", &[], &[]),
            LEXICAL,
            &unsorted,
        );
        assert!(
            matches!(problems[0], Problem::RangeTable { .. }),
            "{problems:?}"
        );
        assert!(problems.contains(&Problem::Unsorted { code: Code::new(1) }));
        assert!(problems.contains(&Problem::NotInSpec { code: Code::new(5) }));
    }

    #[test]
    fn malformed_pages_are_reported() {
        let cases = [
            (PAGE.replace("# MR0001:", "# MR0002:"), "first line"),
            (PAGE.replace("## Failing example", "## Example"), "sections"),
            (
                PAGE.replace("// error: MR0001", "// note: MR0001"),
                "failing example",
            ),
            (PAGE.replace("// expect:", "// output:"), "fixed example"),
        ];
        for (page, expected) in cases {
            let problems = check(
                &repo(SPEC_TEXT, &[("MR0001.md", &page)], &[]),
                LEXICAL,
                &[MR0001],
            );
            let [Problem::BadPage { reason, .. }] = problems.as_slice() else {
                panic!("{expected}: {problems:?}");
            };
            assert!(reason.contains(expected), "{reason}");
        }
    }

    #[test]
    fn the_page_template_is_not_a_use() {
        let template = "# MRnnnn: title\n\n```mora\n// error: MR0002\n```\n";
        let repo = repo(
            SPEC_TEXT,
            &[("MR0001.md", PAGE), ("README.md", template)],
            &[],
        );
        assert_eq!(check(&repo, LEXICAL, &[MR0001]), []);
    }

    #[test]
    fn codes_need_word_boundaries() {
        let found: Vec<String> = codes_in("MR0001, `MR0310`; xMR0002 MR00030 MR12 MR9999")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(found, ["MR0001", "MR0310", "MR9999"]);
    }

    #[test]
    fn repository_passes() {
        let repo = load(&paths::root()).unwrap();
        let problems = check(&repo, RANGES, CODES);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
