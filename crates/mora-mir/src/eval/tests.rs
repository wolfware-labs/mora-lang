//! Tests of constant evaluation, each from source text through the front
//! end.

use mora_hir::test_support::{TestDb, app, codes, file_named, item_named};
use mora_hir::{
    BodyOwner, Expr, ExprId, ItemId, Primitive, Ty, body, body_resolution, body_source_map,
    diagnostics,
};
use mora_span::{Dependency, FileId, Project, ProjectKind, SourceFile, Span, TextRange, TextSize};

use super::arith::{ArithError, MAX_STRING_BYTES};
use super::graph::{ConstGraph, const_graph};
use super::interp::{Failure, Stop, run};
use super::scan::const_scan;
use super::{
    ConstError, ConstKey, ConstValue, ErrorKind, Evaluated, const_read, const_value, constants,
    defaults,
};
use crate::lower::consts::default_mir;
use crate::{
    AggregateKind, BinOp, Block, BlockId, Body, BorrowMode, Callee, CastKind, Const, InterpPiece,
    LocalDecl, LocalId, LocalTy, Operand, Place, Projection, Rvalue, Statement, StatementKind,
    Terminator, TerminatorKind, UnOp,
};

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

    /// The read graph of the project.
    fn graph(&self) -> &ConstGraph {
        const_graph(&self.db, self.project)
    }

    /// The names of `keys`, in order.
    fn names(&self, keys: &[ConstKey]) -> Vec<String> {
        keys.iter()
            .map(|key| key.name(&self.db, self.project))
            .collect()
    }

    /// The names of the members of the cycle that holds `key`, and the
    /// text of its first member's read of the second.
    fn cycle(&self, key: ConstKey) -> Option<(Vec<String>, &str)> {
        let cycle = self.graph().cycle_of(key)?;
        Some((self.names(&cycle.members), self.text(cycle.read)))
    }

    /// The only default value of the file that needs evaluation.
    fn only_default(&self) -> ConstKey {
        let found = defaults(&self.db, self.project, self.file);
        assert_eq!(found.len(), 1, "{found:?}");
        found[0]
    }

    /// What a run of the initializer of the namespace-level constant
    /// `name` gives, with field initializers from `initializer`.
    fn run_with(
        &self,
        name: &str,
        initializer: &mut dyn FnMut(ItemId, u32) -> Option<ConstValue>,
    ) -> Result<ConstValue, Stop> {
        let owner = BodyOwner::Item(self.item(name).item);
        let mir = crate::mir(&self.db, self.project, self.file, owner);
        run(&self.db, &mir.bodies[0], initializer)
    }

    /// What a run of the initializer of the namespace-level constant
    /// `name` gives, when it calls no field initializer.
    fn run(&self, name: &str) -> Result<ConstValue, Stop> {
        self.run_with(name, &mut |item, field| {
            panic!("no initializer is called: {item:?} {field}")
        })
    }

    /// The failure that stops the run of the constant `name`.
    fn failure(&self, name: &str) -> Failure {
        match self.run(name) {
            Err(Stop::Failed(failure)) => failure,
            other => panic!("{name} runs to {other:?}"),
        }
    }

    /// The range of the only occurrence of `text` in the file.
    fn range_of(&self, text: &str) -> TextRange {
        self.range_in(text, text)
    }
}

/// A scalar integer of type `ty`.
fn int(value: i128, ty: Primitive) -> ConstValue {
    ConstValue::Scalar(Const::Int(value, ty))
}

/// A scalar `bool`.
fn boolean(value: bool) -> ConstValue {
    ConstValue::Scalar(Const::Bool(value))
}

/// A span that points nowhere in particular.
fn nowhere() -> Span {
    Span::new(FileId::new(900), TextRange::empty(TextSize::new(0)))
}

/// A body named `Hand` with `locals` locals of type `int`, `_0` among
/// them, and `blocks`.
fn hand_built(locals: usize, blocks: Vec<Block>) -> Body {
    let decl = LocalDecl {
        ty: LocalTy::Value(Ty::Primitive(Primitive::Int)),
        mutable: true,
        source: None,
    };
    Body {
        name: "Hand".to_owned(),
        params: 0,
        locals: vec![decl; locals],
        blocks,
        span: nowhere(),
    }
}

/// A block of `statements` that ends with `terminator`.
fn block(statements: Vec<StatementKind>, terminator: TerminatorKind) -> Block {
    Block {
        statements: statements
            .into_iter()
            .map(|kind| Statement {
                kind,
                span: nowhere(),
            })
            .collect(),
        terminator: Terminator {
            kind: terminator,
            span: nowhere(),
        },
    }
}

/// `place = rvalue`.
fn assign(place: Place, rvalue: Rvalue) -> StatementKind {
    StatementKind::Assign(place, rvalue)
}

/// The place of the local `_n`.
fn local(n: u32) -> Place {
    Place::local(LocalId(n))
}

/// The constant `int` `value` as an operand.
fn int_operand(value: i128) -> Operand {
    Operand::Const(Const::Int(value, Primitive::Int))
}

/// What a run of a hand-built body gives, with no field initializer.
fn run_hand_built(body: &Body) -> Result<ConstValue, Stop> {
    let db = TestDb::new();
    run(&db, body, &mut |_, _| None)
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

#[test]
fn a_direct_cycle_is_one_member() {
    let source = unchecked("const A: int = A + 1;\n");
    let a = source.constant("A");
    assert_eq!(source.cycle(a), Some((vec!["A".to_owned()], "A")));
    assert_eq!(
        source.graph().cycle_of(a).map(|cycle| cycle.read),
        Some(source.range_in("= A + 1", "A"))
    );
    assert_eq!(source.graph().order(), [a]);
}

#[test]
fn a_cycle_through_two_constants_starts_at_the_first() {
    let source = unchecked("const B: int = A;\nconst A: int = B;\n");
    let expected = Some((vec!["B".to_owned(), "A".to_owned()], "A"));
    assert_eq!(source.cycle(source.constant("A")), expected);
    assert_eq!(source.cycle(source.constant("B")), expected);
    assert_eq!(
        source
            .graph()
            .cycle_of(source.constant("B"))
            .map(|cycle| cycle.read),
        Some(source.range_in("B: int = A", "A"))
    );
    // The members of one component are in the order of their sort keys.
    assert_eq!(source.names(source.graph().order()), ["B", "A"]);
}

#[test]
fn a_cycle_through_a_field_initializer() {
    let source = unchecked("struct P {\n    public X: int = A.X;\n}\nconst A: P = new P {};\n");
    let x = ConstKey::Initializer {
        item: source.item("P"),
        field: 0,
    };
    let expected = Some((vec!["A".to_owned(), "P.X".to_owned()], "new P {}"));
    assert_eq!(source.cycle(source.constant("A")), expected);
    assert_eq!(source.cycle(x), expected);
}

#[test]
fn a_cycle_through_three_constants_follows_the_reads() {
    let source = unchecked("const A: int = B;\nconst B: int = C;\nconst C: int = A;\n");
    let expected = Some((vec!["A".to_owned(), "B".to_owned(), "C".to_owned()], "B"));
    for name in ["A", "B", "C"] {
        assert_eq!(source.cycle(source.constant(name)), expected, "{name}");
    }
}

#[test]
fn a_cycle_starts_at_its_smallest_key_whatever_the_walk_reached_first() {
    // The walk reaches `P.X` through `C` before it reaches `A`.
    let source = unchecked(
        "struct P {\n    public X: int = A.X;\n}\nconst C: P = new P {};\nconst A: P = new P {};\n",
    );
    assert_eq!(source.names(source.graph().order()), ["A", "P.X", "C"]);
    assert_eq!(
        source
            .graph()
            .cycle_of(source.constant("A"))
            .map(|cycle| cycle.read),
        Some(source.range_in("A: P = new P {}", "new P {}"))
    );
    assert_eq!(source.cycle(source.constant("C")), None);
}

#[test]
fn every_member_of_a_component_maps_to_its_cycle() {
    let source = unchecked("const A: int = B + C;\nconst B: int = A;\nconst C: int = A;\n");
    let expected = Some((vec!["A".to_owned(), "B".to_owned()], "B"));
    for name in ["A", "B", "C"] {
        assert_eq!(source.cycle(source.constant(name)), expected, "{name}");
    }
    assert_eq!(source.names(source.graph().order()), ["A", "B", "C"]);
}

#[test]
fn a_cycle_leaves_a_read_that_does_not_lead_back_to_its_start() {
    // From `A`, `B` reads `C` first, which only leads back to `B`.
    let source = unchecked("const A: int = B;\nconst B: int = C + A;\nconst C: int = B;\n");
    let expected = Some((vec!["A".to_owned(), "B".to_owned()], "B"));
    for name in ["A", "B", "C"] {
        assert_eq!(source.cycle(source.constant(name)), expected, "{name}");
    }
}

#[test]
fn a_cycle_follows_only_reads_inside_its_component() {
    // `A` reads `D` first, which is outside the component.
    let source = unchecked("const D: int = 1;\nconst A: int = D + B;\nconst B: int = D + A;\n");
    assert_eq!(
        source.cycle(source.constant("B")),
        Some((vec!["A".to_owned(), "B".to_owned()], "B"))
    );
    assert_eq!(source.cycle(source.constant("D")), None);
    assert_eq!(source.names(source.graph().order()), ["D", "A", "B"]);
}

#[test]
fn a_self_read_inside_a_component_closes_the_cycle_at_once() {
    let source = unchecked("const A: int = A + B;\nconst B: int = A;\n");
    let expected = Some((vec!["A".to_owned()], "A"));
    assert_eq!(source.cycle(source.constant("B")), expected);
}

#[test]
fn the_order_puts_reads_first() {
    let source = checked("const B: int = A + 1;\nconst A: int = 1;\n");
    let graph = source.graph();
    assert_eq!(graph.order(), [source.constant("A"), source.constant("B")]);
    assert_eq!(graph.cycle_of(source.constant("A")), None);
    assert_eq!(graph.cycle_of(source.constant("B")), None);
}

#[test]
fn the_order_lists_constants_then_defaults_and_the_initializers_they_read() {
    let source = checked(
        "F(x: int = B) {}\nstruct P {\n    public X: int = A;\n}\nconst B: int = 1;\nconst C: P = new P {};\nconst A: int = 2;\n",
    );
    assert_eq!(
        source.names(source.graph().order()),
        ["B", "A", "P.X", "C", "the default value of x in F"]
    );
}

#[test]
fn the_order_leaves_out_constants_of_other_projects() {
    let db = TestDb::new();
    let util = db.project(
        "util",
        ProjectKind::Lib,
        &[("src/Lib.mr", "public const K: int = 1;\n")],
        Vec::new(),
    );
    let project = db.project(
        "users-api",
        ProjectKind::App,
        &[("src/Program.mr", "const A: int = Util.K;\n")],
        vec![Dependency {
            name: "Util".to_owned(),
            project: util,
        }],
    );
    assert_eq!(codes(diagnostics(&db, project)), Vec::<String>::new());
    let file = file_named(&db, project, "src/Program.mr");
    let source = Source { db, project, file };
    let a = source.constant("A");
    let reads = &const_scan(&source.db, project, a).reads;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, util);
    assert_eq!(source.graph().order(), [a]);
}

#[test]
fn a_long_chain_builds_without_recursion() {
    const LENGTH: usize = 2_000;
    let text: String = (0..LENGTH)
        .map(|i| {
            if i + 1 == LENGTH {
                format!("const C{i}: int = 1;\n")
            } else {
                format!("const C{i}: int = C{} + 1;\n", i + 1)
            }
        })
        .collect();
    let source = unchecked(&text);
    let order = source.graph().order();
    assert_eq!(order.len(), LENGTH);
    assert_eq!(order[0], source.constant(&format!("C{}", LENGTH - 1)));
    assert_eq!(order[LENGTH - 1], source.constant("C0"));
}

#[test]
fn run_computes_arithmetic_and_casts() {
    let source = checked(
        "const C: byte = (byte)(299L + 1L);\n\
         const S: string = \"ab\" + \"cd\";\n\
         const N: long = -(5L) * 2L;\n\
         const R: double = 2.5 * (double)(3L);\n",
    );
    assert_eq!(source.run("C"), Ok(int(44, Primitive::Byte)));
    assert_eq!(
        source.run("S"),
        Ok(ConstValue::Scalar(Const::String("abcd".to_owned())))
    );
    assert_eq!(source.run("N"), Ok(int(-10, Primitive::Long)));
    assert_eq!(
        source.run("R"),
        Ok(ConstValue::Scalar(Const::Real(
            7.5f64.to_bits(),
            Primitive::Double
        )))
    );
}

#[test]
fn run_runs_a_default_value_with_its_conversion() {
    let source = checked("O(x: long = (int)((1L > 0L) ? 2 : 3)) {}\n");
    let ConstKey::Default { owner, default, .. } = source.only_default() else {
        panic!("a default value");
    };
    let mir = default_mir(&source.db, source.project, source.file, owner, default);
    let mut initializer = |_: ItemId, _: u32| None;
    assert_eq!(
        run(&source.db, &mir.bodies[0], &mut initializer),
        Ok(int(2, Primitive::Long))
    );
}

#[test]
fn run_follows_conditionals_and_short_circuits() {
    let source = checked(
        "const D: int = (1 < 2 && 3 > 4) ? 1 : 2;\n\
         const T: int = (1L < 2L || 3L > 4L) ? 1 : 2;\n\
         const E: bool = true || 1L / 0L > 0L;\n\
         const F: bool = false && 1L / 0L > 0L;\n",
    );
    assert_eq!(source.run("D"), Ok(int(2, Primitive::Int)));
    assert_eq!(source.run("T"), Ok(int(1, Primitive::Int)));
    assert_eq!(source.run("E"), Ok(boolean(true)));
    assert_eq!(source.run("F"), Ok(boolean(false)));
}

#[test]
fn run_builds_structs_and_tuples() {
    let source = checked(
        "struct P {\n    public X: int = 1;\n    public Y: int;\n}\n\
         const C: P = new P { Y = 2 };\n\
         const T: (int, string) = (1, \"a\");\n\
         struct Q {\n    public A: int;\n    public B: string;\n}\n\
         const U: string = new Q { A = 1, B = \"b\" }.B;\n",
    );
    let p = source.item("P");
    let mut calls = Vec::new();
    let value = source.run_with("C", &mut |item, field| {
        calls.push((item, field));
        Some(int(7, Primitive::Int))
    });
    assert_eq!(calls, [(p, 0)]);
    assert_eq!(
        value,
        Ok(ConstValue::Aggregate(
            AggregateKind::Struct(p),
            vec![int(7, Primitive::Int), int(2, Primitive::Int)]
        ))
    );
    assert_eq!(
        source.run("T"),
        Ok(ConstValue::Aggregate(
            AggregateKind::Tuple,
            vec![
                int(1, Primitive::Int),
                ConstValue::Scalar(Const::String("a".to_owned()))
            ]
        ))
    );
    assert_eq!(
        source.run("U"),
        Ok(ConstValue::Scalar(Const::String("b".to_owned())))
    );
}

#[test]
fn run_stops_at_a_failed_operation() {
    let source = checked(
        "const F: long = 9223372036854775807L + 1L;\n\
         const G: long = 1L % (2L - 2L);\n\
         const H: int = -((int)(-2147483648L));\n\
         const N: nint = (nint)1099511627776L;\n\
         const X: long = (9223372036854775806L + 2L) * 2L;\n",
    );
    assert_eq!(
        source.failure("X"),
        Failure {
            range: source.range_in(
                "(9223372036854775806L + 2L) * 2L",
                "9223372036854775806L + 2L"
            ),
            error: ArithError::Overflow,
            ty: Ty::Primitive(Primitive::Long),
            operation: "9223372036854775806 + 2".to_owned(),
        }
    );
    assert_eq!(
        source.failure("F"),
        Failure {
            range: source.range_of("9223372036854775807L + 1L"),
            error: ArithError::Overflow,
            ty: Ty::Primitive(Primitive::Long),
            operation: "9223372036854775807 + 1".to_owned(),
        }
    );
    assert_eq!(
        source.failure("G"),
        Failure {
            range: source.range_of("1L % (2L - 2L)"),
            error: ArithError::RemainderByZero,
            ty: Ty::Primitive(Primitive::Long),
            operation: "1 % 0".to_owned(),
        }
    );
    assert_eq!(
        source.failure("H"),
        Failure {
            range: source.range_of("-((int)(-2147483648L))"),
            error: ArithError::Overflow,
            ty: Ty::Primitive(Primitive::Int),
            operation: "-(-2147483648)".to_owned(),
        }
    );
    assert_eq!(
        source.failure("N"),
        Failure {
            range: source.range_of("(nint)1099511627776L"),
            error: ArithError::TargetDependent,
            ty: Ty::Primitive(Primitive::NInt),
            operation: "(nint)1099511627776".to_owned(),
        }
    );
}

#[test]
fn run_stops_when_an_initializer_has_no_value() {
    let source = checked(
        "struct P {\n    public X: int = 1;\n    public Y: int;\n}\n\
         const C: P = new P { Y = 2 };\n",
    );
    assert_eq!(
        source.run_with("C", &mut |_, _| None),
        Err(Stop::Dependency)
    );
}

#[test]
fn run_compares_tuples_field_by_field() {
    let source = checked(
        "const H: bool = (1, 2.0) == (1, -0.0 + 2.0);\n\
         const I: bool = (1, 2.0) == (1, 3.0);\n\
         const J: bool = (1, 2.0) != (1, 3.0);\n\
         const K: bool = ((1L, \"a\"), 0.0) != ((1L, \"a\"), -0.0);\n",
    );
    assert_eq!(source.run("H"), Ok(boolean(true)));
    assert_eq!(source.run("I"), Ok(boolean(false)));
    assert_eq!(source.run("J"), Ok(boolean(true)));
    assert_eq!(source.run("K"), Ok(boolean(false)));
}

#[test]
fn run_writes_through_a_field_and_clears_a_dead_local() {
    let tuple = Rvalue::Aggregate(AggregateKind::Tuple, vec![int_operand(1), int_operand(2)]);
    let field = Place {
        local: LocalId(1),
        projections: vec![Projection::Field(1)],
    };
    let body = hand_built(
        2,
        vec![block(
            vec![
                StatementKind::StorageLive(LocalId(1)),
                assign(local(1), tuple),
                assign(field.clone(), Rvalue::Use(int_operand(5))),
                StatementKind::Nop,
                assign(local(0), Rvalue::Use(Operand::Move(local(1)))),
            ],
            TerminatorKind::Return,
        )],
    );
    assert_eq!(
        run_hand_built(&body),
        Ok(ConstValue::Aggregate(
            AggregateKind::Tuple,
            vec![int(1, Primitive::Int), int(5, Primitive::Int)]
        ))
    );
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: _1 has no value")]
fn run_panics_on_a_local_read_after_its_storage_ends() {
    let body = hand_built(
        2,
        vec![block(
            vec![
                assign(local(1), Rvalue::Use(int_operand(1))),
                StatementKind::StorageDead(LocalId(1)),
                assign(local(0), Rvalue::Use(Operand::Copy(local(1)))),
            ],
            TerminatorKind::Return,
        )],
    );
    let _ = run_hand_built(&body);
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: _0 has no value")]
fn run_panics_on_a_return_without_a_value() {
    let _ = run_hand_built(&hand_built(
        1,
        vec![block(Vec::new(), TerminatorKind::Return)],
    ));
}

#[test]
fn run_goes_to_the_next_block() {
    let body = hand_built(
        1,
        vec![
            block(Vec::new(), TerminatorKind::Goto(BlockId(2))),
            block(Vec::new(), TerminatorKind::Unreachable),
            block(
                vec![assign(local(0), Rvalue::Use(int_operand(3)))],
                TerminatorKind::Return,
            ),
        ],
    );
    assert_eq!(run_hand_built(&body), Ok(int(3, Primitive::Int)));
}

/// A body that calls the initializer of field 3 of `item` into `dest`,
/// then continues at `next`, which returns `_1`.
fn initializer_call(item: ItemId, dest: Option<Place>, next: Option<BlockId>) -> Body {
    hand_built(
        2,
        vec![
            block(
                Vec::new(),
                TerminatorKind::Call {
                    callee: Callee::Initializer(item, 3),
                    args: Vec::new(),
                    dest,
                    next,
                },
            ),
            block(
                vec![assign(local(0), Rvalue::Use(Operand::Copy(local(1))))],
                TerminatorKind::Return,
            ),
        ],
    )
}

#[test]
fn run_writes_an_initializer_to_its_destination() {
    let source = checked("struct P {}\n");
    let p = source.item("P");
    let body = initializer_call(p, Some(local(1)), Some(BlockId(1)));
    let mut seen = Vec::new();
    let value = run(&source.db, &body, &mut |item, field| {
        seen.push((item, field));
        Some(int(9, Primitive::Int))
    });
    assert_eq!(seen, [(p, 3)]);
    assert_eq!(value, Ok(int(9, Primitive::Int)));
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: _1 has no value")]
fn run_drops_an_initializer_without_a_destination() {
    let source = checked("struct P {}\n");
    let body = initializer_call(source.item("P"), None, Some(BlockId(1)));
    let _ = run(&source.db, &body, &mut |_, _| Some(int(9, Primitive::Int)));
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: a call that does not return")]
fn run_panics_on_an_initializer_that_does_not_return() {
    let source = checked("struct P {}\n");
    let body = initializer_call(source.item("P"), Some(local(1)), None);
    let _ = run(&source.db, &body, &mut |_, _| Some(int(9, Primitive::Int)));
}

/// The place `_n` followed by `projections`.
fn projected(n: u32, projections: &[Projection]) -> Place {
    Place {
        local: LocalId(n),
        projections: projections.to_vec(),
    }
}

/// `_0 = rvalue`.
fn to_return(rvalue: Rvalue) -> StatementKind {
    assign(local(0), rvalue)
}

/// `_0 = copy place`.
fn copy_to_return(place: Place) -> StatementKind {
    to_return(Rvalue::Use(Operand::Copy(place)))
}

/// Each form a body that constant evaluation runs never holds, after
/// `_1 = (1,)` and `_2 = (1, 2)`, and what the panic it gives says.
fn unsupported_forms() -> Vec<(StatementKind, TerminatorKind, &'static str)> {
    use Projection::{Deref, Field};
    let ret = TerminatorKind::Return;
    let one = || Rvalue::Use(int_operand(1));
    let whole = || Operand::Copy(local(1));
    let forms = vec![
        (
            to_return(Rvalue::Borrow(BorrowMode::In, local(1))),
            "a borrow",
        ),
        (copy_to_return(projected(1, &[Deref])), "a dereference"),
        (assign(projected(1, &[Deref]), one()), "a dereference"),
        (
            copy_to_return(projected(1, &[Field(0), Field(0)])),
            "field 0 of a scalar",
        ),
        (
            assign(projected(1, &[Field(0), Field(0)]), one()),
            "field 0 of a scalar",
        ),
        (copy_to_return(projected(1, &[Field(3)])), "no field 3"),
        (assign(projected(1, &[Field(3)]), one()), "no field 3"),
        (copy_to_return(local(5)), "no local _5"),
        (assign(local(5), one()), "no local _5"),
        (StatementKind::StorageDead(LocalId(5)), "no local _5"),
        (assign(projected(0, &[Field(0)]), one()), "_0 has no value"),
        (
            to_return(Rvalue::Interpolate(vec![InterpPiece::Text("a".to_owned())])),
            "an interpolated string",
        ),
        (
            to_return(Rvalue::Cast(CastKind::Implicit, int_operand(1), Ty::String)),
            "a cast to string",
        ),
        (
            to_return(Rvalue::Binary(BinOp::Lt, whole(), whole())),
            "`<` on an aggregate",
        ),
        (
            to_return(Rvalue::Binary(BinOp::Eq, whole(), int_operand(1))),
            "`==` on values of different shapes",
        ),
        (
            to_return(Rvalue::Binary(BinOp::Eq, whole(), Operand::Copy(local(2)))),
            "`==` on values of different shapes",
        ),
        (
            to_return(Rvalue::Unary(UnOp::Neg, whole())),
            "an aggregate where a scalar goes",
        ),
    ];
    let mut forms: Vec<_> = forms
        .into_iter()
        .map(|(statement, what)| (statement, ret.clone(), what))
        .collect();
    forms.push((
        StatementKind::Nop,
        TerminatorKind::Unreachable,
        "an unreachable block",
    ));
    forms.push((
        StatementKind::Nop,
        TerminatorKind::Call {
            callee: Callee::External(vec!["F".to_owned()]),
            args: Vec::new(),
            dest: None,
            next: Some(BlockId(0)),
        },
        "a call",
    ));
    forms
}

#[test]
fn run_panics_on_what_constant_evaluation_never_runs() {
    for (statement, terminator, what) in unsupported_forms() {
        let tuple = |parts: Vec<Operand>| Rvalue::Aggregate(AggregateKind::Tuple, parts);
        let prelude = vec![
            assign(local(1), tuple(vec![int_operand(1)])),
            assign(local(2), tuple(vec![int_operand(1), int_operand(2)])),
            statement,
        ];
        let body = hand_built(3, vec![block(prelude, terminator)]);
        let panic = std::panic::catch_unwind(|| run_hand_built(&body)).expect_err(what);
        let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        assert_eq!(
            message,
            format!("internal error evaluating Hand: {what}"),
            "{what}"
        );
    }
}

#[test]
fn run_switches_on_the_first_matching_target() {
    let switch = |discr: i128| {
        hand_built(
            1,
            vec![
                block(
                    Vec::new(),
                    TerminatorKind::Switch {
                        discr: int_operand(discr),
                        targets: vec![
                            (Const::Int(1, Primitive::Int), BlockId(1)),
                            (Const::Int(2, Primitive::Int), BlockId(2)),
                        ],
                        otherwise: BlockId(3),
                    },
                ),
                block(
                    vec![assign(local(0), Rvalue::Use(int_operand(10)))],
                    TerminatorKind::Return,
                ),
                block(
                    vec![assign(local(0), Rvalue::Use(int_operand(20)))],
                    TerminatorKind::Return,
                ),
                block(
                    vec![assign(local(0), Rvalue::Use(int_operand(30)))],
                    TerminatorKind::Return,
                ),
            ],
        )
    };
    assert_eq!(run_hand_built(&switch(1)), Ok(int(10, Primitive::Int)));
    assert_eq!(run_hand_built(&switch(2)), Ok(int(20, Primitive::Int)));
    assert_eq!(run_hand_built(&switch(5)), Ok(int(30, Primitive::Int)));
}

#[test]
fn run_compares_aggregates_with_eq_and_ne() {
    let pair = |second: i128| Operand::Const(Const::Int(second, Primitive::Int));
    let compare = |op: BinOp, second: i128| {
        let tuple =
            |n: i128| Rvalue::Aggregate(AggregateKind::Tuple, vec![int_operand(1), pair(n)]);
        hand_built(
            3,
            vec![block(
                vec![
                    assign(local(1), tuple(2)),
                    assign(local(2), tuple(second)),
                    assign(
                        local(0),
                        Rvalue::Binary(op, Operand::Copy(local(1)), Operand::Copy(local(2))),
                    ),
                ],
                TerminatorKind::Return,
            )],
        )
    };
    assert_eq!(run_hand_built(&compare(BinOp::Eq, 2)), Ok(boolean(true)));
    assert_eq!(run_hand_built(&compare(BinOp::Eq, 3)), Ok(boolean(false)));
    assert_eq!(run_hand_built(&compare(BinOp::Ne, 2)), Ok(boolean(false)));
    assert_eq!(run_hand_built(&compare(BinOp::Ne, 3)), Ok(boolean(true)));
}

#[test]
fn run_gives_the_lengths_of_a_concatenation_too_long_to_keep() {
    let long = "a".repeat(MAX_STRING_BYTES - 1);
    let string = |text: &str| Operand::Const(Const::String(text.to_owned()));
    let body = hand_built(
        1,
        vec![block(
            vec![to_return(Rvalue::Binary(
                BinOp::Add,
                string(&long),
                string("bc"),
            ))],
            TerminatorKind::Return,
        )],
    );
    let Err(Stop::Failed(failure)) = run_hand_built(&body) else {
        panic!("the concatenation fails");
    };
    assert_eq!(failure.error, ArithError::TooLong(MAX_STRING_BYTES + 1));
    assert_eq!(failure.operation, "<16777215 bytes> + <2 bytes>");
}

impl Source {
    /// What evaluating `key` gives.
    fn evaluated(&self, key: ConstKey) -> &Evaluated {
        const_value(&self.db, self.project, key)
    }

    /// The error evaluating `key` reports, at `range`, of `kind`.
    fn error(&self, key: ConstKey, range: TextRange, kind: ErrorKind) -> Evaluated {
        Evaluated::Failed(Some(ConstError { key, range, kind }))
    }

    /// The statements and terminators of the MIR of the function `name`,
    /// one per line, without their spans.
    fn code(&self, name: &str) -> Vec<String> {
        let owner = BodyOwner::Item(self.item(name).item);
        let mut out = String::new();
        crate::write_mir(
            &mut out,
            &self.db,
            Some(self.file),
            crate::mir(&self.db, self.project, self.file, owner),
        );
        out.lines()
            .filter_map(|line| line.split_once(" // "))
            .map(|(code, _)| code.trim().to_owned())
            .collect()
    }

    /// The message of the internal error that lowering the function `name`
    /// panics with.
    fn internal_error(&self, name: &str) -> String {
        let owner = BodyOwner::Item(self.item(name).item);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::mir(&self.db, self.project, self.file, owner);
        }))
        .expect_err("an internal error");
        panic
            .downcast_ref::<String>()
            .expect("a formatted message")
            .clone()
    }
}

/// What `evaluated` is: `value`, `unsupported` and what, or what waits and
/// the milestone it waits for.
fn outcome(evaluated: &Evaluated) -> String {
    match evaluated {
        Evaluated::Value(_) => "value".to_owned(),
        Evaluated::Failed(Some(ConstError {
            kind: ErrorKind::Unsupported(what),
            ..
        })) => format!("unsupported {what}"),
        Evaluated::Later(later) => format!("{} {}", later.what, later.milestone),
        Evaluated::Failed(_) => format!("{evaluated:?}"),
    }
}

#[test]
fn a_constant_reads_another() {
    let source = checked("const A: int = 1_500_000;\nconst B: int = A * 2;\n");
    assert_eq!(
        source.evaluated(source.constant("B")),
        &Evaluated::Value(int(3_000_000, Primitive::Int))
    );
}

#[test]
fn a_failed_operation_is_its_own_error() {
    let source =
        checked("const A: int = 2_000_000_000;\nconst B: int = A * 2;\nconst C: int = B + 1;\n");
    let b = source.constant("B");
    let at = source.range_of("A * 2");
    let failure = Failure {
        range: at,
        error: ArithError::Overflow,
        ty: Ty::Primitive(Primitive::Int),
        operation: "2000000000 * 2".to_owned(),
    };
    assert_eq!(
        source.evaluated(b),
        &source.error(b, at, ErrorKind::Failed(failure))
    );
    // Its reader reports nothing more.
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Failed(None)
    );
}

#[test]
fn cycles_fail_once() {
    let source = unchecked("const A: int = A + 1;\nconst C: int = A + 1;\n");
    let a = source.constant("A");
    assert_eq!(
        source.evaluated(a),
        &source.error(
            a,
            source.range_in("= A + 1;\nconst", "A"),
            ErrorKind::Cycle(vec![a])
        )
    );
    // A reader outside the cycle reports nothing more.
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Failed(None)
    );

    let source = unchecked("const B: int = A;\nconst A: int = B;\n");
    let (a, b) = (source.constant("A"), source.constant("B"));
    assert_eq!(
        source.evaluated(b),
        &source.error(
            b,
            source.range_in("B: int = A", "A"),
            ErrorKind::Cycle(vec![b, a])
        )
    );
    assert_eq!(source.evaluated(a), &Evaluated::Failed(None));

    let source = unchecked("struct P {\n    public X: int = A.X;\n}\nconst A: P = new P {};\n");
    let a = source.constant("A");
    let x = ConstKey::Initializer {
        item: source.item("P"),
        field: 0,
    };
    assert_eq!(
        source.evaluated(a),
        &source.error(a, source.range_of("new P {}"), ErrorKind::Cycle(vec![a, x]))
    );
    assert_eq!(source.evaluated(x), &Evaluated::Failed(None));
}

#[test]
fn a_member_of_a_component_outside_its_cycle_fails_without_an_error() {
    let source = unchecked("const A: int = B + C;\nconst B: int = A;\nconst C: int = A;\n");
    assert!(matches!(
        source.evaluated(source.constant("A")),
        Evaluated::Failed(Some(_))
    ));
    for name in ["B", "C"] {
        assert_eq!(
            source.evaluated(source.constant(name)),
            &Evaluated::Failed(None),
            "{name}"
        );
    }
}

#[test]
fn unsupported_code_fails() {
    let source = checked("Zero(): int => 0;\nconst A: int = Zero();\nconst B: int = A;\n");
    let a = source.constant("A");
    assert_eq!(
        source.evaluated(a),
        &source.error(
            a,
            source.range_in("= Zero();", "Zero()"),
            ErrorKind::Unsupported("a call")
        )
    );
    assert_eq!(
        source.evaluated(source.constant("B")),
        &Evaluated::Failed(None)
    );
}

#[test]
fn a_struct_constant_takes_its_initializers() {
    let source = checked(
        "struct P {\n    public X: int = 1 + 2;\n    public Y: int;\n}\nconst C: P = new P { Y = 4 };\n",
    );
    let p = source.item("P");
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Value(ConstValue::Aggregate(
            AggregateKind::Struct(p),
            vec![int(3, Primitive::Int), int(4, Primitive::Int)]
        ))
    );
    assert_eq!(
        source.evaluated(ConstKey::Initializer { item: p, field: 0 }),
        &Evaluated::Value(int(3, Primitive::Int))
    );
}

#[test]
fn a_failed_initializer_fails_its_creation_without_an_error() {
    let source = checked(
        "struct P {\n    public X: int = Big * 2;\n}\nconst Big: int = 2_000_000_000;\nconst C: P = new P {};\n",
    );
    let x = ConstKey::Initializer {
        item: source.item("P"),
        field: 0,
    };
    assert!(matches!(
        source.evaluated(x),
        Evaluated::Failed(Some(ConstError {
            kind: ErrorKind::Failed(_),
            ..
        }))
    ));
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Failed(None)
    );
}

#[test]
fn a_constant_whose_code_comes_later_waits() {
    let source = checked(
        "const K: int = default;\nconst J: int = K + 1;\nF(): int => K;\n\
         struct P {\n    public X: int = default;\n}\nconst C: P = new P {};\n",
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("K"))),
        "`default` M3.12"
    );
    // A constant that reads it waits for it by name, and one that leaves
    // out a field whose initializer waits, for what the initializer waits.
    assert_eq!(
        outcome(source.evaluated(source.constant("J"))),
        "a constant M3.12"
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("C"))),
        "`default` M3.12"
    );
    assert_eq!(
        source.internal_error("F"),
        "internal error lowering UsersApi.F: a constant is not lowered until M3.12"
    );
}

#[test]
fn a_constant_waits_for_the_latest_of_its_reads_the_first_on_a_tie() {
    let source = checked(
        "const B: bool = true;\nstruct P {\n    public X: bool = B is true;\n}\n\
         struct Q {\n    public Y: int = default;\n}\nstruct R {\n    public Z: int = sizeof(int);\n}\n\
         const C: (P, Q) = (new P {}, new Q {});\nconst D: (Q, P) = (new Q {}, new P {});\n\
         const E: (Q, R) = (new Q {}, new R {});\nconst G: (R, Q) = (new R {}, new Q {});\n",
    );
    for (name, expected) in [
        ("C", "`default` M3.12"),
        ("D", "`default` M3.12"),
        ("E", "`default` M3.12"),
        ("G", "`sizeof` M3.12"),
    ] {
        assert_eq!(
            outcome(source.evaluated(source.constant(name))),
            expected,
            "{name}"
        );
    }
}

#[test]
fn a_waiting_constant_waits_for_the_latest_of_its_code_and_its_reads() {
    let source = checked(
        "struct P {\n    public static S: int = 1;\n}\nconst K: int = P.S;\n\
         const A: int = K + sizeof(int);\nconst T: int = default;\n\
         const B: int = T + sizeof(int);\nconst C: int = sizeof(int) + K;\n",
    );
    for (name, expected) in [
        ("K", "a static member M3.13"),
        // `K` waits longer than `sizeof`.
        ("A", "a constant M3.13"),
        ("C", "a constant M3.13"),
        // On a tie its own code names it.
        ("B", "`sizeof` M3.12"),
    ] {
        assert_eq!(
            outcome(source.evaluated(source.constant(name))),
            expected,
            "{name}"
        );
    }
}

#[test]
fn a_constant_whose_code_waits_waits_even_when_a_read_fails() {
    let source =
        checked("Zero(): int => 0;\nconst B: int = Zero();\nconst A: int = B + sizeof(int);\n");
    assert_eq!(
        outcome(source.evaluated(source.constant("A"))),
        "`sizeof` M3.12"
    );
}

#[test]
fn a_read_that_fails_comes_before_one_that_waits() {
    let source = checked(
        "struct P {\n    public X: int = default;\n}\nZero(): int => 0;\n\
         struct Q {\n    public Y: int = Zero();\n}\n\
         const C: (P, Q) = (new P {}, new Q {});\n",
    );
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Failed(None)
    );
}

#[test]
fn a_default_value_is_evaluated() {
    let source = checked("const B: int = 2;\nF(x: long = B * 3) {}\n");
    assert_eq!(
        source.evaluated(source.only_default()),
        &Evaluated::Value(int(6, Primitive::Long))
    );
}

/// The constant `A` of `text`, or its only default value that needs
/// evaluation when it declares no `A`.
fn evaluated_key(source: &Source) -> ConstKey {
    let tree = mora_hir::item_tree(&source.db, source.file);
    if tree.items().any(|(_, item)| item.name == "A") {
        source.constant("A")
    } else {
        source.only_default()
    }
}

#[test]
fn every_expression_kind_in_a_constant_evaluates_reports_or_waits() {
    let cases = [
        ("const A: int = 1;", "value"),
        ("const A: long = 1L;", "value"),
        ("const A: byte = 1;", "value"),
        ("const A: sbyte = -1;", "value"),
        ("const A: short = 1;", "value"),
        ("const A: ushort = 1;", "value"),
        ("const A: uint = 1;", "value"),
        ("const A: ulong = 1;", "value"),
        ("const A: nint = 1;", "value"),
        ("const A: nuint = 1;", "value"),
        ("const A: float = 1.5f;", "value"),
        ("const A: double = 1.5;", "value"),
        ("const A: bool = true;", "value"),
        ("const A: char = 'a';", "value"),
        ("const A: string = \"s\";", "value"),
        ("const B: int = 2;\nconst A: int = -B;", "value"),
        ("const B: bool = true;\nconst A: bool = !B;", "value"),
        ("const B: int = 2;\nconst A: int = B * 3;", "value"),
        (
            "const B: string = \"a\";\nconst A: string = B + \"b\";",
            "value",
        ),
        ("const B: int = 300;\nconst A: byte = (byte)B;", "value"),
        ("const B: int = 2;\nconst A: int = B > 1 ? 1 : 2;", "value"),
        ("const A: (int, string) = (1, \"a\");", "value"),
        (
            "struct P {\n    public X: int;\n}\nconst A: P = new P { X = 1 };",
            "value",
        ),
        ("const B: int = 2;\nconst A: int = B;", "value"),
        (
            "struct P {\n    public const K: int = 4;\n}\nconst A: int = P.K;",
            "value",
        ),
        (
            "struct P {\n    public X: int;\n}\nconst O: P = new P { X = 1 };\nconst A: int = O.X;",
            "value",
        ),
        (
            "Zero(): int => 0;\nconst A: int = Zero();",
            "unsupported a call",
        ),
        (
            "const A: string = $\"{1}\";",
            "unsupported an interpolated string",
        ),
        ("const A: (() => int) = () => 1;", "unsupported a lambda"),
        // A loop can only come in a lambda, which never runs.
        (
            "const A: int = (() => { while (true) {} return 1; })();",
            "unsupported a lambda",
        ),
        ("const A: int = default;", "`default` M3.12"),
        ("const A: int = sizeof(int);", "`sizeof` M3.12"),
        (
            "const B: int = 1;\nconst A: string = nameof(B);",
            "`nameof` M3.12",
        ),
        // A read the scan does not follow, of the constant itself or of one
        // that reads it, waits with what holds it.
        ("const A: string = nameof(A);", "`nameof` M3.12"),
        (
            "const B: bool = true;\nconst A: int = match (B) { true => A, false => 2 };",
            "a `match` M3.3",
        ),
        (
            "const A: int = match (true) { _ => B };\nconst B: int = A;",
            "a `match` M3.3",
        ),
        (
            "enum Color { Red }\nconst A: Color = Color.Red;",
            "the initializer of a constant with an enum type M3.2",
        ),
        (
            "const A: int = int.MaxValue;",
            "a member of a built-in type M1.15",
        ),
        ("const A: int[] = [1, 2];", "a collection expression M3.12"),
        (
            "const A: int[] = new int[2];",
            "the initializer of a constant with an array type M2.3",
        ),
        (
            "struct P {\n    public X: int;\n}\nconst O: P = new P { X = 1 };\nconst A: P = O with { X = 2 };",
            "`with` M2.2",
        ),
        (
            "const B: bool = true;\nconst A: int = match (B) { true => 1, false => 2 };",
            "a `match` M3.3",
        ),
        (
            "const B: bool = true;\nconst A: bool = B is true;",
            "an `is` test M3.3",
        ),
        (
            "struct P {\n    public X: int;\n}\nconst O: P? = none;\nconst A: int? = O?.X;",
            // The type of the constant comes as late as `?.`, and first.
            "the initializer of a constant with an option type M3.5",
        ),
        (
            "const A: int? = none;",
            "the initializer of a constant with an option type M3.5",
        ),
        (
            "const A: bool = \"a\" < \"b\";",
            "an ordering of strings M5.2",
        ),
        ("F(x: int? = none) {}", "`none` M3.5"),
        // A default value waits for each expression in it, by the rules
        // that look at one expression at a time.
        (
            "G<T>(x: T): T => x;\nF(x: int = G(1)) {}",
            "a call of a generic function M3.9",
        ),
        (
            "struct P {\n    public X: int;\n    public P(x: int) { X = x; }\n}\nF(p: P = new P(1)) {}",
            "a constructor call M2.1",
        ),
        (
            "struct P {\n    public X: int;\n    public static operator -(p: P): P => p;\n}\n\
             const O: P = new P { X = 1 };\nF(p: P = -O) {}",
            "an operator on a struct M3.6",
        ),
        (
            "class C {}\nF(c: C = new C()) {}",
            "an expression with a class type M2.1",
        ),
    ];
    let outcomes: Vec<(&str, String)> = cases
        .iter()
        .map(|&(text, _)| {
            let source = unchecked(&format!("{text}\n"));
            assert_eq!(
                codes(diagnostics(&source.db, source.project)),
                Vec::<String>::new(),
                "{text}"
            );
            (text, outcome(source.evaluated(evaluated_key(&source))))
        })
        .collect();
    let expected: Vec<(&str, String)> = cases
        .iter()
        .map(|&(text, outcome)| (text, outcome.to_owned()))
        .collect();
    assert_eq!(outcomes, expected);
}

#[test]
fn editing_a_constant_changes_its_readers() {
    use salsa::Setter;

    let mut source = checked("const A: int = 1;\nF(): int => A;\n");
    assert_eq!(source.code("F"), ["_0 = 1;", "return;"]);
    source
        .file
        .set_text(&mut source.db)
        .to("const A: int = 5;\nF(): int => A;\n".to_owned());
    assert_eq!(source.code("F"), ["_0 = 5;", "return;"]);
}
