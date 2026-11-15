//! Tests of constant evaluation, each from source text through the front
//! end.

use mora_hir::test_support::{TestDb, app, codes, file_named, item_named};
use mora_hir::{
    BodyOwner, Expr, ExprId, ItemId, body, body_resolution, body_source_map, diagnostics,
};
use mora_span::{Project, SourceFile, TextRange, TextSize};

use super::scan::const_scan;
use super::{ConstKey, const_read, constants, defaults};

/// A file of an app, read by the front end.
struct Source {
    db: TestDb,
    project: Project,
    file: SourceFile,
}

/// `text` as `src/Program.mr`, alone in an app, which the front end checks
/// without an error.
fn checked(text: &str) -> Source {
    let source = unchecked(text);
    assert_eq!(
        codes(diagnostics(&source.db, source.project)),
        Vec::<String>::new(),
        "{text}"
    );
    source
}

/// `text` as `src/Program.mr`, alone in an app, whatever the front end
/// reports about it.
fn unchecked(text: &str) -> Source {
    let db = TestDb::new();
    let project = app(&db, &[("src/Program.mr", text)]);
    let file = file_named(&db, project, "src/Program.mr");
    Source { db, project, file }
}

impl Source {
    /// The item `name` declared in the file.
    fn item(&self, name: &str) -> ItemId {
        item_named(&self.db, self.project, self.file, name)
    }

    /// The constant `name` declared at namespace level.
    fn constant(&self, name: &str) -> ConstKey {
        ConstKey::Constant {
            file: self.file,
            owner: BodyOwner::Item(self.item(name).item),
        }
    }

    /// The value member `name` of the type `ty`.
    fn value(&self, ty: &str, name: &str) -> BodyOwner {
        let item = self.item(ty);
        let tree = mora_hir::item_tree(&self.db, self.file);
        let index = tree
            .item(item.item)
            .values
            .iter()
            .position(|value| value.name == name)
            .expect("the member");
        BodyOwner::Value(item.item, u32::try_from(index).expect("a small index"))
    }

    /// The constant `name` that the type `ty` declares.
    fn member_constant(&self, ty: &str, name: &str) -> ConstKey {
        ConstKey::Constant {
            file: self.file,
            owner: self.value(ty, name),
        }
    }

    /// The range of the first `needle` inside the only occurrence of
    /// `context` in the text.
    fn range_in(&self, context: &str, needle: &str) -> TextRange {
        let text = self.file.text(&self.db);
        let mut found = text.match_indices(context);
        let (start, _) = found.next().expect("the context");
        assert_eq!(found.next(), None, "`{context}` occurs more than once");
        let offset = context.find(needle).expect("the needle in its context");
        let start = u32::try_from(start + offset).expect("a short text");
        let len = u32::try_from(needle.len()).expect("a short needle");
        TextRange::at(TextSize::new(start), TextSize::new(len))
    }

    /// The text at `range`.
    fn text(&self, range: TextRange) -> &str {
        &self.file.text(&self.db)[std::ops::Range::<usize>::from(range)]
    }

    /// The expression of the body of `owner` written at `range`, that is
    /// not a part of a longer expression there.
    fn expr_at(&self, owner: BodyOwner, range: TextRange) -> ExprId {
        let hir = body(&self.db, self.file, owner);
        let map = body_source_map(&self.db, self.file, owner);
        hir.exprs
            .iter()
            .map(|(id, _)| id)
            .rfind(|&id| map.expr(id) == range)
            .expect("an expression at the range")
    }

    /// What `const_read` gives for the expression of the body of `owner`
    /// written as the only `needle` inside `context`.
    fn read(&self, owner: BodyOwner, context: &str, needle: &str) -> Option<(Project, ConstKey)> {
        let expr = self.expr_at(owner, self.range_in(context, needle));
        let resolution = body_resolution(&self.db, self.project, self.file, owner);
        const_read(&self.db, resolution, expr)
    }

    /// The reads of `key`, each as the key and the text it is read at.
    fn reads(&self, key: ConstKey) -> Vec<(ConstKey, &str)> {
        const_scan(&self.db, self.project, key)
            .reads
            .iter()
            .map(|&(project, read, range)| {
                assert_eq!(project, self.project);
                (read, self.text(range))
            })
            .collect()
    }

    /// The first unsupported operation of `key`, as its text and what it
    /// is.
    fn unsupported(&self, key: ConstKey) -> Option<(&str, &'static str)> {
        let scan = const_scan(&self.db, self.project, key);
        scan.unsupported
            .map(|(range, what)| (self.text(range), what))
    }

    /// The text of each expression the scan of `key` visited, in order.
    fn visited(&self, key: ConstKey) -> Vec<&str> {
        let map = body_source_map(&self.db, key.file(), key.owner(&self.db));
        const_scan(&self.db, self.project, key)
            .exprs
            .iter()
            .map(|&expr| self.text(map.expr(expr)))
            .collect()
    }

    /// The only default value of the file that needs evaluation.
    fn only_default(&self) -> ConstKey {
        let found = defaults(&self.db, self.project, self.file);
        assert_eq!(found.len(), 1, "{found:?}");
        found[0]
    }
}

#[test]
fn a_scan_finds_the_reads_in_text_order() {
    let source = checked(
        "const A: int = 1;\nconst B: int = 2;\nstruct P {\n    public X: int = 1;\n    public Y: int;\n    public const K: int = 3;\n}\nconst C: P = new P { Y = A + B + P.K };\n",
    );
    let p = source.item("P");
    assert_eq!(
        source.reads(source.constant("C")),
        vec![
            (source.constant("A"), "A"),
            (source.constant("B"), "B"),
            (source.member_constant("P", "K"), "P.K"),
            (
                ConstKey::Initializer { item: p, field: 0 },
                "new P { Y = A + B + P.K }"
            ),
        ]
    );
    assert_eq!(
        source.range_in("Y = A + B", "A"),
        const_scan(&source.db, source.project, source.constant("C")).reads[0].2
    );
    // A read is not descended into: `P` of `P.K` is not visited.
    assert_eq!(
        source.visited(source.constant("C")),
        vec![
            "new P { Y = A + B + P.K }",
            "A + B + P.K",
            "A + B",
            "A",
            "B",
            "P.K"
        ]
    );
    assert_eq!(source.unsupported(source.constant("C")), None);
}

#[test]
fn a_scan_reads_the_initializers_of_every_field_left_out_in_field_order() {
    let source = checked(
        "struct P {\n    public X: int = 1;\n    public static S: int = 4;\n    public const K: int = 3;\n    public Y: int = 2;\n    public Z: int = 3;\n}\nconst C: P = new P { Y = 5 };\n",
    );
    let p = source.item("P");
    assert_eq!(
        source.reads(source.constant("C")),
        vec![
            (
                ConstKey::Initializer { item: p, field: 0 },
                "new P { Y = 5 }"
            ),
            (
                ConstKey::Initializer { item: p, field: 2 },
                "new P { Y = 5 }"
            ),
        ]
    );
}

#[test]
fn a_scan_descends_into_operations_and_values_but_not_into_types() {
    let source = checked(
        "const A: int = 1;\nconst B: int = 2;\nstruct P {\n    public Y: int;\n    public static S: int = 4;\n}\nstruct R(V: int);\nconst T: (int, int) = (A, (long)B > 0 ? -A : B);\nconst M: int = (new P { Y = A }).Y;\nconst N: R = new R(B);\nconst S2: int = P.S + 1;\n",
    );
    let (a, b) = (source.constant("A"), source.constant("B"));
    assert_eq!(
        source.reads(source.constant("T")),
        vec![(a, "A"), (b, "B"), (a, "A"), (b, "B")]
    );
    assert_eq!(source.reads(source.constant("M")), vec![(a, "A")]);
    assert_eq!(source.reads(source.constant("N")), vec![(b, "B")]);
    // `P.S` names a static, which is not read, and `P` is not visited.
    assert_eq!(source.reads(source.constant("S2")), vec![]);
    assert_eq!(
        source.visited(source.constant("S2")),
        vec!["P.S + 1", "P.S", "1"]
    );
}

#[test]
fn a_scan_stops_at_the_first_unsupported_operation() {
    let source = checked("Zero(): int => 0;\nconst A: int = 1 + Zero() + Zero();\n");
    let a = source.constant("A");
    let scan = const_scan(&source.db, source.project, a);
    assert_eq!(
        scan.unsupported,
        Some((source.range_in("1 + Zero()", "Zero()"), "a call"))
    );
    // Nothing after the call is visited.
    assert_eq!(
        source.visited(a),
        vec!["1 + Zero() + Zero()", "1 + Zero()", "1", "Zero()"]
    );

    let source = checked("const S: string = $\"{1}\";\n");
    assert_eq!(
        source.unsupported(source.constant("S")),
        Some(("$\"{1}\"", "an interpolated string"))
    );

    let source = unchecked("F() {\n    var n = 1;\n    G(x: int = n++) {}\n    G();\n}\n");
    assert_eq!(
        source.unsupported(source.only_default()),
        Some(("n++", "`++`"))
    );

    let source = unchecked("F() {\n    var n = 1;\n    G(x: int = n) {}\n    G();\n}\n");
    assert_eq!(
        source.unsupported(source.only_default()),
        Some(("n", "a local variable"))
    );
}

#[test]
fn a_scan_names_each_unsupported_operation() {
    let cases = [
        (
            "F() {\n    var n = 1;\n    G(x: int = n--) {}\n    G();\n}\n",
            "n--",
            "`--`",
        ),
        (
            "F() {\n    var n = 1;\n    G(x: int = ++n) {}\n    G();\n}\n",
            "++n",
            "`++`",
        ),
        (
            "F() {\n    var n = 1;\n    G(x: int = --n) {}\n    G();\n}\n",
            "--n",
            "`--`",
        ),
        (
            "F() {\n    var n = 1;\n    G(x: int = n = 2) {}\n    G();\n}\n",
            "n = 2",
            "an assignment",
        ),
        (
            "F() {\n    G(x: () -> int = () => 1) {}\n    G();\n}\n",
            "() => 1",
            "a lambda",
        ),
        (
            "struct P {\n    public X: int;\n    M(x: int = this.X) {}\n}\n",
            "this",
            "`this`",
        ),
        (
            "struct P {\n    public X: int;\n    M(x: int = X) {}\n}\n",
            "X",
            "`this`",
        ),
        (
            "struct P {\n    public X: int => 1;\n    M(x: int = X) {}\n}\n",
            "X",
            "`this`",
        ),
        (
            "const A: int = Std.Math.Max(1, 2);\n",
            "Std.Math.Max(1, 2)",
            "a call",
        ),
    ];
    for (text, at, what) in cases {
        let source = unchecked(text);
        let key = match defaults(&source.db, source.project, source.file).first() {
            Some(&key) => key,
            None => source.constant("A"),
        };
        assert_eq!(source.unsupported(key), Some((at, what)), "{text}");
    }
}

#[test]
fn a_scan_flags_a_field_initializer_that_reads_another_field() {
    let source = unchecked("struct P {\n    public X: int = 1;\n    public Y: int = X;\n}\n");
    let y = ConstKey::Initializer {
        item: source.item("P"),
        field: 1,
    };
    assert_eq!(source.unsupported(y), Some(("X", "`this`")));
}

#[test]
fn a_scan_leaves_the_operand_of_nameof_alone() {
    let source = unchecked(
        "struct P {\n    public static S: int = 1;\n    public X: int;\n    M(x: int = S) {}\n}\nF() {\n    var n = 1;\n    G(x: string = nameof(n)) {}\n    G();\n}\n",
    );
    let found = defaults(&source.db, source.project, source.file);
    // A static named by its bare name is not `this`.
    assert_eq!(source.unsupported(found[0]), None);
    assert_eq!(source.visited(found[0]), vec!["S"]);
    assert_eq!(source.unsupported(found[1]), None);
    assert_eq!(source.visited(found[1]), vec!["nameof(n)"]);
}

#[test]
fn const_read_names_constants_by_name_and_through_their_type() {
    let source = checked(
        "const A: int = 1;\nstatic S: int = 1;\nstruct P {\n    public const K: int = 1;\n    public X: int;\n    Get(): int => K;\n    GetX(): int => X;\n}\nF(): int => P.K;\nH(): int => A;\nI(): int => S;\n",
    );
    let get = BodyOwner::Item(source.item("Get").item);
    let k = Some((source.project, source.member_constant("P", "K")));
    assert_eq!(source.read(get, "=> K", "K"), k);
    let f = BodyOwner::Item(source.item("F").item);
    assert_eq!(source.read(f, "=> P.K", "P.K"), k);
    let h = BodyOwner::Item(source.item("H").item);
    assert_eq!(
        source.read(h, "=> A", "A"),
        Some((source.project, source.constant("A")))
    );
    let i = BodyOwner::Item(source.item("I").item);
    assert_eq!(source.read(i, "=> S", "S"), None);
    let get_x = BodyOwner::Item(source.item("GetX").item);
    assert_eq!(source.read(get_x, "=> X", "X"), None);
    // A function is not a constant either.
    let get_call = source.read(f, "=> P.K", "P");
    assert_eq!(get_call, None);
}

#[test]
fn constants_lists_the_constants_of_a_file_in_order() {
    let source = checked(
        "const A: int = 1;\nstatic S: int = 1;\nstruct P {\n    public const K: int = 1;\n    public X: int = 2;\n}\nconst B: int = 2;\nF() {}\n",
    );
    assert_eq!(
        constants(&source.db, source.file),
        vec![
            source.constant("A"),
            source.member_constant("P", "K"),
            source.constant("B"),
        ]
    );
}

#[test]
fn defaults_lists_only_values_that_need_evaluation() {
    let source = checked(
        "const A: int = 1;\nO(a: int = 1, b: int = 1 + 2, c: int = A) {\n    L(d: bool = true, e: int = A) {}\n    L();\n}\nQ(f: int = A) {}\n",
    );
    let o = BodyOwner::Item(source.item("O").item);
    let q = BodyOwner::Item(source.item("Q").item);
    let expr = |owner, context| source.expr_at(owner, source.range_in(context, "A"));
    assert_eq!(
        defaults(&source.db, source.project, source.file),
        vec![
            ConstKey::Default {
                file: source.file,
                owner: o,
                default: expr(o, "c: int = A"),
            },
            ConstKey::Default {
                file: source.file,
                owner: o,
                default: expr(o, "e: int = A"),
            },
            ConstKey::Default {
                file: source.file,
                owner: q,
                default: expr(q, "f: int = A"),
            },
        ]
    );
    // A default's scan starts at the default.
    assert_eq!(
        source.reads(defaults(&source.db, source.project, source.file)[0]),
        vec![(source.constant("A"), "A")]
    );
}

#[test]
fn defaults_lists_nested_local_functions_in_text_order() {
    let source = checked(
        "const A: int = 1;\nO() {\n    L(e: int = A) {\n        M(d: int = A) {}\n        M();\n    }\n    L();\n}\n",
    );
    let names: Vec<String> = defaults(&source.db, source.project, source.file)
        .into_iter()
        .map(|key| key.name(&source.db, source.project))
        .collect();
    assert_eq!(
        names,
        vec!["the default value of e in O", "the default value of d in O"]
    );
}

#[test]
fn defaults_skips_those_of_lambdas() {
    let source = unchecked("const A: int = 1;\nF() {\n    let f = (x: int = A) => x;\n}\n");
    assert_eq!(defaults(&source.db, source.project, source.file), vec![]);
}

#[test]
fn key_names_drop_the_root_namespace() {
    let source = checked(
        "const A: int = 1;\nstruct P {\n    public const K: int = 3;\n    public Y: int;\n    public X: int = 1;\n}\nO(c: int = A) {}\n",
    );
    let name = |key: ConstKey| key.name(&source.db, source.project);
    assert_eq!(name(source.constant("A")), "A");
    assert_eq!(name(source.member_constant("P", "K")), "P.K");
    let p = source.item("P");
    assert_eq!(name(ConstKey::Initializer { item: p, field: 1 }), "P.X");
    assert_eq!(name(source.only_default()), "the default value of c in O");
}

#[test]
fn keys_know_their_file_owner_and_root() {
    let source = checked(
        "struct P {\n    public const K: int = 3;\n    public Y: int;\n    public X: int = 1;\n}\nO(c: int = P.K) {}\n",
    );
    let db = &source.db;
    let p = source.item("P");
    let initializer = ConstKey::Initializer { item: p, field: 1 };
    assert_eq!(initializer.file(), source.file);
    assert_eq!(initializer.owner(db), source.value("P", "X"));
    let root = initializer.root(db).expect("a root");
    let map = body_source_map(db, source.file, initializer.owner(db));
    assert_eq!(source.text(map.expr(root)), "1");
    assert!(matches!(
        body(db, source.file, initializer.owner(db)).exprs[root],
        Expr::Literal(_)
    ));

    let default = source.only_default();
    assert_eq!(default.file(), source.file);
    assert_eq!(default.owner(db), BodyOwner::Item(source.item("O").item));
    let map = body_source_map(db, source.file, default.owner(db));
    assert_eq!(
        source.text(map.expr(default.root(db).expect("a root"))),
        "P.K"
    );

    let k = source.member_constant("P", "K");
    assert_eq!(k.file(), source.file);
    assert_eq!(k.owner(db), source.value("P", "K"));
    // A field without an initializer has no root, and its scan is empty.
    let y = ConstKey::Initializer { item: p, field: 0 };
    assert_eq!(y.root(db), None);
    assert_eq!(source.visited(y), Vec::<&str>::new());
}

#[test]
fn keys_sort_constants_then_initializers_then_defaults_then_by_place() {
    let source = checked(
        "O(c: int = P.K, d: int = P.K) {}\nstruct P {\n    public X: int = 1;\n    public const K: int = 3;\n}\nconst A: int = 1;\n",
    );
    let db = &source.db;
    let p = source.item("P");
    let found = defaults(db, source.project, source.file);
    let (c, d) = (found[0], found[1]);
    let x = ConstKey::Initializer { item: p, field: 0 };
    let k = source.member_constant("P", "K");
    let a = source.constant("A");
    let mut keys = vec![d, x, a, c, k];
    keys.sort_by_key(|key| key.sort_key(db));
    assert_eq!(keys, vec![k, a, x, c, d]);
    let (kind, path, _) = x.sort_key(db);
    assert_eq!((kind, path.as_str()), (1, "src/Program.mr"));
}
