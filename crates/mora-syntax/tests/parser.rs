// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Parser snapshot tests (M1.3). Each `tests/parser/ok/*.mr` and
//! `tests/parser/err/*.mr` file sits next to a `.rast` file holding its tree
//! and its errors. `ok` files must parse without errors. To update the
//! snapshots after a deliberate change, run
//! `MORA_BLESS=1 cargo test -p mora-syntax --test parser` and review the diff.
//! The tests by grammar area are in `tests/grammar`.

use std::fmt::Write;
use std::path::Path;

use mora_syntax::parse;

mod common;

// The snapshots and every helper here that parses run it.
use common::assert_errors_hold_tokens;

fn render(text: &str) -> String {
    assert_errors_hold_tokens(text);
    let parse = parse(text);
    let mut out = parse.syntax().debug_tree();
    for error in parse.errors() {
        // Writing to a `String` cannot fail.
        let _ = writeln!(
            out,
            "error {} {:?}: {}",
            error.code, error.range, error.message
        );
    }
    out
}

#[test]
fn snapshots() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parser");
    let bless = std::env::var_os("MORA_BLESS").is_some();
    let mut failures = Vec::new();
    let mut count = 0;
    for group in ["ok", "err"] {
        let mut files: Vec<_> = std::fs::read_dir(dir.join(group))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "mr"))
            .collect();
        files.sort();
        for path in files {
            count += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let parse = parse(&text);
            assert_eq!(
                parse.syntax().text(),
                text,
                "{}: not lossless",
                path.display()
            );
            let has_errors = !parse.errors().is_empty();
            if (group == "ok") == has_errors {
                failures.push(format!(
                    "{}: {}",
                    path.display(),
                    if has_errors {
                        "unexpected errors"
                    } else {
                        "expected errors"
                    }
                ));
            }
            let rendered = render(&text);
            let snapshot = path.with_extension("rast");
            if bless {
                std::fs::write(&snapshot, &rendered).unwrap();
            } else if std::fs::read_to_string(&snapshot).ok().as_deref() != Some(rendered.as_str())
            {
                failures.push(format!(
                    "{}: snapshot differs; run MORA_BLESS=1 cargo test -p mora-syntax --test parser\n{rendered}",
                    path.display()
                ));
            }
        }
    }
    assert!(count >= 10, "found only {count} parser tests");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn every_spec_example_parses_without_panicking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let spec = std::fs::read_to_string(root.join("docs/design/mora.md")).unwrap();
    let mut block: Option<String> = None;
    let mut count = 0;
    for line in spec.lines() {
        match &mut block {
            None if line.trim_start() == "```mora" => block = Some(String::new()),
            Some(text) if line.trim_start() == "```" => {
                assert_eq!(parse(text).syntax().text(), *text);
                count += 1;
                block = None;
            }
            Some(text) => {
                text.push_str(line);
                text.push('\n');
            }
            None => {}
        }
    }
    assert!(count >= 20, "{count}");
}

/// Every program the spec and the error pages run (`// expect:` first)
/// parses without errors: the examples are the language as documented.
#[test]
fn every_runnable_example_parses_cleanly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut docs = vec![root.join("docs/design/mora.md")];
    for entry in std::fs::read_dir(root.join("docs/errors")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|name| name != "README.md") {
            docs.push(path);
        }
    }
    let mut checked = 0;
    let mut failures = Vec::new();
    for doc in docs {
        let text = std::fs::read_to_string(&doc).unwrap();
        let mut block: Option<String> = None;
        for line in text.lines() {
            match &mut block {
                None if line.trim_start() == "```mora" => block = Some(String::new()),
                Some(example) if line.trim_start() == "```" => {
                    if example.starts_with("// expect:") {
                        checked += 1;
                        let errors: Vec<_> = parse(example)
                            .errors()
                            .iter()
                            .map(|e| e.message.clone())
                            .collect();
                        if !errors.is_empty() {
                            failures.push(format!("{}: {errors:?}\n{example}", doc.display()));
                        }
                    }
                    block = None;
                }
                Some(example) => {
                    example.push_str(line);
                    example.push('\n');
                }
                None => {}
            }
        }
    }
    assert!(checked >= 10, "{checked}");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
