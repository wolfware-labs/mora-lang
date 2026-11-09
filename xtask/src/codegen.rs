//! `cargo xtask codegen`: generates `SyntaxKind` and the typed AST views of
//! `mora-syntax` (spec §14.4).
//!
//! The reserved and contextual keywords come from §3.4 of the spec, the
//! punctuation from the tables below, and the nodes and their children from
//! `crates/mora-syntax/mora.ungram`. `--check` fails when the files on disk
//! differ from what the generator would write.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ungrammar::{Grammar, Rule};

use crate::paths::SPEC;

/// The grammar, relative to the repository root.
pub const GRAMMAR: &str = "crates/mora-syntax/mora.ungram";
/// The generated `SyntaxKind`, relative to the repository root.
pub const KINDS: &str = "crates/mora-syntax/src/generated/kinds.rs";
/// The generated AST views, relative to the repository root.
pub const NODES: &str = "crates/mora-syntax/src/generated/nodes.rs";

/// A token kind without fixed text.
struct OtherToken {
    /// The `SyntaxKind` variant.
    name: &'static str,
    /// How `mora.ungram` spells it, without quotes; also the accessor stem.
    spelling: &'static str,
    /// The variant's doc comment.
    doc: &'static str,
    /// Whether the parser skips it.
    trivia: bool,
}

/// Tokens without fixed text.
const OTHER_TOKENS: &[OtherToken] = &[
    OtherToken {
        name: "Whitespace",
        spelling: "whitespace",
        doc: "Spaces, tabs, a lone carriage return, a form feed, or a byte order mark at the start of the file.",
        trivia: true,
    },
    OtherToken {
        name: "Newline",
        spelling: "newline",
        doc: "`\\n` or `\\r\\n`.",
        trivia: true,
    },
    OtherToken {
        name: "LineComment",
        spelling: "line_comment",
        doc: "`//` to the end of the line.",
        trivia: true,
    },
    OtherToken {
        name: "DocComment",
        spelling: "doc_comment",
        doc: "`///` to the end of the line.",
        trivia: true,
    },
    OtherToken {
        name: "BlockComment",
        spelling: "block_comment",
        doc: "`/* ... */`.",
        trivia: true,
    },
    OtherToken {
        name: "Ident",
        spelling: "ident",
        doc: "A name, possibly with an `@` prefix.",
        trivia: false,
    },
    OtherToken {
        name: "IntNumber",
        spelling: "int_number",
        doc: "An integer literal, such as `0x4D2` or `10ul`.",
        trivia: false,
    },
    OtherToken {
        name: "RealNumber",
        spelling: "real_number",
        doc: "A real literal, such as `1.5e-3f`.",
        trivia: false,
    },
    OtherToken {
        name: "Char",
        spelling: "char_literal",
        doc: "A character literal, such as `'a'`.",
        trivia: false,
    },
    OtherToken {
        name: "String",
        spelling: "string_literal",
        doc: "A regular string literal, such as `\"a\\tb\"` or `\"abc\"u8`.",
        trivia: false,
    },
    OtherToken {
        name: "RawString",
        spelling: "raw_string",
        doc: "A raw string literal, delimited by backticks.",
        trivia: false,
    },
    OtherToken {
        name: "InterpStart",
        spelling: "interp_start",
        doc: "The start of an interpolated string, such as `$\"`, `` $` `` or `` $$` ``.",
        trivia: false,
    },
    OtherToken {
        name: "InterpText",
        spelling: "interp_text",
        doc: "Literal text inside an interpolated string.",
        trivia: false,
    },
    OtherToken {
        name: "HoleOpen",
        spelling: "hole_open",
        doc: "The `{` (or `{{` after `$$`) that opens a hole.",
        trivia: false,
    },
    OtherToken {
        name: "HoleFormat",
        spelling: "hole_format",
        doc: "The `:format` part of a hole, from the colon to the closing brace.",
        trivia: false,
    },
    OtherToken {
        name: "HoleClose",
        spelling: "hole_close",
        doc: "The `}` (or `}}` after `$$`) that closes a hole.",
        trivia: false,
    },
    OtherToken {
        name: "InterpEnd",
        spelling: "interp_end",
        doc: "The quotes that end an interpolated string.",
        trivia: false,
    },
    OtherToken {
        name: "Unknown",
        spelling: "unknown",
        doc: "A character that cannot start a token.",
        trivia: false,
    },
];

/// Punctuation as the lexer produces it (§3.6).
const PUNCTUATION: &[(&str, &str)] = &[
    ("LBrace", "{"),
    ("RBrace", "}"),
    ("LBracket", "["),
    ("RBracket", "]"),
    ("LParen", "("),
    ("RParen", ")"),
    ("Dot", "."),
    ("DotDot", ".."),
    ("Comma", ","),
    ("Colon", ":"),
    ("Semicolon", ";"),
    ("Question", "?"),
    ("QuestionQuestion", "??"),
    ("QuestionQuestionEq", "??="),
    ("QuestionDot", "?."),
    ("FatArrow", "=>"),
    ("ThinArrow", "->"),
    ("At", "@"),
    ("Dollar", "$"),
    ("Hash", "#"),
    ("Plus", "+"),
    ("Minus", "-"),
    ("Star", "*"),
    ("Slash", "/"),
    ("Percent", "%"),
    ("Amp", "&"),
    ("Pipe", "|"),
    ("Caret", "^"),
    ("Bang", "!"),
    ("Tilde", "~"),
    ("Shl", "<<"),
    ("EqEq", "=="),
    ("BangEq", "!="),
    ("Lt", "<"),
    ("Gt", ">"),
    ("LtEq", "<="),
    ("AmpAmp", "&&"),
    ("PipePipe", "||"),
    ("Eq", "="),
    ("PlusEq", "+="),
    ("MinusEq", "-="),
    ("StarEq", "*="),
    ("SlashEq", "/="),
    ("PercentEq", "%="),
    ("AmpEq", "&="),
    ("PipeEq", "|="),
    ("CaretEq", "^="),
    ("ShlEq", "<<="),
    ("PlusPlus", "++"),
    ("MinusMinus", "--"),
    ("PlusPercent", "+%"),
    ("MinusPercent", "-%"),
    ("StarPercent", "*%"),
    ("PlusPercentEq", "+%="),
    ("MinusPercentEq", "-%="),
    ("StarPercentEq", "*%="),
];

/// Punctuation the lexer produces in pieces and the parser joins (§14.4).
const JOINED: &[(&str, &str)] = &[
    ("GtEq", ">="),
    ("Shr", ">>"),
    ("ShrEq", ">>="),
    ("UShr", ">>>"),
    ("UShrEq", ">>>="),
    ("QuestionLBracket", "?["),
];

/// The node kind for text the parser could not fit into the grammar.
const ERROR_NODE: &str = "Error";

/// Rust keywords that cannot be method names as they are.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "yield", "abstract", "become", "box", "do", "final", "macro", "override",
    "priv", "typeof", "unsized", "virtual", "try", "gen",
];

/// A token kind and how the grammar and the accessors name it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenKind {
    /// The `SyntaxKind` variant.
    name: String,
    /// How `mora.ungram` spells it, without quotes.
    spelling: String,
    /// The accessor name without `_token`.
    stem: String,
}

/// The tokens of the language.
#[derive(Debug, Default)]
struct Tokens {
    /// Every token kind, in `SyntaxKind` order.
    all: Vec<TokenKind>,
    /// The reserved keywords of §3.4.
    keywords: Vec<String>,
    /// The contextual keywords of §3.4.
    contextual: Vec<String>,
}

impl Tokens {
    fn from_spec(spec: &str) -> Result<Self, String> {
        let keywords = reserved_keywords(spec)?;
        let contextual = contextual_keywords(spec)?;
        let mut all = Vec::new();
        for token in OTHER_TOKENS {
            all.push(TokenKind {
                name: token.name.into(),
                spelling: token.spelling.into(),
                stem: token.spelling.into(),
            });
        }
        for (name, text) in PUNCTUATION.iter().chain(JOINED) {
            all.push(TokenKind {
                name: (*name).into(),
                spelling: (*text).into(),
                stem: snake(name),
            });
        }
        for word in keywords.iter().chain(&contextual) {
            let stem = if word == "_" { "underscore" } else { word };
            all.push(TokenKind {
                name: keyword_kind(word),
                spelling: word.clone(),
                stem: stem.into(),
            });
        }
        Ok(Tokens {
            all,
            keywords,
            contextual,
        })
    }

    fn by_spelling(&self, spelling: &str) -> Option<&TokenKind> {
        self.all.iter().find(|token| token.spelling == spelling)
    }
}

/// The words of the reserved-keyword block of §3.4.
fn reserved_keywords(spec: &str) -> Result<Vec<String>, String> {
    let start = spec
        .find("Reserved keywords (never identifiers):")
        .ok_or("the spec has no reserved-keyword block in §3.4")?;
    let block = &spec[start..];
    let fence = "```text";
    let open = block
        .find(fence)
        .ok_or("the reserved-keyword block has no ```text fence")?
        + fence.len();
    let close = block[open..]
        .find("```")
        .ok_or("the reserved-keyword block is not closed")?
        + open;
    let words: Vec<String> = block[open..close]
        .split_whitespace()
        .map(String::from)
        .collect();
    if words.is_empty() {
        return Err("the reserved-keyword block is empty".into());
    }
    Ok(words)
}

/// The backquoted words of the contextual-keyword paragraph of §3.4.
fn contextual_keywords(spec: &str) -> Result<Vec<String>, String> {
    let start = spec
        .find("Contextual keywords (keywords only in the listed position):")
        .ok_or("the spec has no contextual-keyword paragraph in §3.4")?;
    let paragraph = spec[start..].split("\n\n").next().unwrap_or_default();
    let mut words = Vec::new();
    for (i, piece) in paragraph.split('`').enumerate() {
        let is_word = i % 2 == 1 && piece.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        if is_word && !piece.is_empty() && !words.iter().any(|w| w == piece) {
            words.push(piece.to_owned());
        }
    }
    if words.is_empty() {
        return Err("the contextual-keyword paragraph names no keywords".into());
    }
    Ok(words)
}

fn keyword_kind(word: &str) -> String {
    if word == "_" {
        return "Underscore".into();
    }
    let mut chars = word.chars();
    let first = chars.next().map(|c| c.to_ascii_uppercase()).into_iter();
    first.chain(chars).collect::<String>() + "Kw"
}

/// `LBrace` to `l_brace`, `StructDecl` to `struct_decl`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A method name, escaped when it is a Rust keyword.
fn method(name: &str) -> String {
    if name == "type" {
        "ty".into()
    } else if RUST_KEYWORDS.contains(&name) {
        format!("r#{name}")
    } else {
        name.into()
    }
}

/// The default name of a child node: its type in snake case, plural when
/// it repeats.
fn node_field_name(ty: &str, many: bool) -> String {
    let base = if ty == "Type" {
        "ty".to_owned()
    } else {
        snake(ty)
    };
    if !many {
        base
    } else if base.ends_with('s') || base.ends_with('x') {
        base + "es"
    } else if base == "ty" {
        "types".into()
    } else if let Some(stem) = base.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        stem.to_owned() + "ies"
    } else {
        base + "s"
    }
}

/// A child accessor of a struct node.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field {
    /// A token of one of `kinds`.
    Token { name: String, kinds: Vec<String> },
    /// A child node of type `ty`: all of them, or one. When several
    /// children share the type, the one after a token of `after` and before
    /// a token of `before`; when no token tells them apart, the `nth`.
    Node {
        name: String,
        ty: String,
        many: bool,
        nth: usize,
        after: Vec<String>,
        before: Vec<String>,
    },
}

impl Field {
    fn name(&self) -> &str {
        match self {
            Field::Token { name, .. } | Field::Node { name, .. } => name,
        }
    }
}

/// A node of the grammar as the generator renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AstDef {
    /// A node kind with child accessors.
    Struct {
        name: String,
        rule: String,
        fields: Vec<Field>,
    },
    /// A rule that is only a choice between nodes.
    Enum {
        name: String,
        rule: String,
        variants: Vec<String>,
    },
}

impl AstDef {
    fn name(&self) -> &str {
        match self {
            AstDef::Struct { name, .. } | AstDef::Enum { name, .. } => name,
        }
    }
}

fn lower(grammar: &Grammar, tokens: &Tokens) -> Result<Vec<AstDef>, String> {
    let casts = casts(grammar);
    let overlap = |a: &str, b: &str| {
        a == b
            || casts.get(a).is_some_and(|set| set.contains(b))
            || casts.get(b).is_some_and(|set| set.contains(a))
    };
    let mut defs = Vec::new();
    for node in grammar.iter() {
        let data = &grammar[node];
        let rule = show(grammar, &data.rule, false);
        if let Some(variants) = enum_variants(grammar, &data.rule) {
            defs.push(AstDef::Enum {
                name: data.name.clone(),
                rule,
                variants,
            });
            continue;
        }
        let mut fields = Vec::new();
        lower_rule(grammar, tokens, &data.rule, None, false, &mut fields)
            .map_err(|err| format!("{}: {err}", data.name))?;
        let fields =
            dedupe(place(fields, &overlap)).map_err(|err| format!("{}: {err}", data.name))?;
        defs.push(AstDef::Struct {
            name: data.name.clone(),
            rule,
            fields,
        });
    }
    if defs.iter().any(|def| def.name() == ERROR_NODE) {
        return Err(format!(
            "`{ERROR_NODE}` is reserved for the parser's error node"
        ));
    }
    if let Some(def) = defs
        .iter()
        .find(|def| tokens.all.iter().any(|token| token.name == def.name()))
    {
        return Err(format!(
            "node `{}` has the name of a token kind",
            def.name()
        ));
    }
    Ok(defs)
}

/// Every node each enum can be viewed as, through its variants and theirs:
/// a `TupleType` is a `Type`.
fn casts(grammar: &Grammar) -> HashMap<String, HashSet<String>> {
    let direct: HashMap<String, Vec<String>> = grammar
        .iter()
        .filter_map(|node| {
            let data = &grammar[node];
            Some((data.name.clone(), enum_variants(grammar, &data.rule)?))
        })
        .collect();
    let mut out = HashMap::new();
    for name in direct.keys() {
        let mut seen = HashSet::new();
        let mut stack = vec![name.clone()];
        while let Some(next) = stack.pop() {
            for variant in direct.get(&next).into_iter().flatten() {
                if seen.insert(variant.clone()) {
                    stack.push(variant.clone());
                }
            }
        }
        out.insert(name.clone(), seen);
    }
    out
}

/// The variants of a rule that is only a choice between nodes.
fn enum_variants(grammar: &Grammar, rule: &Rule) -> Option<Vec<String>> {
    let Rule::Alt(alternatives) = rule else {
        return None;
    };
    alternatives
        .iter()
        .map(|alt| match alt {
            Rule::Node(node) => Some(grammar[*node].name.clone()),
            _ => None,
        })
        .collect()
}

fn lower_rule(
    grammar: &Grammar,
    tokens: &Tokens,
    rule: &Rule,
    label: Option<&str>,
    many: bool,
    out: &mut Vec<Field>,
) -> Result<(), String> {
    match rule {
        Rule::Labeled { label, rule } => lower_rule(grammar, tokens, rule, Some(label), many, out),
        Rule::Node(node) => {
            let ty = grammar[*node].name.clone();
            let name = label.map_or_else(|| node_field_name(&ty, many), String::from);
            out.push(Field::Node {
                name: method(&name),
                ty,
                many,
                nth: 0,
                after: Vec::new(),
                before: Vec::new(),
            });
            Ok(())
        }
        Rule::Token(token) => {
            // Unlabeled tokens in a repetition are separators.
            if many && label.is_none() {
                return Ok(());
            }
            let kind = token_kind(grammar, tokens, *token)?;
            let name = label.map_or_else(|| format!("{}_token", kind.stem), String::from);
            out.push(Field::Token {
                name: method(&name),
                kinds: vec![kind.name.clone()],
            });
            Ok(())
        }
        Rule::Alt(alternatives) if let Some(label) = label => {
            let mut kinds = Vec::new();
            for alt in alternatives {
                let Rule::Token(token) = alt else {
                    return Err(format!(
                        "label `{label}` names a choice that is not only tokens; make the choice its own node"
                    ));
                };
                kinds.push(token_kind(grammar, tokens, *token)?.name.clone());
            }
            out.push(Field::Token {
                name: method(label),
                kinds,
            });
            Ok(())
        }
        Rule::Seq(rules) | Rule::Alt(rules) => {
            for rule in rules {
                lower_rule(grammar, tokens, rule, label, many, out)?;
            }
            Ok(())
        }
        Rule::Opt(rule) => lower_rule(grammar, tokens, rule, label, many, out),
        Rule::Rep(rule) => lower_rule(grammar, tokens, rule, label, true, out),
    }
}

fn token_kind<'a>(
    grammar: &Grammar,
    tokens: &'a Tokens,
    token: ungrammar::Token,
) -> Result<&'a TokenKind, String> {
    let spelling = &grammar[token].name;
    tokens
        .by_spelling(spelling)
        .ok_or_else(|| format!("unknown token '{spelling}'"))
}

/// Tells apart single children whose types overlap by the tokens between
/// them: in `start:Expr? '..' end:Expr?`, `start` is before `..` and `end`
/// after it, whichever of them is missing. Two types overlap when one can
/// be viewed as the other, as `params:TupleType '=>' ret:Type`.
fn place(mut fields: Vec<Field>, overlap: &dyn Fn(&str, &str) -> bool) -> Vec<Field> {
    let single = |field: &Field| match field {
        Field::Node {
            ty, many: false, ..
        } => Some(ty.clone()),
        _ => None,
    };
    let kinds = |field: &Field| match field {
        Field::Token { kinds, .. } => Some(kinds.clone()),
        Field::Node { .. } => None,
    };
    let types: Vec<Option<String>> = fields.iter().map(single).collect();
    for i in 0..fields.len() {
        let Some(ty) = &types[i] else { continue };
        let same: Vec<usize> = (0..fields.len())
            .filter(|&j| types[j].as_deref().is_some_and(|other| overlap(other, ty)))
            .collect();
        if same.len() < 2 {
            continue;
        }
        let k = same.iter().position(|&j| j == i).unwrap_or(0);
        // Each range takes in the node at its start, which carries no
        // token kind: only the tokens between the two nodes count.
        let after = if k > 0 {
            fields[same[k - 1]..i].iter().rev().find_map(kinds)
        } else {
            None
        };
        let before = same
            .get(k + 1)
            .and_then(|&next| fields[i..next].iter().find_map(kinds));
        if let Field::Node {
            after: a,
            before: b,
            ..
        } = &mut fields[i]
        {
            *a = after.unwrap_or_default();
            *b = before.unwrap_or_default();
        }
    }
    fields
}

/// Merges repeated accessors and numbers the children that share a type.
fn dedupe(fields: Vec<Field>) -> Result<Vec<Field>, String> {
    let mut out: Vec<Field> = Vec::new();
    for field in fields {
        match out.iter_mut().find(|f| f.name() == field.name()) {
            Some(Field::Token { kinds, .. }) => {
                let Field::Token { kinds: more, .. } = field else {
                    return Err(format!("`{}` is both a token and a node", field.name()));
                };
                for kind in more {
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
            }
            Some(Field::Node { many, .. }) => {
                let Field::Node { many: more, .. } = field else {
                    return Err(format!("`{}` is both a node and a token", field.name()));
                };
                *many |= more;
            }
            None => {
                let field = match field {
                    Field::Node {
                        name,
                        ty,
                        many: false,
                        after,
                        before,
                        ..
                    } => {
                        let nth = out
                            .iter()
                            .filter(
                                |f| matches!(f, Field::Node { ty: t, many: false, .. } if *t == ty),
                            )
                            .count();
                        Field::Node {
                            name,
                            ty,
                            many: false,
                            nth,
                            after,
                            before,
                        }
                    }
                    other => other,
                };
                out.push(field);
            }
        }
    }
    Ok(out)
}

/// The rule as `mora.ungram` would write it.
fn show(grammar: &Grammar, rule: &Rule, nested: bool) -> String {
    // A nested sequence or alternative brackets itself.
    let atom = |rule: &Rule| show(grammar, rule, true);
    match rule {
        Rule::Labeled { label, rule } => format!("{label}:{}", atom(rule)),
        Rule::Node(node) => grammar[*node].name.clone(),
        Rule::Token(token) => format!("'{}'", grammar[*token].name),
        Rule::Seq(rules) => {
            let parts: Vec<_> = rules.iter().map(atom).collect();
            let text = parts.join(" ");
            if nested { format!("({text})") } else { text }
        }
        Rule::Alt(rules) => {
            let text = rules
                .iter()
                .map(|r| show(grammar, r, false))
                .collect::<Vec<_>>()
                .join(" | ");
            if nested { format!("({text})") } else { text }
        }
        Rule::Opt(rule) => format!("{}?", atom(rule)),
        Rule::Rep(rule) => format!("{}*", atom(rule)),
    }
}

/// The generated `SyntaxKind`. Writing to a `String` cannot fail, so the
/// results of `write!` are ignored.
fn render_kinds(tokens: &Tokens, defs: &[AstDef]) -> String {
    let mut out = String::new();
    out.push_str("//! Generated by `cargo xtask codegen` from §3.4 of the spec and `mora.ungram`; do not edit.\n\n");
    out.push_str("/// The kind of a token or a node.\n");
    out.push_str("#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n#[repr(u16)]\npub enum SyntaxKind {\n");
    let mut all = Vec::new();
    for OtherToken { name, doc, .. } in OTHER_TOKENS {
        let _ = writeln!(out, "    /// {doc}\n    {name},");
        all.push((*name).to_owned());
    }
    for (name, text) in PUNCTUATION {
        let _ = writeln!(out, "    /// `{text}`\n    {name},");
        all.push((*name).to_owned());
    }
    for (name, text) in JOINED {
        let _ = writeln!(
            out,
            "    /// `{text}`, joined by the parser from adjacent tokens.\n    {name},"
        );
        all.push((*name).to_owned());
    }
    for word in &tokens.keywords {
        let name = keyword_kind(word);
        let _ = writeln!(out, "    /// The keyword `{word}`.\n    {name},");
        all.push(name);
    }
    for word in &tokens.contextual {
        let name = keyword_kind(word);
        let _ = writeln!(
            out,
            "    /// The contextual keyword `{word}`: an identifier the parser marks in position.\n    {name},"
        );
        all.push(name);
    }
    let _ = writeln!(
        out,
        "    /// Text the parser could not fit into the grammar.\n    {ERROR_NODE},"
    );
    all.push(ERROR_NODE.to_owned());
    for def in defs {
        if let AstDef::Struct { name, .. } = def {
            let _ = writeln!(out, "    /// A `{name}` node.\n    {name},");
            all.push(name.clone());
        }
    }
    out.push_str("}\n\nuse SyntaxKind::*;\n\nimpl SyntaxKind {\n");
    let list = |names: &mut dyn Iterator<Item = String>| names.collect::<Vec<_>>().join(", ");
    let _ = writeln!(
        out,
        "    /// Every kind, in order: `ALL[k as usize] == k`.\n    pub const ALL: &[SyntaxKind] = &[{}];",
        list(&mut all.iter().cloned())
    );
    let _ = writeln!(
        out,
        "    /// The punctuation the lexer produces.\n    pub const PUNCTUATION: &[SyntaxKind] = &[{}];",
        list(&mut PUNCTUATION.iter().map(|(n, _)| (*n).to_owned()))
    );
    let _ = writeln!(
        out,
        "    /// The punctuation the parser joins from adjacent tokens.\n    pub const JOINED: &[SyntaxKind] = &[{}];",
        list(&mut JOINED.iter().map(|(n, _)| (*n).to_owned()))
    );
    let _ = writeln!(
        out,
        "    /// The reserved keywords (§3.4).\n    pub const KEYWORDS: &[SyntaxKind] = &[{}];",
        list(&mut tokens.keywords.iter().map(|w| keyword_kind(w)))
    );
    let _ = writeln!(
        out,
        "    /// The contextual keywords (§3.4).\n    pub const CONTEXTUAL_KEYWORDS: &[SyntaxKind] = &[{}];",
        list(&mut tokens.contextual.iter().map(|w| keyword_kind(w)))
    );
    out.push_str(
        "\n    /// The kind with this raw number.\n    pub fn from_raw(raw: u16) -> Option<SyntaxKind> {\n        Self::ALL.get(usize::from(raw)).copied()\n    }\n",
    );
    out.push_str("\n    /// The fixed text of a punctuation or keyword kind.\n    pub fn text(self) -> Option<&'static str> {\n        Some(match self {\n");
    for (name, text) in PUNCTUATION.iter().chain(JOINED) {
        let _ = writeln!(out, "            {name} => {text:?},");
    }
    for word in tokens.keywords.iter().chain(&tokens.contextual) {
        let _ = writeln!(out, "            {} => {word:?},", keyword_kind(word));
    }
    out.push_str("            _ => return None,\n        })\n    }\n");
    for (fn_name, doc, words) in [
        (
            "keyword",
            "The reserved keyword spelled `text`, if any.",
            &tokens.keywords,
        ),
        (
            "contextual_keyword",
            "The contextual keyword spelled `text`, if any.",
            &tokens.contextual,
        ),
    ] {
        let _ = writeln!(
            out,
            "\n    /// {doc}\n    pub fn {fn_name}(text: &str) -> Option<SyntaxKind> {{\n        Some(match text {{"
        );
        for word in words {
            let _ = writeln!(out, "            {word:?} => {},", keyword_kind(word));
        }
        out.push_str("            _ => return None,\n        })\n    }\n");
    }
    out.push_str(
        "\n    pub fn is_keyword(self) -> bool {\n        Self::KEYWORDS.contains(&self)\n    }\n",
    );
    out.push_str("\n    pub fn is_contextual_keyword(self) -> bool {\n        Self::CONTEXTUAL_KEYWORDS.contains(&self)\n    }\n");
    let trivia: Vec<_> = OTHER_TOKENS
        .iter()
        .filter(|token| token.trivia)
        .map(|token| token.name)
        .collect();
    let _ = writeln!(
        out,
        "\n    /// Whether the parser skips tokens of this kind: whitespace, newlines,\n    /// comments and directives.\n    pub fn is_trivia(self) -> bool {{\n        matches!(self, {})\n    }}",
        trivia.join(" | ")
    );
    let _ = writeln!(
        out,
        "\n    /// Whether this is a node kind rather than a token kind.\n    pub fn is_node(self) -> bool {{\n        self >= {ERROR_NODE}\n    }}\n}}"
    );
    out
}

/// `SyntaxKind::A, SyntaxKind::B`: kind names as a list of paths.
fn kind_paths(kinds: &[String]) -> String {
    kinds
        .iter()
        .map(|kind| format!("SyntaxKind::{kind}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The generated AST views. Writing to a `String` cannot fail, so the
/// results of `write!` are ignored.
fn render_nodes(defs: &[AstDef]) -> String {
    let mut out = String::new();
    out.push_str("//! Generated by `cargo xtask codegen` from `mora.ungram`; do not edit.\n\n");
    out.push_str("use crate::ast::{AstChildren, AstNode, support};\nuse crate::{SyntaxKind, SyntaxNode, SyntaxToken};\n");
    for def in defs {
        match def {
            AstDef::Struct { name, rule, fields } => {
                let _ = writeln!(
                    out,
                    "\n/// ```text\n/// {name} = {rule}\n/// ```\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub struct {name}<'a> {{\n    pub(crate) syntax: SyntaxNode<'a>,\n}}\n"
                );
                if !fields.is_empty() {
                    let _ = writeln!(out, "impl<'a> {name}<'a> {{");
                    for field in fields {
                        match field {
                            Field::Token { name, kinds } => {
                                let _ = writeln!(
                                    out,
                                    "    pub fn {name}(&self) -> Option<SyntaxToken<'a>> {{\n        support::token(&self.syntax, &[{}])\n    }}",
                                    kind_paths(kinds)
                                );
                            }
                            Field::Node {
                                name,
                                ty,
                                many: true,
                                ..
                            } => {
                                let _ = writeln!(
                                    out,
                                    "    pub fn {name}(&self) -> AstChildren<'a, {ty}<'a>> {{\n        support::children(&self.syntax)\n    }}"
                                );
                            }
                            Field::Node {
                                name,
                                ty,
                                after,
                                before,
                                ..
                            } if !after.is_empty() || !before.is_empty() => {
                                let _ = writeln!(
                                    out,
                                    "    pub fn {name}(&self) -> Option<{ty}<'a>> {{\n        support::child_between(&self.syntax, &[{}], &[{}])\n    }}",
                                    kind_paths(after),
                                    kind_paths(before)
                                );
                            }
                            Field::Node {
                                name, ty, nth: 0, ..
                            } => {
                                let _ = writeln!(
                                    out,
                                    "    pub fn {name}(&self) -> Option<{ty}<'a>> {{\n        support::child(&self.syntax)\n    }}"
                                );
                            }
                            Field::Node { name, ty, nth, .. } => {
                                let _ = writeln!(
                                    out,
                                    "    pub fn {name}(&self) -> Option<{ty}<'a>> {{\n        support::nth_child(&self.syntax, {nth})\n    }}"
                                );
                            }
                        }
                    }
                    out.push_str("}\n");
                }
                let _ = writeln!(
                    out,
                    "\nimpl<'a> AstNode<'a> for {name}<'a> {{\n    fn can_cast(kind: SyntaxKind) -> bool {{\n        kind == SyntaxKind::{name}\n    }}\n\n    fn cast(node: SyntaxNode<'a>) -> Option<Self> {{\n        Self::can_cast(node.kind()).then_some(Self {{ syntax: node }})\n    }}\n\n    fn syntax(&self) -> &SyntaxNode<'a> {{\n        &self.syntax\n    }}\n}}"
                );
            }
            AstDef::Enum {
                name,
                rule,
                variants,
            } => {
                let _ = writeln!(
                    out,
                    "\n/// ```text\n/// {name} = {rule}\n/// ```\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub enum {name}<'a> {{"
                );
                for variant in variants {
                    let _ = writeln!(out, "    {variant}({variant}<'a>),");
                }
                let can_cast: Vec<_> = variants
                    .iter()
                    .map(|v| format!("{v}::can_cast(kind)"))
                    .collect();
                let _ = writeln!(
                    out,
                    "}}\n\nimpl<'a> AstNode<'a> for {name}<'a> {{\n    fn can_cast(kind: SyntaxKind) -> bool {{\n        {}\n    }}\n\n    fn cast(node: SyntaxNode<'a>) -> Option<Self> {{\n        let kind = node.kind();",
                    can_cast.join(" || ")
                );
                for variant in variants {
                    let _ = writeln!(
                        out,
                        "        if {variant}::can_cast(kind) {{\n            return {variant}::cast(node).map(Self::{variant});\n        }}"
                    );
                }
                out.push_str("        None\n    }\n\n    fn syntax(&self) -> &SyntaxNode<'a> {\n        match self {\n");
                for variant in variants {
                    let _ = writeln!(out, "            Self::{variant}(it) => it.syntax(),");
                }
                out.push_str("        }\n    }\n}\n");
            }
        }
    }
    out
}

/// Formats Rust source with the repository's `rustfmt` settings.
fn rustfmt(root: &Path, source: &str) -> Result<String, String> {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("cannot run rustfmt: {err}"))?;
    // Feed the input from another thread while this one reads the output,
    // so neither side can block on a full pipe.
    let mut stdin = child.stdin.take().ok_or("rustfmt has no stdin")?;
    let input = source.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|err| format!("rustfmt failed: {err}"))?;
    let written = writer
        .join()
        .map_err(|_| "the thread writing to rustfmt panicked".to_owned())?;
    // rustfmt's own error explains a pipe it closed early, so it goes first.
    if !output.status.success() {
        return Err(format!(
            "rustfmt rejected the generated code:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    written.map_err(|err| format!("cannot write to rustfmt: {err}"))?;
    String::from_utf8(output.stdout).map_err(|err| format!("rustfmt wrote invalid UTF-8: {err}"))
}

/// The generated files and their contents.
pub fn outputs(root: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path)).map_err(|err| format!("{path}: {err}"))
    };
    let tokens = Tokens::from_spec(&read(SPEC)?)?;
    let grammar: Grammar = read(GRAMMAR)?
        .parse()
        .map_err(|err| format!("{GRAMMAR}: {err}"))?;
    let defs = lower(&grammar, &tokens)?;
    Ok(vec![
        (
            root.join(KINDS),
            rustfmt(root, &render_kinds(&tokens, &defs))?,
        ),
        (root.join(NODES), rustfmt(root, &render_nodes(&defs))?),
    ])
}

/// The generated files whose contents on disk differ from `outputs`.
pub fn stale(outputs: &[(PathBuf, String)]) -> Vec<PathBuf> {
    outputs
        .iter()
        .filter(|(path, text)| std::fs::read_to_string(path).ok().as_deref() != Some(text))
        .map(|(path, _)| path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_TEXT: &str = "Reserved keywords (never identifiers):\n\n```text\nclass if\nstruct\n```\n\nContextual keywords (keywords only in the listed position): `get`, `set` (accessors); `when`.\n\nNext paragraph `ignored`.\n";

    #[test]
    fn keywords_come_from_the_spec() {
        let tokens = Tokens::from_spec(SPEC_TEXT).unwrap();
        assert_eq!(tokens.keywords, ["class", "if", "struct"]);
        assert_eq!(tokens.contextual, ["get", "set", "when"]);
        assert_eq!(tokens.by_spelling("struct").unwrap().name, "StructKw");
        assert_eq!(tokens.by_spelling("{").unwrap().stem, "l_brace");
        assert_eq!(tokens.by_spelling(">>=").unwrap().name, "ShrEq");
    }

    #[test]
    fn the_real_spec_has_both_keyword_lists() {
        let spec = std::fs::read_to_string(crate::paths::root().join(SPEC)).unwrap();
        let tokens = Tokens::from_spec(&spec).unwrap();
        assert_eq!(tokens.keywords.len(), 68);
        assert!(tokens.contextual.contains(&"adds".to_owned()));
        assert!(tokens.contextual.contains(&"match".to_owned()));
    }

    fn lower_text(text: &str) -> Result<Vec<AstDef>, String> {
        let grammar: Grammar = text.parse().map_err(|err| format!("{err}"))?;
        lower(&grammar, &Tokens::from_spec(SPEC_TEXT).unwrap())
    }

    #[test]
    fn rules_become_structs_and_enums() {
        let defs = lower_text(
            "Item = StructDecl | ClassDecl\n\
             StructDecl = 'struct' Name '{' Field* '}'\n\
             ClassDecl = 'class' Name ('<' Name (',' Name)* '>')?\n\
             Field = Name\n\
             Name = 'ident'\n\
             BinExpr = lhs:Name op:('+' | '-') rhs:Name\n",
        )
        .unwrap();
        assert_eq!(
            defs[0],
            AstDef::Enum {
                name: "Item".into(),
                rule: "StructDecl | ClassDecl".into(),
                variants: vec!["StructDecl".into(), "ClassDecl".into()],
            }
        );
        let AstDef::Struct { fields, .. } = &defs[1] else {
            panic!("{:?}", defs[1]);
        };
        let names: Vec<_> = fields.iter().map(Field::name).collect();
        assert_eq!(
            names,
            [
                "struct_token",
                "name",
                "l_brace_token",
                "fields",
                "r_brace_token"
            ]
        );
        let AstDef::Struct { fields, rule, .. } = &defs[2] else {
            panic!();
        };
        assert_eq!(rule, "'class' Name ('<' Name (',' Name)* '>')?");
        // An unlabeled `Name` again merges into `name`; the repeated one is `names`.
        let names: Vec<_> = fields.iter().map(Field::name).collect();
        assert_eq!(
            names,
            ["class_token", "name", "lt_token", "names", "gt_token"]
        );
        let AstDef::Struct { fields, .. } = &defs[5] else {
            panic!();
        };
        assert_eq!(
            fields,
            &[
                Field::Node {
                    name: "lhs".into(),
                    ty: "Name".into(),
                    many: false,
                    nth: 0,
                    after: vec![],
                    before: vec!["Plus".into(), "Minus".into()],
                },
                Field::Token {
                    name: "op".into(),
                    kinds: vec!["Plus".into(), "Minus".into()]
                },
                Field::Node {
                    name: "rhs".into(),
                    ty: "Name".into(),
                    many: false,
                    nth: 1,
                    after: vec!["Plus".into(), "Minus".into()],
                    before: vec![],
                },
            ]
        );
    }

    #[test]
    fn grammar_mistakes_are_reported() {
        assert!(
            lower_text("A = 'nope'\n")
                .unwrap_err()
                .contains("unknown token 'nope'")
        );
        assert!(
            lower_text("A = x:(B | C)\nB = 'if'\nC = 'class'\n")
                .unwrap_err()
                .contains("make the choice its own node")
        );
        assert!(
            lower_text("Error = 'if'\n")
                .unwrap_err()
                .contains("reserved")
        );
        assert!(
            lower_text("Ident = 'if'\n")
                .unwrap_err()
                .contains("name of a token kind")
        );
    }

    #[test]
    fn children_whose_types_overlap_are_told_apart_by_tokens() {
        let defs = lower_text(
            "Type = TupleType | PathType\nTupleType = '(' ')'\nPathType = 'ident'\n\
             FnType = params:TupleType '=>' ret:Type\n",
        )
        .unwrap();
        let fields = defs
            .iter()
            .find_map(|def| match def {
                AstDef::Struct { name, fields, .. } if name == "FnType" => Some(fields.clone()),
                _ => None,
            })
            .unwrap();
        let placed: Vec<_> = fields
            .iter()
            .filter_map(|field| match field {
                Field::Node {
                    name,
                    after,
                    before,
                    ..
                } => Some((name.as_str(), after.clone(), before.clone())),
                Field::Token { .. } => None,
            })
            .collect();
        assert_eq!(
            placed,
            [
                ("params", vec![], vec!["FatArrow".to_owned()]),
                ("ret", vec!["FatArrow".to_owned()], vec![]),
            ]
        );
    }

    #[test]
    fn children_of_one_type_without_a_token_between_are_counted() {
        let defs = lower_text("Name = 'ident'\nPair = first:Name second:Name\n").unwrap();
        let rendered = render_nodes(&defs);
        assert!(
            rendered.contains("support::child(&self.syntax)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("support::nth_child(&self.syntax, 1)"),
            "{rendered}"
        );
    }

    #[test]
    fn repeated_accessors_merge() {
        let token = |kind: &str| Field::Token {
            name: "op".into(),
            kinds: vec![kind.into()],
        };
        let node = |many| Field::Node {
            name: "x".into(),
            ty: "Name".into(),
            many,
            nth: 0,
            after: vec![],
            before: vec![],
        };
        let fields = vec![
            token("Plus"),
            token("Minus"),
            token("Plus"),
            node(false),
            node(true),
        ];
        assert_eq!(
            dedupe(fields).unwrap(),
            [
                Field::Token {
                    name: "op".into(),
                    kinds: vec!["Plus".into(), "Minus".into()],
                },
                node(true),
            ]
        );
    }

    #[test]
    fn names() {
        assert_eq!(snake("QuestionQuestionEq"), "question_question_eq");
        assert_eq!(method("type"), "ty");
        assert_eq!(method("else"), "r#else");
        assert_eq!(node_field_name("Type", true), "types");
        assert_eq!(node_field_name("Modifier", true), "modifiers");
        assert_eq!(node_field_name("Class", true), "classes");
        assert_eq!(node_field_name("Capability", true), "capabilities");
        assert_eq!(node_field_name("Key", true), "keys");
    }

    #[test]
    fn generated_files_are_up_to_date() {
        let outputs = outputs(&crate::paths::root()).unwrap();
        assert_eq!(
            stale(&outputs),
            Vec::<PathBuf>::new(),
            "run `cargo xtask codegen`"
        );
        let changed: Vec<_> = outputs
            .iter()
            .map(|(path, text)| (path.clone(), format!("{text}// edited\n")))
            .collect();
        assert_eq!(stale(&changed).len(), 2);
    }
}
