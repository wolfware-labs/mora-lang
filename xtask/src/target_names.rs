//! The target-name half of `cargo xtask layers` (spec §19.4 rule 1).
//!
//! Only `mora-target`, `mora-link` and the backend may branch on a target.
//! Elsewhere, a string literal that equals the name, triple, `os` or `arch`
//! of a built-in target is a violation, unless the file is in [`ALLOW`].
//! Literals are found by a small lexer, so comments never count, and
//! literals inside a Rust `cfg`, `cfg!` or `cfg_attr` are skipped: `cfg`
//! describes the host the code is built for, not a Mora target.

use std::fmt;
use std::io;
use std::path::Path;

use mora_target::{Target, builtin_names};

/// The crates that may match on target names, relative to the repository root.
pub const OWNERS: &[&str] = &[
    "crates/mora-target",
    "crates/mora-link",
    "crates/mora-codegen",
];

/// Files outside [`OWNERS`] that may name a target, each with the reason.
pub const ALLOW: &[(&str, &str)] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub word: String,
}

impl fmt::Display for Hit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: \"{}\" names a target; only mora-target, mora-link and mora-codegen may match on targets (§19.4)",
            self.path, self.line, self.word
        )
    }
}

/// The name, triple, `os` and `arch` of every built-in target, without duplicates.
pub fn words() -> Vec<String> {
    let mut words = Vec::new();
    for name in builtin_names() {
        let target = Target::builtin(name).expect("built-in targets parse");
        for word in [target.name, target.triple, target.os, target.arch] {
            if !words.contains(&word) {
                words.push(word);
            }
        }
    }
    words
}

/// The 1-based line numbers and words of the target names quoted in `text`.
pub fn scan(text: &str, words: &[String]) -> Vec<(usize, String)> {
    literals(text)
        .into_iter()
        .filter(|literal| !literal.in_cfg && words.contains(&literal.text))
        .map(|literal| (literal.line, literal.text))
        .collect()
}

struct Literal {
    line: usize,
    text: String,
    in_cfg: bool,
}

/// The string literals of Rust source `text`. Comments, nested block
/// comments included, are skipped, and character literals are told apart
/// from lifetimes. A literal inside the parentheses of `cfg`, `cfg!` or
/// `cfg_attr` is marked, since `cfg` describes the host the code is built
/// for. Escapes in ordinary strings are decoded, so `"\x6cinux"` is
/// `linux`; raw strings are taken as written.
fn literals(text: &str) -> Vec<Literal> {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied().unwrap_or_default();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    let mut line = 1;
    // The parenthesis depth inside a `cfg`, zero outside one.
    let mut cfg_depth = 0usize;
    // The last two characters of code before `i`, outside comments and
    // whitespace, latest first, and whether the latest `[` opened an
    // attribute (`#[` or `#![`).
    let mut prev = [' ', ' '];
    let mut attribute_bracket = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
        } else if let Some(end) = comment_end(&chars, i) {
            line += chars[i..end].iter().filter(|&&c| c == '\n').count();
            i = end;
        } else if (c.is_alphabetic() || c == '_') && !(i > 0 && is_ident(chars[i - 1])) {
            let mut j = i;
            while j < chars.len() && is_ident(chars[j]) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let in_attribute = prev[0] == '[' && attribute_bracket;
            prev = ['a', prev[0]];
            let mut k = j;
            while at(k) == '#' {
                k += 1;
            }
            if matches!(word.as_str(), "r" | "br" | "cr") && at(k) == '"' {
                // A raw string: r"...", r#"..."#, and so on.
                let hashes = k - j;
                let start = (line, k + 1);
                let mut end = k + 1;
                while end < chars.len() {
                    if chars[end] == '"' && (1..=hashes).all(|h| at(end + h) == '#') {
                        break;
                    }
                    line += usize::from(chars[end] == '\n');
                    end += 1;
                }
                found.push(Literal {
                    line: start.0,
                    text: chars[start.1..end.min(chars.len())].iter().collect(),
                    in_cfg: cfg_depth > 0,
                });
                i = end + 1 + hashes;
                continue;
            }
            i = j;
            // `cfg!(...)` anywhere, `cfg(...)` and `cfg_attr(...)` only right
            // after the `[` of an attribute: elsewhere they are calls.
            if cfg_depth == 0 && matches!(word.as_str(), "cfg" | "cfg_attr") {
                let mut k = j;
                let mut bang = false;
                loop {
                    if at(k) == '!' && !bang {
                        bang = true;
                        k += 1;
                    } else if at(k).is_whitespace() {
                        k += 1;
                    } else if let Some(end) = comment_end(&chars, k) {
                        k = end;
                    } else {
                        break;
                    }
                }
                let is_cfg = if bang { word == "cfg" } else { in_attribute };
                if at(k) == '(' && is_cfg {
                    line += chars[j..k].iter().filter(|&&c| c == '\n').count();
                    cfg_depth = 1;
                    i = k + 1;
                }
            }
        } else if c == '"' {
            let start = (line, i + 1);
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                if chars[j] == '\\' {
                    j += 1;
                }
                line += usize::from(at(j) == '\n');
                j += 1;
            }
            found.push(Literal {
                line: start.0,
                text: unescape(&chars[start.1..j.min(chars.len())]),
                in_cfg: cfg_depth > 0,
            });
            prev = ['"', prev[0]];
            i = j + 1;
        } else if c == '\'' {
            // A character literal, or a lifetime, which has no closing quote.
            prev = ['\'', prev[0]];
            if at(i + 1) == '\\' {
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
            } else if at(i + 2) == '\'' {
                i += 3;
            } else {
                i += 1;
            }
        } else {
            if !c.is_whitespace() {
                if c == '[' {
                    attribute_bracket = prev[0] == '#' || prev == ['!', '#'];
                }
                prev = [c, prev[0]];
            }
            if cfg_depth > 0 {
                match c {
                    '(' => cfg_depth += 1,
                    ')' => cfg_depth -= 1,
                    _ => {}
                }
            }
            i += 1;
        }
    }
    found
}

/// The value of an ordinary string literal's contents: `\n`, `\r`, `\t`,
/// `\0`, `\\`, `\'`, `\"`, `\xHH`, `\u{...}` and a backslash before a line
/// break, which drops the break and the whitespace after it. An escape that
/// does not decode is kept as written.
fn unescape(raw: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != '\\' || i + 1 == raw.len() {
            out.push(raw[i]);
            i += 1;
            continue;
        }
        let simple = match raw[i + 1] {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '0' => Some('\0'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            i += 2;
        } else if raw[i + 1] == '\n' {
            i += 2;
            while i < raw.len() && raw[i].is_whitespace() {
                i += 1;
            }
        } else if raw[i + 1] == 'x' && i + 3 < raw.len() {
            let hex: String = raw[i + 2..i + 4].iter().collect();
            match u8::from_str_radix(&hex, 16) {
                Ok(byte) => {
                    out.push(char::from(byte));
                    i += 4;
                }
                Err(_) => {
                    out.push('\\');
                    i += 1;
                }
            }
        } else if raw[i + 1] == 'u' && raw.get(i + 2) == Some(&'{') {
            let close = raw[i..].iter().position(|&c| c == '}').map(|k| i + k);
            let decoded = close.and_then(|close| {
                let hex: String = raw[i + 3..close].iter().filter(|&&c| c != '_').collect();
                let c = u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)?;
                Some((c, close))
            });
            match decoded {
                Some((c, close)) => {
                    out.push(c);
                    i = close + 1;
                }
                None => {
                    out.push('\\');
                    i += 1;
                }
            }
        } else {
            out.push('\\');
            i += 1;
        }
    }
    out
}

/// The index just past the comment that starts at `i`, if one does. Block
/// comments nest, as in Rust.
fn comment_end(chars: &[char], i: usize) -> Option<usize> {
    let at = |i: usize| chars.get(i).copied().unwrap_or_default();
    if at(i) != '/' {
        return None;
    }
    let mut j = i;
    match at(i + 1) {
        '/' => {
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
        }
        '*' => {
            let mut depth = 0;
            while j < chars.len() {
                if chars[j] == '/' && at(j + 1) == '*' {
                    depth += 1;
                    j += 2;
                } else if chars[j] == '*' && at(j + 1) == '/' {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
        }
        _ => return None,
    }
    Some(j.min(chars.len()))
}

/// Scans every `.rs` file under `crates/` outside [`OWNERS`] and [`ALLOW`].
pub fn check(root: &Path) -> io::Result<Vec<Hit>> {
    let words = words();
    let mut files = Vec::new();
    collect(root, &root.join("crates"), &mut files)?;
    files.sort();
    let mut hits = Vec::new();
    for path in files {
        let owned = OWNERS
            .iter()
            .any(|owner| path.starts_with(&format!("{owner}/")));
        if owned || ALLOW.iter().any(|(allowed, _)| *allowed == path) {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&path))?;
        for (line, word) in scan(&text, &words) {
            hits.push(Hit {
                path: path.clone(),
                line,
                word,
            });
        }
    }
    Ok(hits)
}

/// Adds the `.rs` files under `dir` to `files`, as `/`-separated paths relative to `root`.
fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let relative = path.strip_prefix(root).expect("under the root");
            let parts: Vec<_> = relative.iter().map(|part| part.to_string_lossy()).collect();
            files.push(parts.join("/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_cover_names_triples_os_and_arch() {
        let words = words();
        for word in [
            "x86_64-linux",
            "aarch64-pc-windows-gnullvm",
            "macos",
            "riscv64",
        ] {
            assert!(words.contains(&word.to_owned()), "{word}");
        }
        assert_eq!(words.iter().filter(|w| *w == "linux").count(), 1);
    }

    #[test]
    fn a_quoted_target_name_is_a_hit() {
        let text =
            "fn f(t: &str) -> bool {\n    t == \"x86_64-linux\" || t.ends_with(\"windows\")\n}\n";
        assert_eq!(
            scan(text, &words()),
            [(2, "x86_64-linux".to_owned()), (2, "windows".to_owned())]
        );
    }

    #[test]
    fn comments_cfg_and_partial_words_are_not_hits_but_later_lines_are() {
        let text = concat!(
            "// builds for \"x86_64-linux\"\n",
            "    /// on \"macos\"\n",
            "#[cfg(target_os = \"linux\")]\n",
            "if cfg!(target_os = \"windows\") {}\n",
            "#[cfg_attr(target_os = \"linux\", allow(dead_code))]\n",
            "/* one line \"linux\" */\n",
            "/*\n",
            " * on \"aarch64-macos\"\n",
            " */\n",
            "let s = \"linux-ish\";\n",
            "let t = \"my-linux\";\n",
            "let u = \"riscv64\";\n",
        );
        assert_eq!(scan(text, &words()), [(12, "riscv64".to_owned())]);
    }

    #[test]
    fn comments_are_skipped_but_code_beside_them_is_not() {
        let text = concat!(
            "let n = 0; // \"linux\"\n",
            "/* a */ let t = \"linux\";\n",
            "/* outer /* \"macos\" */ still \"windows\" */ let a = \"aarch64\";\n",
            "let c = '\"'; let w = \"windows\";\n",
            "fn f<'a>(s: &'a str) -> bool { s == r#\"x86_64\"# }\n",
            "let e = '\\''; let s = \"say \\\"linux\\\"\";\n",
            "let m = \"two\nlines\"; let r = \"riscv64\";\n",
        );
        assert_eq!(
            scan(text, &words()),
            [
                (2, "linux".to_owned()),
                (3, "aarch64".to_owned()),
                (4, "windows".to_owned()),
                (5, "x86_64".to_owned()),
                (8, "riscv64".to_owned()),
            ]
        );
    }

    #[test]
    fn only_literals_inside_cfg_parentheses_are_skipped() {
        let text = concat!(
            "let marker = \"cfg(\"; let t = \"linux\";\n",
            "let n = 0; // cfg(\n",
            "let w = \"windows\";\n",
            "#[cfg (target_os = \"linux\")]\n",
            "#[cfg_attr(all(target_os = \"macos\", unix), allow(dead_code))] fn g() -> &'static str { \"macos\" }\n",
            "if cfg!\n(target_arch = \"x86_64\") { f(\"aarch64\") }\n",
            "let config = \"riscv64\";\n",
            "#[cfg /* host */ (unix)] #[cfg_attr(\n",
            "    // why\n",
            "    target_os = \"linux\", inline)] fn h() {}\n",
            "if cfg! /* a\n b */ (target_os = \"windows\") { f(\"x86_64\") }\n",
            "fn cfg(s: &str) {} cfg(\"linux\"); x.cfg_attr(\"macos\");\n",
            "#![cfg(unix)] #[ /* c */ cfg_attr(target_os = \"linux\", inline)] fn k() {}\n",
            "let v = [cfg(\"windows\")]; # ! [cfg(target_os = \"macos\")]\n",
        );
        assert_eq!(
            scan(text, &words()),
            [
                (1, "linux".to_owned()),
                (3, "windows".to_owned()),
                (5, "macos".to_owned()),
                (7, "aarch64".to_owned()),
                (8, "riscv64".to_owned()),
                (13, "x86_64".to_owned()),
                (14, "linux".to_owned()),
                (14, "macos".to_owned()),
                (16, "windows".to_owned()),
            ]
        );
    }

    #[test]
    fn escapes_are_decoded_before_matching() {
        let text = concat!(
            r#"let a = "\x6cinux"; let b = "\u{6d}a\u{63}os";"#,
            "\n",
            r#"let c = "x86\x5f64"; let d = "win\
                  dows"; let e = r"\x6cinux";"#,
            "\n",
            r#"let f = "aarch64\n"; let g = "\u{zz}"; let h = "\q";"#,
            "\n",
        );
        assert_eq!(
            scan(text, &words()),
            [
                (1, "linux".to_owned()),
                (1, "macos".to_owned()),
                (2, "x86_64".to_owned()),
                (2, "windows".to_owned()),
            ]
        );
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(unescape(&chars(r#"\t\"\\\0\'"#)), "\t\"\\\0'");
        assert_eq!(
            unescape(&chars(r"\u{1F600}\u{10_FFFF}")),
            "\u{1F600}\u{10FFFF}"
        );
        assert_eq!(unescape(&chars(r"\u{zz}\x4\q\")), r"\u{zz}\x4\q\");
    }

    #[test]
    fn raw_strings_end_at_their_own_hashes() {
        let text = concat!(
            "let a = r\"a\" \"linux\";\n",
            "let b = r##\"a\"## \"macos\";\n",
            "let c = r\"windows\"; let d = r#\"\\x6cinux\"#;\n",
            "let e = r\"two\nlines\"; let f = \"riscv64\";\n",
            "#[cfg(feature = r\"linux\")] fn g() {}\n",
            "let h = 1r\"\\x6cinux\";\n",
        );
        assert_eq!(
            scan(text, &words()),
            [
                (1, "linux".to_owned()),
                (2, "macos".to_owned()),
                (3, "windows".to_owned()),
                (5, "riscv64".to_owned()),
                (7, "linux".to_owned()),
            ]
        );
    }

    #[test]
    fn escaped_quotes_do_not_end_strings_or_characters() {
        let text = concat!(
            "let a = \"\\\"\"; let b = \"linux\";\n",
            "let c = '\\\"'; let d = \"macos\";\n",
            "let e = ['\\x41','\\\"', \"windows\"];\n",
        );
        assert_eq!(
            scan(text, &words()),
            [
                (1, "linux".to_owned()),
                (2, "macos".to_owned()),
                (3, "windows".to_owned()),
            ]
        );
    }

    #[test]
    fn nested_parentheses_stay_inside_cfg() {
        let text = concat!(
            "#[cfg_attr(all(unix), doc = \"linux\")] fn f() {}\n",
            "#[doc(alias = \"macos\")] fn g() {}\n",
        );
        assert_eq!(scan(text, &words()), [(2, "macos".to_owned())]);
    }

    #[test]
    fn unterminated_text_at_the_end_is_not_read_past() {
        for text in [
            "let a = r\"abc",
            "let a = \"abc",
            "let a = '\\",
            "let a = 1; // end",
            "let a = 1; /* end",
        ] {
            assert_eq!(scan(text, &words()), [], "{text}");
        }
        assert_eq!(
            scan("let a = \"linux\"; a", &words()),
            [(1, "linux".to_owned())]
        );
    }

    #[test]
    fn line_breaks_and_unknown_escapes_decode() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(unescape(&chars(r"a\nb\r")), "a\nb\r");
        assert_eq!(unescape(&chars("x\\\n  ")), "x");
        assert_eq!(unescape(&chars(r"\x4")), r"\x4");
        assert_eq!(unescape(&chars(r"\a{41}")), r"\a{41}");
    }

    #[test]
    fn hits_name_their_place() {
        let hit = Hit {
            path: "crates/mora-cli/src/main.rs".to_owned(),
            line: 7,
            word: "linux".to_owned(),
        };
        assert_eq!(
            hit.to_string(),
            "crates/mora-cli/src/main.rs:7: \"linux\" names a target; only mora-target, mora-link and mora-codegen may match on targets (§19.4)"
        );
    }

    #[test]
    fn check_reads_every_crate_but_the_owners() {
        let root =
            std::env::temp_dir().join(format!("mora-xtask-target-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (file, text) in [
            (
                "crates/mora-cli/src/main.rs",
                "fn f() {}\nlet t = \"linux\";\n",
            ),
            ("crates/mora-cli/src/notes.txt", "\"linux\"\n"),
            ("crates/mora-target/src/lib.rs", "let t = \"linux\";\n"),
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let hits = check(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            hits,
            [Hit {
                path: "crates/mora-cli/src/main.rs".to_owned(),
                line: 2,
                word: "linux".to_owned(),
            }]
        );
    }

    #[test]
    fn allowed_files_exist() {
        let root = crate::paths::root();
        for (path, reason) in ALLOW {
            assert!(root.join(path).is_file(), "{path}");
            assert!(!reason.is_empty(), "{path}");
        }
    }

    #[test]
    fn the_workspace_has_no_hits() {
        let hits = check(&crate::paths::root()).unwrap();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
