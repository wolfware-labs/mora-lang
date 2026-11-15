//! The values of character and string literal tokens (§3.5).
//!
//! These functions take the text of a token that [`lex`](super::lex)
//! produced without errors. They return `None` for text that is not such a
//! token.

use super::escape::escape;
use super::run;

/// The value of a [`Char`](crate::SyntaxKind::Char) token, such as `'a'` or `'\n'`.
pub fn char_value(text: &str) -> Option<char> {
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut chars = inner.chars();
    match chars.next()? {
        '\\' => match escape(inner) {
            Ok((c, len)) if len == inner.len() => Some(c),
            _ => None,
        },
        c if chars.next().is_none() => Some(c),
        _ => None,
    }
}

/// The value of a [`String`](crate::SyntaxKind::String) or
/// [`RawString`](crate::SyntaxKind::RawString) token, and whether it has the
/// `u8` suffix.
pub fn string_value(text: &str) -> Option<(String, bool)> {
    let (text, utf8) = match text.strip_suffix("u8") {
        Some(text) => (text, true),
        None => (text, false),
    };
    let value = if text.starts_with('`') {
        raw_value(text)?
    } else {
        regular_value(text.strip_prefix('"')?.strip_suffix('"')?)?
    };
    Some((value, utf8))
}

/// The value of an [`InterpText`](crate::SyntaxKind::InterpText) token of
/// an interpolated string that is not raw: its escapes decoded, and `{{`
/// and `}}` each one brace.
pub fn interp_text_value(text: &str) -> Option<String> {
    let mut value = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = match c {
            '\\' => {
                let (c, len) = escape(rest).ok()?;
                value.push(c);
                len
            }
            '{' | '}' if rest[1..].starts_with(c) => {
                value.push(c);
                2
            }
            '{' | '}' => return None,
            c => {
                value.push(c);
                c.len_utf8()
            }
        };
        rest = &rest[len..];
    }
    Some(value)
}

/// The values of the [`InterpText`](crate::SyntaxKind::InterpText) tokens
/// of a raw interpolated string, which `parts` lists in order with `None`
/// for each hole. On one line, each text is its value as written. On
/// several lines, as for a raw string, the opening and closing lines are
/// not part of the value and each line that starts in a text loses the
/// indentation of the closing delimiter. `None` when the parts are not
/// those of such a string.
pub fn raw_interp_text_values(parts: &[Option<&str>]) -> Option<Vec<String>> {
    if !parts.iter().flatten().any(|text| text.contains('\n')) {
        return Some(
            parts
                .iter()
                .flatten()
                .map(|&text| text.to_owned())
                .collect(),
        );
    }
    let (Some(Some(first)), Some(Some(last))) = (parts.first(), parts.last()) else {
        return None;
    };
    let first_break = first.find('\n')?;
    let last_break = last.rfind('\n')?;
    let indent = &last[last_break + 1..];
    let mut values = Vec::new();
    for (index, text) in parts.iter().enumerate() {
        let Some(text) = text else { continue };
        let start = if index == 0 { first_break + 1 } else { 0 };
        let end = if index + 1 == parts.len() {
            last_break
        } else {
            text.len()
        };
        let mut value = String::with_capacity(text.len());
        // The first text starts a line; any other starts after a hole.
        let mut line_start = index == 0;
        for line in text
            .get(start..end)
            .unwrap_or_default()
            .split_inclusive('\n')
        {
            if line_start {
                value.push_str(
                    line.strip_prefix(indent)
                        .unwrap_or(line.trim_start_matches([' ', '\t'])),
                );
            } else {
                value.push_str(line);
            }
            line_start = line.ends_with('\n');
        }
        // The break before the closing line is not part of the value.
        if index + 1 == parts.len() && value.ends_with('\r') {
            value.pop();
        }
        values.push(value);
    }
    Some(values)
}

/// Decodes the escapes of a regular string's contents.
fn regular_value(body: &str) -> Option<String> {
    let mut value = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find('\\') {
        value.push_str(&rest[..at]);
        let (c, len) = escape(&rest[at..]).ok()?;
        value.push(c);
        rest = &rest[at + len..];
    }
    value.push_str(rest);
    Some(value)
}

/// The contents of a raw string: as written on one line; on several lines,
/// the lines between the delimiters' lines with the closing delimiter's
/// indentation removed. Line breaks are kept as written.
fn raw_value(text: &str) -> Option<String> {
    let quotes = run(text, b'`');
    let content = text.get(quotes..text.len().checked_sub(quotes)?)?;
    // A raw string is a raw interpolated string without holes.
    raw_interp_text_values(&[Some(content)])?.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chars() {
        for (text, value) in [
            ("'a'", 'a'),
            ("'𝄞'", '𝄞'),
            (r"'\''", '\''),
            (r"'\n'", '\n'),
            (r"'\u0041'", 'A'),
            (r"'\u{1D11E}'", '𝄞'),
            ("'\"'", '"'),
        ] {
            assert_eq!(char_value(text), Some(value), "{text}");
        }
        assert_eq!(char_value("'ab'"), None);
        assert_eq!(char_value("''"), None);
        assert_eq!(char_value(r"'\nx'"), None);
    }

    #[test]
    fn strings() {
        for (text, value, utf8) in [
            (r#""a\tb""#, "a\tb", false),
            (r#""""#, "", false),
            (r#""\u{48}i \"there\"""#, "Hi \"there\"", false),
            (r#""abc"u8"#, "abc", true),
            ("`one \"quoted\" line`", "one \"quoted\" line", false),
            ("`\n    a\n      b\n    `", "a\n  b", false),
            ("`\r\n  x\r\n  `", "x", false),
            ("``\n  ` inside\n  ``", "` inside", false),
            ("`\n  a\n\n  b\n  `u8", "a\n\nb", true),
            ("`\n  `", "", false),
            ("`\r\n`", "", false),
            ("`C:\\data\\x.json`", "C:\\data\\x.json", false),
            ("``a`b``u8", "a`b", true),
            ("`\n    \"a\"\n      b\n    `", "\"a\"\n  b", false),
        ] {
            assert_eq!(string_value(text), Some((value.to_owned(), utf8)), "{text}");
        }
    }

    #[test]
    fn interpolated_text() {
        for (text, value) in [
            ("Hello, ", "Hello, "),
            (r"a\tb\n", "a\tb\n"),
            ("{{x}}", "{x}"),
            (r#"\"{{"#, "\"{"),
            (r"\u{7B}}}", "{}"),
            (r"\\{{", "\\{"),
            ("", ""),
        ] {
            assert_eq!(interp_text_value(text).as_deref(), Some(value), "{text}");
        }
        assert_eq!(interp_text_value(r"\q"), None);
        assert_eq!(interp_text_value("a{b"), None);
        assert_eq!(interp_text_value("a}b"), None);
    }

    #[test]
    fn raw_interpolated_text() {
        let values = |parts: &[Option<&str>]| raw_interp_text_values(parts);
        let owned = |values: &[&str]| Some(values.iter().map(|v| (*v).to_owned()).collect());
        // One line: the text as written, backslashes and lone braces too.
        assert_eq!(
            values(&[Some("n = "), None, Some(r" \q {")]),
            owned(&["n = ", r" \q {"])
        );
        assert_eq!(values(&[None]), owned(&[]));
        // Several lines: the opening and closing lines go, and each line
        // loses the closing delimiter's indentation.
        assert_eq!(
            values(&[Some("\n    a "), None, Some(" b\n      c\n    ")]),
            owned(&["a ", " b\n  c"])
        );
        // A line that starts with a hole keeps the indentation before it.
        assert_eq!(
            values(&[Some("\n  x\n  "), None, Some("\n  ")]),
            owned(&["x\n", ""])
        );
        // A blank line keeps its break; a line that runs on after a hole is
        // not a new line.
        assert_eq!(
            values(&[Some("\n  a\n\n  "), None, Some("b\n  c\n  ")]),
            owned(&["a\n\n", "b\nc"])
        );
        assert_eq!(values(&[Some("\r\n  x\r\n  ")]), owned(&["x"]));
        assert_eq!(values(&[Some("\n  ")]), owned(&[""]));
        // A multi-line string starts and ends with text.
        assert_eq!(values(&[None, Some("\n  ")]), None);
        assert_eq!(values(&[Some("\n  a\n"), None]), None);
    }
}
