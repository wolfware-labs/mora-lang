//! Cases that span several files.
//!
//! A case is one `.mr` file. Lines that start with `//@` describe the
//! project it stands for:
//!
//! - `//@ project kind=lib name=users-api namespace=UsersApi entry=src/Main.mr`
//!   sets the project's manifest values; each key is optional, and the
//!   defaults are an app named `app` with the entry file `src/Program.mr`.
//! - `//@ file src/Store/UserStore.mr` starts a file: the lines after it, up
//!   to the next `//@ file`, are its text.
//!
//! A case without `//@ file` lines is one file, the entry file, shown under
//! the case's own path. Before the first `//@ file` line there may be only
//! `//@ project`, comments and blank lines.

use mora_span::ProjectKind;

/// A case split into the project it describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    /// The `[project] name`.
    pub name: String,
    /// The `[project] namespace`, when the case sets one.
    pub namespace: Option<String>,
    /// Whether the project is an app or a library.
    pub kind: ProjectKind,
    /// The entry file, relative to the project root.
    pub entry: String,
    /// The files, in the order of the case.
    pub files: Vec<CaseFile>,
}

/// One file of a case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseFile {
    /// The path diagnostics show.
    pub shown: String,
    /// The path relative to the project root.
    pub path: String,
    /// The file's text.
    pub text: String,
}

const PROJECT: &str = "//@ project";
const FILE: &str = "//@ file";

/// Splits the case `text`, found at `path`, into its project.
pub fn split(path: &str, text: &str) -> Result<Case, String> {
    let mut case = Case {
        name: "app".to_owned(),
        namespace: None,
        kind: ProjectKind::App,
        entry: "src/Program.mr".to_owned(),
        files: Vec::new(),
    };
    let mut current: Option<CaseFile> = None;
    for (number, line) in (1usize..).zip(text.split_inclusive('\n')) {
        let Some(directive) = line.trim().strip_prefix("//@") else {
            if let Some(file) = &mut current {
                file.text.push_str(line);
            }
            continue;
        };
        let mut words = directive.split_whitespace();
        match words.next() {
            Some("file") => {
                let (Some(file), None) = (words.next(), words.next()) else {
                    return Err(format!("line {number}: `{FILE}` takes one path"));
                };
                if !file.starts_with("src/") || !file.ends_with(".mr") {
                    return Err(format!(
                        "line {number}: `{FILE}` takes a path such as `src/Store/UserStore.mr`"
                    ));
                }
                case.files.extend(current.take());
                if case.files.iter().any(|it| it.path == file) {
                    return Err(format!("line {number}: `{file}` is listed twice"));
                }
                current = Some(CaseFile {
                    shown: file.to_owned(),
                    path: file.to_owned(),
                    text: String::new(),
                });
            }
            Some("project") => {
                if current.is_some() || !case.files.is_empty() {
                    return Err(format!(
                        "line {number}: `{PROJECT}` comes before the first `{FILE}`"
                    ));
                }
                project(&mut case, words).map_err(|problem| format!("line {number}: {problem}"))?;
            }
            _ => {
                return Err(format!(
                    "line {number}: `//@` is followed by `project` or `file`"
                ));
            }
        }
    }
    // Once a `//@ file` line is seen, a file is always open.
    let Some(last) = current else {
        case.files.push(CaseFile {
            shown: path.to_owned(),
            path: case.entry.clone(),
            text: text.to_owned(),
        });
        return Ok(case);
    };
    case.files.push(last);
    let preamble_has_code = text
        .lines()
        .take_while(|line| !line.trim().starts_with(FILE))
        .any(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//")
        });
    if preamble_has_code {
        return Err(format!("only comments come before the first `{FILE}`"));
    }
    Ok(case)
}

/// Reads the `key=value` pairs of a `//@ project` line.
fn project<'a>(case: &mut Case, pairs: impl Iterator<Item = &'a str>) -> Result<(), String> {
    for pair in pairs {
        let Some((key, value)) = pair.split_once('=') else {
            return Err(format!("`{pair}` is not `key=value`"));
        };
        match key {
            "kind" => {
                case.kind = match value {
                    "app" => ProjectKind::App,
                    "lib" => ProjectKind::Lib,
                    _ => return Err(format!("`kind` is `app` or `lib`, not `{value}`")),
                }
            }
            "name" => value.clone_into(&mut case.name),
            "namespace" => case.namespace = Some(value.to_owned()),
            "entry" => value.clone_into(&mut case.entry),
            _ => {
                return Err(format!(
                    "unknown key `{key}`; use `kind`, `name`, `namespace` or `entry`"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_without_files_is_the_entry_file() {
        let case = split("tests/ui/a.mr", "//@ project kind=lib\nlet x = 1;\n").unwrap();
        assert_eq!(case.kind, ProjectKind::Lib);
        assert_eq!(
            case.files,
            [CaseFile {
                shown: "tests/ui/a.mr".into(),
                path: "src/Program.mr".into(),
                text: "//@ project kind=lib\nlet x = 1;\n".into(),
            }]
        );
    }

    #[test]
    fn file_lines_split_the_case() {
        let text = "// a comment\n//@ project name=users-api entry=src/Main.mr\n\
                    //@ file src/Main.mr\nlet x = 1;\n//@ file src/Store/A.mr\nclass A {}\n";
        let case = split("tests/ui/a.mr", text).unwrap();
        assert_eq!(case.name, "users-api");
        assert_eq!(case.entry, "src/Main.mr");
        let files: Vec<_> = case
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.text.as_str()))
            .collect();
        assert_eq!(
            files,
            [
                ("src/Main.mr", "let x = 1;\n"),
                ("src/Store/A.mr", "class A {}\n")
            ]
        );
    }

    #[test]
    fn every_project_key_is_read() {
        let text = "//@ project kind=lib\n//@ project kind=app namespace=Acme.Users\n";
        let case = split("a.mr", text).unwrap();
        assert_eq!(case.kind, ProjectKind::App);
        assert_eq!(case.namespace.as_deref(), Some("Acme.Users"));
    }

    #[test]
    fn problems_name_their_line() {
        let error = split("a.mr", "// one\n// two\n//@ file A.mr\n").unwrap_err();
        assert!(error.starts_with("line 3: "), "{error}");
    }

    #[test]
    fn malformed_cases_are_rejected() {
        let bad = [
            ("let x = 1;\n//@ file src/A.mr\n", "only comments"),
            ("//@ file A.mr\n", "takes a path"),
            ("//@ file src/A.mr src/B.mr\n", "one path"),
            ("//@ file src/A.mr\n//@ file src/A.mr\n", "listed twice"),
            ("//@ file src/A.mr\n//@ project kind=lib\n", "comes before"),
            ("//@ project kind=exe\n", "`kind` is"),
            ("//@ project color=red\n", "unknown key"),
            ("//@ project kind\n", "is not `key=value`"),
            ("//@ files src/A.mr\n", "followed by"),
        ];
        for (text, problem) in bad {
            let error = split("a.mr", text).unwrap_err();
            assert!(error.contains(problem), "{text:?}: {error}");
        }
    }
}
