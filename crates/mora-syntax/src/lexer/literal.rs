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
    if !content.contains('\n') {
        return Some(content.to_owned());
    }
    let first_break = content.find('\n')? + 1;
    let last_break = content.rfind('\n')?;
    let indent = &content[last_break + 1..];
    // With no content lines, the opening and closing lines share one break.
    let body = content.get(first_break..last_break).unwrap_or_default();
    let mut value = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        value.push_str(
            line.strip_prefix(indent)
                .unwrap_or(line.trim_start_matches([' ', '\t'])),
        );
    }
    // The break before the closing line is not part of the value.
    if value.ends_with('\r') {
        value.pop();
    }
    Some(value)
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
}
