//! Numeric literals (§3.5): telling integers from reals, checking their
//! form, and their values.

use mora_diag::Code;
use mora_diag::codes;

use crate::SyntaxKind;

/// The suffix of an integer literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSuffix {
    /// No suffix: the type comes from the context (§3.5).
    None,
    /// `u`: `uint`.
    U,
    /// `l`: `long`.
    L,
    /// `ul` or `lu`: `ulong`.
    Ul,
}

/// The suffix of a real literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealSuffix {
    /// No suffix: `double` unless the context asks for `float`.
    None,
    /// `f`: `float`.
    F,
    /// `d`: `double`.
    D,
}

/// The value of an integer literal that the lexer accepted. The literal
/// fits in a `ulong`; whether it fits its type is decided by the checker.
pub fn int_value(text: &str) -> Option<(u64, IntSuffix)> {
    let parts = split(text)?;
    if parts.real {
        return None;
    }
    let value = u64::from_str_radix(&parts.digits.replace('_', ""), parts.radix).ok()?;
    Some((value, int_suffix(parts.suffix)?))
}

/// The value of a real literal that the lexer accepted.
pub fn real_value(text: &str) -> Option<(f64, RealSuffix)> {
    let parts = split(text)?;
    if !parts.real {
        return None;
    }
    let suffix = real_suffix(parts.suffix)?;
    let value = parts.digits.replace('_', "").parse::<f64>().ok()?;
    Some((value, suffix))
}

/// The integer suffix written `text`, in either case.
fn int_suffix(text: &str) -> Option<IntSuffix> {
    [
        ("", IntSuffix::None),
        ("u", IntSuffix::U),
        ("l", IntSuffix::L),
        ("ul", IntSuffix::Ul),
        ("lu", IntSuffix::Ul),
    ]
    .into_iter()
    .find(|(suffix, _)| text.eq_ignore_ascii_case(suffix))
    .map(|(_, suffix)| suffix)
}

/// The real suffix written `text`.
fn real_suffix(text: &str) -> Option<RealSuffix> {
    match text {
        "" => Some(RealSuffix::None),
        "f" | "F" => Some(RealSuffix::F),
        "d" | "D" => Some(RealSuffix::D),
        _ => None,
    }
}

/// Whether `text` is the `decimal` suffix, which is reserved.
fn is_decimal_suffix(text: &str) -> bool {
    matches!(text, "m" | "M")
}

/// The radix that a `0x` or `0b` prefix of `text` sets, in either case.
fn radix_prefix(text: &str) -> Option<u32> {
    match text.as_bytes() {
        [b'0', b'x' | b'X', ..] => Some(16),
        [b'0', b'b' | b'B', ..] => Some(2),
        _ => None,
    }
}

/// The kind of a number token and the problems with it, each about the
/// whole token.
pub(crate) fn check(text: &str) -> (SyntaxKind, Vec<(Code, String)>) {
    let Some(parts) = split(text) else {
        let hex = radix_prefix(text) == Some(16);
        let kind = if text.contains(['.', 'e', 'E']) && !hex {
            SyntaxKind::RealNumber
        } else {
            SyntaxKind::IntNumber
        };
        return (
            kind,
            vec![(codes::MR0014, format!("`{text}` is not a valid number"))],
        );
    };
    let kind = if parts.real {
        SyntaxKind::RealNumber
    } else {
        SyntaxKind::IntNumber
    };
    let mut errors = Vec::new();
    if let Some(problem) = separators(parts.digits, parts.radix) {
        errors.push((codes::MR0014, problem));
    }
    let suffix_ok = if parts.real {
        real_suffix(parts.suffix).is_some()
    } else {
        int_suffix(parts.suffix).is_some()
    };
    if parts.real && is_decimal_suffix(parts.suffix) {
        errors.push((
            codes::MR0014,
            "the `m` suffix (`decimal`) is reserved; Mora has no `decimal` type yet".into(),
        ));
        return (kind, errors);
    }
    if !suffix_ok {
        errors.push((
            codes::MR0014,
            format!("`{}` is not a suffix for this literal", parts.suffix),
        ));
        return (kind, errors);
    }
    if !errors.is_empty() {
        return (kind, errors);
    }
    if parts.real {
        match real_value(text) {
            Some((value, RealSuffix::F)) if (value as f32).is_infinite() => errors.push((
                codes::MR0010,
                "the literal is too large for a `float`".into(),
            )),
            Some((value, _)) if value.is_infinite() => errors.push((
                codes::MR0010,
                "the literal is too large for a `double`".into(),
            )),
            Some(_) => {}
            None => errors.push((codes::MR0014, format!("`{text}` is not a valid number"))),
        }
    } else if int_value(text).is_none() {
        errors.push((
            codes::MR0010,
            "the literal is larger than `ulong.MaxValue`".into(),
        ));
    }
    (kind, errors)
}

/// A number split into its digits (including any fraction and exponent)
/// and its suffix.
struct Parts<'a> {
    radix: u32,
    real: bool,
    digits: &'a str,
    suffix: &'a str,
}

/// Splits `text` into radix, digits and suffix, or `None` when the digits
/// themselves are malformed (no digits, a letter in the middle).
fn split(text: &str) -> Option<Parts<'_>> {
    let (radix, body) = match radix_prefix(text) {
        Some(radix) => (radix, &text[2..]),
        None => (10, text),
    };
    if radix != 10 {
        let end = body
            .find(|c: char| !(c.is_digit(radix) || c == '_'))
            .unwrap_or(body.len());
        let (digits, suffix) = body.split_at(end);
        if !digits.contains(|c: char| c.is_digit(radix)) {
            return None;
        }
        return Some(Parts {
            radix,
            real: false,
            digits,
            suffix,
        });
    }
    // Decimal: integer part, optional fraction, optional exponent, suffix.
    let bytes = body.as_bytes();
    let run = |from: usize| {
        from + bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b'_')
            .count()
    };
    let mut end = run(0);
    let mut real = false;
    if bytes.get(end) == Some(&b'.') {
        real = true;
        end = run(end + 1);
    }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        let mut exp = end + 1;
        if matches!(bytes.get(exp), Some(b'+' | b'-')) {
            exp += 1;
        }
        let exp_end = run(exp);
        if exp_end == exp {
            return None;
        }
        real = true;
        end = exp_end;
    }
    let (digits, suffix) = body.split_at(end);
    if matches!(real_suffix(suffix), Some(RealSuffix::F | RealSuffix::D))
        || is_decimal_suffix(suffix)
    {
        real = true;
    }
    Some(Parts {
        radix,
        real,
        digits,
        suffix,
    })
}

/// Why the `_` separators in `digits` are misplaced, if they are: a
/// separator must sit between two digits.
fn separators(digits: &str, radix: u32) -> Option<String> {
    let bytes = digits.as_bytes();
    let is_digit = |b: Option<&u8>| b.is_some_and(|&b| char::from(b).is_digit(radix));
    let misplaced = bytes.iter().enumerate().any(|(i, &b)| {
        b == b'_'
            && !(is_digit(bytes[..i].iter().rev().find(|&&b| b != b'_'))
                && is_digit(bytes[i + 1..].iter().find(|&&b| b != b'_')))
    });
    misplaced.then(|| "a `_` separator must sit between two digits".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes_of(text: &str) -> (SyntaxKind, Vec<Code>) {
        let (kind, errors) = check(text);
        (kind, errors.into_iter().map(|(code, _)| code).collect())
    }

    #[test]
    fn integers() {
        for (text, value, suffix) in [
            ("0", 0, IntSuffix::None),
            ("1234", 1234, IntSuffix::None),
            ("1_000_000", 1_000_000, IntSuffix::None),
            ("1__0", 10, IntSuffix::None),
            ("0x4D2", 0x4D2, IntSuffix::None),
            ("0X4d2", 0x4D2, IntSuffix::None),
            ("0xFF_FF", 0xFFFF, IntSuffix::None),
            ("0b1001", 9, IntSuffix::None),
            ("0B10_01", 9, IntSuffix::None),
            ("10u", 10, IntSuffix::U),
            ("10U", 10, IntSuffix::U),
            ("10l", 10, IntSuffix::L),
            ("10UL", 10, IntSuffix::Ul),
            ("10lu", 10, IntSuffix::Ul),
            ("0xFFul", 255, IntSuffix::Ul),
            ("18446744073709551615", u64::MAX, IntSuffix::None),
        ] {
            assert_eq!(codes_of(text), (SyntaxKind::IntNumber, vec![]), "{text}");
            assert_eq!(int_value(text), Some((value, suffix)), "{text}");
        }
    }

    #[test]
    fn reals() {
        for (text, value, suffix) in [
            ("1.5", 1.5, RealSuffix::None),
            ("1e9", 1e9, RealSuffix::None),
            ("1.5e-3", 1.5e-3, RealSuffix::None),
            ("1E+3", 1e3, RealSuffix::None),
            ("1_0.2_5", 10.25, RealSuffix::None),
            ("1.5f", 1.5, RealSuffix::F),
            ("2F", 2.0, RealSuffix::F),
            ("2d", 2.0, RealSuffix::D),
            ("3.0D", 3.0, RealSuffix::D),
            ("1e308", 1e308, RealSuffix::None),
        ] {
            assert_eq!(codes_of(text), (SyntaxKind::RealNumber, vec![]), "{text}");
            assert_eq!(real_value(text), Some((value, suffix)), "{text}");
        }
    }

    #[test]
    fn too_large_is_mr0010() {
        for (text, kind) in [
            ("18446744073709551616", SyntaxKind::IntNumber),
            ("0x1_0000_0000_0000_0000", SyntaxKind::IntNumber),
            ("1e309", SyntaxKind::RealNumber),
            ("1e39f", SyntaxKind::RealNumber),
        ] {
            assert_eq!(codes_of(text), (kind, vec![codes::MR0010]), "{text}");
        }
    }

    #[test]
    fn malformed_is_mr0014() {
        for text in [
            "0x", "0xg", "0b2", "0b", "1_", "1__", "0x_1", "1_.5", "1._5", "1e", "1e+", "1e_5",
            "1.5u", "10f5", "10ul5", "1m", "1.5m", "12abc", "1uu",
        ] {
            let (_, codes) = codes_of(text);
            assert_eq!(codes, vec![codes::MR0014], "{text}");
        }
    }

    #[test]
    fn decimal_suffix_is_reserved() {
        for text in ["1m", "1.5M", "2_0m"] {
            let (kind, errors) = check(text);
            assert_eq!(kind, SyntaxKind::RealNumber, "{text}");
            assert_eq!(
                errors,
                vec![(
                    codes::MR0014,
                    "the `m` suffix (`decimal`) is reserved; Mora has no `decimal` type yet"
                        .to_string()
                )],
                "{text}"
            );
        }
    }

    #[test]
    fn malformed_numbers_keep_their_kind() {
        for (text, kind) in [
            ("0xge", SyntaxKind::IntNumber),
            ("0Xge", SyntaxKind::IntNumber),
            ("12abc", SyntaxKind::IntNumber),
            ("1e", SyntaxKind::RealNumber),
            ("1._5", SyntaxKind::RealNumber),
        ] {
            assert_eq!(codes_of(text), (kind, vec![codes::MR0014]), "{text}");
        }
    }
}
