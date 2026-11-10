//! The functions of the standard library that never return (D-138).

/// The names of the standard library's functions of the never type (§11.4),
/// which a path ends at. Until the standard library is loaded they are
/// recognised by name.
const DIVERGING: [&str; 3] = ["panic", "Unreachable", "Todo"];

/// Whether a call to `path` never returns (D-138): its last segment names
/// a function of the never type.
pub fn is_diverging(path: &[String]) -> bool {
    path.last()
        .is_some_and(|name| DIVERGING.contains(&name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_diverging_matches_d138_names() {
        for name in ["Todo", "Unreachable", "panic"] {
            assert!(is_diverging(&[name.to_owned()]), "{name}");
            assert!(
                is_diverging(&["Std".to_owned(), name.to_owned()]),
                "Std.{name}"
            );
        }
        assert!(!is_diverging(&[
            "Console".to_owned(),
            "WriteLine".to_owned()
        ]));
        assert!(!is_diverging(&["Panic".to_owned()]));
        assert!(!is_diverging(&["panic".to_owned(), "WriteLine".to_owned()]));
        assert!(!is_diverging(&[]));
    }
}
