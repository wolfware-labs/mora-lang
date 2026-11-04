use mora_syntax::ast;
use mora_syntax::lexer::ident_name;
use mora_syntax::{SyntaxKind, SyntaxToken, lex};

/// A folder or project name in PascalCase, or `None` when the result is not
/// a valid identifier (§4.2).
///
/// The name is split at `-` and `_`, the first letter of each part is made
/// upper case and the rest is kept: `users-api` gives `UsersApi`,
/// `http-client` and `HttpClient` both give `HttpClient`, and `2d` gives
/// `None`.
pub fn pascal_case(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    for part in name.split(['-', '_']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    is_identifier(&out).then_some(out)
}

/// Whether `text` lexes as exactly one identifier, which rules out keywords,
/// leading digits and characters an identifier cannot hold.
pub(crate) fn is_identifier(text: &str) -> bool {
    let lexed = lex(text);
    lexed.errors.is_empty()
        && matches!(lexed.tokens.as_slice(), [token] if token.kind == SyntaxKind::Ident)
}

/// The name an identifier token declares or refers to, in NFC, as names
/// are compared (§3.1).
pub(crate) fn token_name(token: SyntaxToken) -> String {
    ident_name(token.text()).into_owned()
}

/// The name a declaration writes, `None` when the parser recovered from a
/// missing one.
pub(crate) fn name_of(name: Option<ast::Name>) -> Option<String> {
    name?.ident_token().map(token_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_and_snake_case_join_into_pascal_case() {
        assert_eq!(pascal_case("users-api").as_deref(), Some("UsersApi"));
        assert_eq!(pascal_case("http-client").as_deref(), Some("HttpClient"));
        assert_eq!(pascal_case("json_value").as_deref(), Some("JsonValue"));
        assert_eq!(pascal_case("store").as_deref(), Some("Store"));
    }

    #[test]
    fn existing_capitals_are_kept() {
        assert_eq!(pascal_case("HttpClient").as_deref(), Some("HttpClient"));
        assert_eq!(pascal_case("ioUtils").as_deref(), Some("IoUtils"));
    }

    #[test]
    fn names_that_give_no_identifier_are_rejected() {
        for name in ["2d", "", "-", "a.b", "my folder", "a+b"] {
            assert_eq!(pascal_case(name), None, "{name:?}");
        }
    }

    #[test]
    fn non_ascii_letters_are_capitalized() {
        assert_eq!(pascal_case("élan-vital").as_deref(), Some("ÉlanVital"));
    }
}
