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
use super::interp::{Failure, run};
use super::report::failed_message;
use super::scan::{children, const_scan};
use super::{
    ConstError, ConstKey, ConstValue, ErrorKind, Evaluated, const_read, const_value, constants,
    defaults,
};
use crate::lower::consts::eval_mir;
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

/// `program` as `src/Program.mr` of an app that depends on a library
/// `Util` of `lib` as `src/Lib.mr`, whatever the front end reports about
/// them; and the library.
fn using_util(lib: &str, program: &str) -> (Source, Project) {
    let db = TestDb::new();
    let util = db.project("util", ProjectKind::Lib, &[("src/Lib.mr", lib)], Vec::new());
    let project = db.project(
        "users-api",
        ProjectKind::App,
        &[("src/Program.mr", program)],
        vec![Dependency {
            name: "Util".to_owned(),
            project: util,
        }],
    );
    let file = file_named(&db, project, "src/Program.mr");
    (Source { db, project, file }, util)
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
    ) -> Result<ConstValue, Failure> {
        let owner = BodyOwner::Item(self.item(name).item);
        let key = ConstKey::Constant {
            file: self.file,
            owner,
        };
        let mir = eval_mir(&self.db, self.project, key);
        run(&self.db, &mir.bodies[0], &initializers(initializer))
    }

    /// What a run of the initializer of the namespace-level constant
    /// `name` gives, when it calls no field initializer.
    fn run(&self, name: &str) -> Result<ConstValue, Failure> {
        self.run_with(name, &mut |item, field| {
            panic!("no initializer is called: {item:?} {field}")
        })
    }

    /// The failure that stops the run of the constant `name`.
    fn failure(&self, name: &str) -> Failure {
        match self.run(name) {
            Err(failure) => failure,
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

/// The values of a run that calls field initializers and reads no
/// constant: each initializer from `initializer`.
fn initializers(
    initializer: &mut dyn FnMut(ItemId, u32) -> Option<ConstValue>,
) -> impl Fn(Project, ConstKey) -> Option<ConstValue> + '_ {
    let initializer = std::cell::RefCell::new(initializer);
    move |_, key| match key {
        ConstKey::Initializer { item, field } => (initializer.borrow_mut())(item, field),
        ConstKey::Constant { .. } | ConstKey::Default { .. } => panic!("a read of {key:?}"),
    }
}

/// What a run of a hand-built body gives, with no field initializer.
fn run_hand_built(body: &Body) -> Result<ConstValue, Failure> {
    let db = TestDb::new();
    run(&db, body, &|_, _| None)
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
        "const A: int = 1;\nconst B: int = 2;\nstruct P {\n    public Y: int;\n    public static S: int = 4;\n}\nstruct R(V: int);\nstruct Q(V: int, W: int);\nconst T: (int, int) = (A, (long)B > 0 ? -A : B);\nconst M: int = (new P { Y = A }).Y;\nconst N: R = new R(B);\nconst S2: int = P.S + 1;\nconst N2: Q = new Q(B + 1, 2);\n",
    );
    let (a, b) = (source.constant("A"), source.constant("B"));
    assert_eq!(
        source.reads(source.constant("T")),
        vec![(a, "A"), (b, "B"), (a, "A"), (b, "B")]
    );
    assert_eq!(source.reads(source.constant("M")), vec![(a, "A")]);
    // The creation of a positional struct waits, and so does what it holds.
    assert_eq!(source.reads(source.constant("N")), vec![]);
    // What a creation that waits holds is listed in pre-order.
    assert_eq!(
        source.visited(source.constant("N2")),
        vec!["new Q(B + 1, 2)", "B + 1", "B", "1", "2"]
    );
    // `P.S` names a static, which waits and is not read; `P`, which it
    // holds, is listed without being checked.
    assert_eq!(source.reads(source.constant("S2")), vec![]);
    assert_eq!(
        source.visited(source.constant("S2")),
        vec!["P.S + 1", "P.S", "P", "1"]
    );
}

#[test]
fn a_scan_stops_at_constructs_a_later_milestone_lowers() {
    let source = unchecked(
        "const A: int = 1;\nconst S: string = \"ab\";\nconst I: char = S[A];\n\
         const R: int = Get(A)?;\nconst W: int = await Get(A);\nconst G: int = (A..A).Length;\n\
         Arr(): int[] => Todo();\nconst X: int = Arr()[A];\n",
    );
    for name in ["I", "R", "W", "G"] {
        let key = source.constant(name);
        assert_eq!(source.reads(key), vec![], "{name}");
        assert_eq!(source.unsupported(key), None, "{name}");
    }
    // An element of an array is no construct of its own: the scan goes on
    // into its base.
    let x = source.constant("X");
    assert_eq!(source.unsupported(x), Some(("Arr()", "a call")));
}

#[test]
fn a_construct_that_waits_lists_the_parts_its_milestone_will_scan() {
    let source = unchecked(
        "const A: int = 1;\nconst B: int = 2;\nconst W: Q = O with { X = A };\n\
         const I: bool = A is 0;\nconst S: J = A as J;\nconst T: int = await A;\n\
         const R: int = A?;\nconst G: int = F(A..B);\nconst C: int[] = [A, B];\n\
         const N: int[] = new int[B];\n",
    );
    let cases: [(&str, &str, &[&str]); 8] = [
        ("W", "O with { X = A }", &["O", "A"]),
        ("I", "A is 0", &["A"]),
        ("S", "A as J", &["A"]),
        ("T", "await A", &["A"]),
        ("R", "A?", &["A"]),
        ("G", "A..B", &["A", "B"]),
        ("C", "[A, B]", &["A", "B"]),
        ("N", "new int[B]", &["B"]),
    ];
    for (name, text, parts) in cases {
        let owner = source.constant(name).owner(&source.db);
        let hir = body(&source.db, source.file, owner);
        let map = body_source_map(&source.db, source.file, owner);
        let expr = source.expr_at(owner, source.range_in(text, text));
        let found: Vec<&str> = children(&hir.exprs[expr])
            .into_iter()
            .map(|part| source.text(map.expr(part)))
            .collect();
        assert_eq!(found, parts, "{text}");
    }
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
        ("F(): int => 1;\nconst A: int = F();\n", "F()", "a call"),
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
    let (source, util) = using_util("public const K: int = 1;\n", "const A: int = Util.K;\n");
    assert_eq!(
        codes(diagnostics(&source.db, source.project)),
        Vec::<String>::new()
    );
    let a = source.constant("A");
    let reads = &const_scan(&source.db, source.project, a).reads;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, util);
    assert_eq!(source.graph().order(), [a]);
}

#[test]
fn a_long_chain_builds_without_recursion() {
    let source = unchecked(&reversed_chain(""));
    let order = source.graph().order();
    assert_eq!(order.len(), CHAIN);
    assert_eq!(order[0], source.constant("C0"));
    assert_eq!(
        order[CHAIN - 1],
        source.constant(&format!("C{}", CHAIN - 1))
    );
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
        Ok(ConstValue::Scalar(Const::String("abcd".into())))
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
fn the_mir_evaluation_runs_reads_constants_without_their_values() {
    let source = checked("const A: int = 1;\nconst B: int = A + 1;\nF(): int => B;\n");
    let mut out = String::new();
    let mir = eval_mir(&source.db, source.project, source.constant("B"));
    crate::write_mir(&mut out, &source.db, Some(source.file), mir);
    let code: Vec<&str> = out
        .lines()
        .filter_map(|line| line.split_once(" // "))
        .map(|(code, _)| code.trim())
        .collect();
    assert_eq!(code, ["_0 = read A + 1;", "return;"]);
    // An ordinary body reads the value.
    assert_eq!(source.code("F"), ["_0 = 2;", "return;"]);
}

#[test]
fn run_runs_a_default_value_with_its_conversion() {
    let source = checked("O(x: long = (int)((1L > 0L) ? 2 : 3)) {}\n");
    let ConstKey::Default { owner, default, .. } = source.only_default() else {
        panic!("a default value");
    };
    let key = ConstKey::Default {
        file: source.file,
        owner,
        default,
    };
    let mir = eval_mir(&source.db, source.project, key);
    assert_eq!(
        run(&source.db, &mir.bodies[0], &|_, _| None),
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
                ConstValue::Scalar(Const::String("a".into()))
            ]
        ))
    );
    assert_eq!(
        source.run("U"),
        Ok(ConstValue::Scalar(Const::String("b".into())))
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
#[should_panic(
    expected = "internal error evaluating UsersApi.C: a field initializer without a value"
)]
fn run_panics_on_an_initializer_without_a_value() {
    let source = checked(
        "struct P {\n    public X: int = 1;\n    public Y: int;\n}\n\
         const C: P = new P { Y = 2 };\n",
    );
    let _ = source.run_with("C", &mut |_, _| None);
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
#[should_panic(expected = "internal error evaluating Hand: `+` on")]
fn run_names_its_body_when_an_operation_takes_mismatched_types() {
    let long = Operand::Const(Const::Int(1, Primitive::Long));
    let _ = run_hand_built(&hand_built(
        1,
        vec![block(
            vec![to_return(Rvalue::Binary(BinOp::Add, int_operand(1), long))],
            TerminatorKind::Return,
        )],
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
    let value = run(
        &source.db,
        &body,
        &initializers(&mut |item, field| {
            seen.push((item, field));
            Some(int(9, Primitive::Int))
        }),
    );
    assert_eq!(seen, [(p, 3)]);
    assert_eq!(value, Ok(int(9, Primitive::Int)));
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: _1 has no value")]
fn run_drops_an_initializer_without_a_destination() {
    let source = checked("struct P {}\n");
    let body = initializer_call(source.item("P"), None, Some(BlockId(1)));
    let _ = run(&source.db, &body, &|_, _| Some(int(9, Primitive::Int)));
}

#[test]
#[should_panic(expected = "internal error evaluating Hand: a call that does not return")]
fn run_panics_on_an_initializer_that_does_not_return() {
    let source = checked("struct P {}\n");
    let body = initializer_call(source.item("P"), Some(local(1)), None);
    let _ = run(&source.db, &body, &|_, _| Some(int(9, Primitive::Int)));
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
fn the_unit_value_compares_equal_however_it_is_written() {
    let cases = [
        "const U: () = ();\nconst B: bool = U == ();\n",
        "const U: () = ();\nconst T: ((), int) = (U, 1);\nconst B: bool = T == ((), 1);\n",
        "struct P {\n    public X: () = ();\n}\nconst Q: P = new P {};\n\
         const B: bool = Q == new P { X = () };\n",
    ];
    for text in cases {
        let source = checked(text);
        assert_eq!(
            source.evaluated(source.constant("B")),
            &Evaluated::Value(boolean(true)),
            "{text}"
        );
    }
}

#[test]
fn the_unit_value_is_one_scalar() {
    let source = checked("const U: () = ();\nconst T: ((), int) = ((), 1);\n");
    let unit = ConstValue::Scalar(Const::Unit);
    assert_eq!(
        source.evaluated(source.constant("U")),
        &Evaluated::Value(unit.clone())
    );
    assert_eq!(
        source.evaluated(source.constant("T")),
        &Evaluated::Value(ConstValue::Aggregate(
            AggregateKind::Tuple,
            vec![unit, int(1, Primitive::Int)]
        ))
    );
}

#[test]
fn copies_of_a_string_constant_share_its_text() {
    let mut text = String::from("const A0: string = \"a\";\n");
    for n in 1..=20 {
        let m = n - 1;
        text.push_str(&format!("const A{n}: string = A{m} + A{m};\n"));
    }
    for n in 0..200 {
        text.push_str(&format!("const B{n}: string = A20;\n"));
    }
    let source = checked(&text);
    let data = |name: &str| match source.evaluated(source.constant(name)) {
        Evaluated::Value(ConstValue::Scalar(Const::String(text))) => {
            assert_eq!(text.len(), 1 << 20);
            text.as_ptr()
        }
        other => panic!("{name} gives {other:?}"),
    };
    let shared = data("A20");
    for n in 0..200 {
        assert_eq!(data(&format!("B{n}")), shared, "B{n}");
    }
}

#[test]
fn run_gives_the_lengths_of_a_concatenation_too_long_to_keep() {
    let long = "a".repeat(MAX_STRING_BYTES - 1);
    let string = |text: &str| Operand::Const(Const::String(text.into()));
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
    let Err(failure) = run_hand_built(&body) else {
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
fn a_comparison_that_calls_a_declared_equals_waits() {
    let source = checked(
        "struct P : IEquatable<P> {\n    public X: int = 1;\n    public Y: int = 2;\n    \
         public Equals(in other: P): bool => X == other.X;\n}\n\
         struct Q {\n    public p: P;\n}\n\
         const S: bool = new P {} == new P { Y = 3 };\n\
         const N: bool = new P {} != new P { Y = 3 };\n\
         const T: bool = (new P {}, 1) == (new P { Y = 3 }, 1);\n\
         const U: bool = new Q { p = new P {} } == new Q { p = new P { Y = 3 } };\n",
    );
    for name in ["S", "N", "T", "U"] {
        assert_eq!(
            outcome(source.evaluated(source.constant(name))),
            "a comparison that calls a declared `Equals` M3.10",
            "{name}"
        );
    }
}

#[test]
fn an_equals_that_is_not_the_equality_of_its_struct_leaves_a_comparison_alone() {
    // None of them takes one `P` and returns `bool`, so `==` compares field
    // by field.
    for equals in [
        "public Equals(x: int): bool => X == x;",
        "public Equals(in other: int): bool => X == other;",
        "public Equals(in other: P): int => 1;",
        "public Equals(in other: P, x: int): bool => true;",
    ] {
        let source = checked(&format!(
            "struct P {{\n    public X: int = 1;\n    {equals}\n}}\n\
             const S: bool = new P {{}} == new P {{}};\n"
        ));
        assert_eq!(
            source.evaluated(source.constant("S")),
            &Evaluated::Value(boolean(true)),
            "{equals}"
        );
    }
}

#[test]
fn an_equals_that_takes_its_struct_in_any_mode_waits() {
    let source = checked(
        "struct P {\n    public X: int = 1;\n    public Equals(other: P): bool => true;\n}\n\
         const S: bool = new P {} == new P {};\n",
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("S"))),
        "a comparison that calls a declared `Equals` M3.10"
    );
}

impl Source {
    /// What each key of the file named in `names` gives, as [`outcome`]
    /// says, by the name the key's project gives it.
    fn outcomes(&self, names: &[&str]) -> Vec<String> {
        let mut keys = constants(&self.db, self.file);
        keys.extend(defaults(&self.db, self.project, self.file));
        names
            .iter()
            .map(|&name| {
                let key = keys
                    .iter()
                    .copied()
                    .find(|key| key.name(&self.db, self.project) == name)
                    .unwrap_or_else(|| panic!("no key {name}"));
                outcome(self.evaluated(key))
            })
            .collect()
    }
}

#[test]
fn a_key_a_walk_evaluates_early_keeps_its_first_result() {
    // `S` reaches `C` through the default of `G` before `C`'s turn, while
    // the default of `F` that `C` leaves out is not known yet.
    let rest = "const C: int = match (true) { _ => F() };\nconst E: int = default;\n\
                F(x: int = C + E): int => x;\nG(y: int = C): int => y;\n";
    let names = ["C", "the default value of y in G"];
    let expected = ["a `match` M3.3", "a constant M3.3"];
    assert_eq!(checked(rest).outcomes(&names), expected);
    let with_s = format!("const S: int = match (true) {{ _ => G() }};\n{rest}");
    assert_eq!(checked(&with_s).outcomes(&names), expected);
    // The same when the default of `F` reads `C` alone.
    let plain = "const S: int = match (true) { _ => G() };\n\
                 const C: int = match (true) { _ => F() };\n\
                 F(x: int = C): int => x;\nG(y: int = C): int => y;\n";
    assert_eq!(checked(plain).outcomes(&names), expected);
}

#[test]
fn an_initializer_a_walk_evaluates_early_keeps_its_first_result() {
    // `A` reaches `P.X` through the default of `G` before the turn of
    // `P.X`, while the default of `F` that `P.X` leaves out is not known yet.
    let text = "const A: int = match (true) { _ => G() };\n\
                G(y: int = new P {}.X): int => y;\n\
                struct P {\n    public X: int = match (true) { _ => F() };\n}\n\
                F(x: int = new P {}.X + B): int => x;\nconst B: int = default;\n";
    let source = checked(text);
    let x = ConstKey::Initializer {
        item: source.item("P"),
        field: 0,
    };
    assert_eq!(outcome(source.evaluated(x)), "a `match` M3.3");
}

#[test]
fn a_call_of_the_library_waits_for_its_signatures() {
    // Whether a library function is generic is unknown until the library
    // has signatures, so no library call is `MR0460` yet.
    for call in [
        "Math.Max(1, 2)",
        "Math.Max<int>(1, 2)",
        "List.Empty<int>()",
        "Std.Math.Max(1, 2)",
    ] {
        let source = checked(&format!("const X: int = {call};\n"));
        assert_eq!(
            outcome(source.evaluated(source.constant("X"))),
            "a value of the standard library M1.15",
            "{call}"
        );
    }
    let source = checked(
        "F(): int => 1;\nMax<T>(a: T, b: T): T => a;\n\
         const X: int = F();\nconst Y: int = Max(1, 2);\n",
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("X"))),
        "unsupported a call"
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("Y"))),
        "a call of a generic function M3.9"
    );
}

#[test]
fn a_cut_cycle_inside_a_walk_keeps_the_frames_before_the_cut() {
    // The walk of `S` reaches the cycle `K -> K2 -> F's default -> K` at
    // its third frame, and the cycle is cut at its second edge.
    let text = "const S: int = match (true) { _ => G() };\nG(y: int = K): int => y;\n\
                const K: int = K2;\nconst K2: int = match (true) { _ => F() };\n\
                F(x: int = K): int => x;\n";
    let names = [
        "S",
        "the default value of y in G",
        "K",
        "K2",
        "the default value of x in F",
    ];
    assert_eq!(
        checked(text).outcomes(&names),
        [
            "a call that leaves out a default that needs evaluation M3.3",
            "a constant M3.3",
            "a constant M3.3",
            "a `match` M3.3",
            "a constant M3.3",
        ]
    );
}

#[test]
fn an_unsupported_constant_runs_no_initializer_it_reads() {
    let lib = util_struct("    public X: int = Zero();\n");
    let (source, _) = using_util(&lib, "const C: int = new Util.P {}.X + Util.Zero();\n");
    let reported: Vec<_> = crate::diagnostics(&source.db, source.project)
        .iter()
        .map(|diagnostic| (diagnostic.code.to_string(), diagnostic.primary.span.file))
        .collect();
    assert_eq!(
        reported,
        [("MR0460".to_owned(), source.file.id(&source.db))]
    );
}

/// Checks that each key in `names` of `text` gives what `expected` says,
/// as [`outcome`] writes it, and that evaluation reports nothing.
fn waits_quietly(text: &str, names: &[&str], expected: &[&str]) {
    let source = checked(text);
    assert_eq!(source.outcomes(names), expected, "{text}");
    assert_eq!(source.reported(), [], "{text}");
}

#[test]
fn a_read_of_a_constant_of_a_class_waits_for_classes() {
    let class = "class C {\n    public const K: int = 1;\n}\n";
    waits_quietly(
        &format!("{class}const A: int = C.K + 1;\nF(x: int = C.K): int => x;\n"),
        &["A", "the default value of x in F"],
        &["a constant M2.1", "a constant M2.1"],
    );
    waits_quietly(
        &format!("{class}const A: int = C.K + B;\nconst B: int = 2;\n"),
        &["A", "B"],
        &["a constant M2.1", "value"],
    );
}

#[test]
fn a_read_of_a_constant_of_a_library_class_waits_in_both_projects() {
    let lib = "public class C {\n    public const K: int = 1;\n}\n";
    let (source, util) = using_util(lib, "const A: int = Util.C.K + 1;\n");
    assert_eq!(
        codes(diagnostics(&source.db, source.project)),
        Vec::<String>::new()
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("A"))),
        "a constant M2.1"
    );
    assert_eq!(source.reported(), []);
    assert_eq!(crate::diagnostics(&source.db, util), &[]);
}

#[test]
fn a_read_of_a_constant_of_a_union_waits_for_unions() {
    // Of the types that wait, only a class and a union declare constants:
    // `const` in an enum or an interface is a front-end error.
    waits_quietly(
        "union E {\n    A(X: int);\n    public const K: int = 1;\n}\n\
         const A: int = E.K + 1;\nF(x: int = E.K): int => x;\n",
        &["A", "the default value of x in F"],
        &["a constant M3.1", "a constant M3.1"],
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

/// A diagnostic of constant evaluation, as the tests compare it.
#[derive(Debug, PartialEq, Eq)]
struct Reported {
    /// Its code.
    code: String,
    /// Its message.
    message: String,
    /// The text its label points at.
    at: String,
    /// Its label.
    label: Option<String>,
    /// Its note.
    note: Option<String>,
}

/// The note of every `MR0460`.
const UNSUPPORTED_NOTE: &str = "constant evaluation runs literals, operators, casts, other constants and the creation of structs and tuples; a value computed when the program runs can be a `static lazy`";

/// The note of every `MR0461`.
const FAILED_NOTE: &str = "a constant is computed when the program is compiled, where an operation that would panic at run time is an error in every build profile";

impl Source {
    /// The diagnostics of constant evaluation in the project, each in the
    /// file of the source.
    fn reported(&self) -> Vec<Reported> {
        self.reported_in(self.file)
    }

    /// The diagnostics of constant evaluation in the project, each in
    /// `file`, which may be a file of another project.
    fn reported_in(&self, file: SourceFile) -> Vec<Reported> {
        let text = file.text(&self.db);
        crate::diagnostics(&self.db, self.project)
            .iter()
            .map(|diagnostic| {
                assert_eq!(diagnostic.primary.span.file, file.id(&self.db));
                assert_eq!(diagnostic.secondary, []);
                assert_eq!(diagnostic.fixes, []);
                assert_eq!(diagnostic.severity, mora_diag::Severity::Error);
                Reported {
                    code: diagnostic.code.to_string(),
                    message: diagnostic.message.clone(),
                    at: text[std::ops::Range::<usize>::from(diagnostic.primary.span.range)]
                        .to_owned(),
                    label: diagnostic.primary.message.clone(),
                    note: diagnostic.note.clone(),
                }
            })
            .collect()
    }
}

/// A diagnostic that `reported` gives.
fn reported(code: &str, message: &str, at: &str, label: &str, note: &str) -> Reported {
    Reported {
        code: code.to_owned(),
        message: message.to_owned(),
        at: at.to_owned(),
        label: Some(label.to_owned()),
        note: Some(note.to_owned()),
    }
}

#[test]
fn diagnostics_render_each_code() {
    let source = checked("Zero(): int => 0;\nconst A: int = Zero();\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0460",
            "a call cannot be evaluated at compile time",
            "Zero()",
            "evaluated at compile time",
            UNSUPPORTED_NOTE,
        )]
    );

    let source = checked("const Big: int = 2_000_000_000;\nconst Twice: int = Big * 2;\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0461",
            "`Big * 2` overflows `int`: 2000000000 * 2",
            "Big * 2",
            "evaluated at compile time",
            FAILED_NOTE,
        )]
    );

    let source = unchecked("const A: int = B + 1;\nconst B: int = A + 1;\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0462",
            "constant `A` depends on its own value",
            "B",
            "this needs the value of `B`",
            "the cycle is A -> B -> A",
        )]
    );
}

#[test]
fn a_cycle_of_one_needs_its_own_value() {
    let source = unchecked("const A: int = A + 1;\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0462",
            "constant `A` depends on its own value",
            "A",
            "this needs the value of `A`",
            "the cycle is A -> A",
        )]
    );
}

#[test]
fn a_cycle_that_starts_at_an_initializer_names_it() {
    let source = unchecked(
        "struct P {\n    public X: int = Q.Y;\n}\nstruct Q {\n    public Y: int = P.X;\n}\n",
    );
    // No constant creates `P` or `Q`, so the initializers are not in the
    // graph; a constant that creates one brings its cycle in.
    assert_eq!(source.reported(), []);

    let source =
        unchecked("struct P {\n    public X: int = new P {}.X;\n}\nconst C: int = new P {}.X;\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0462",
            "the initializer of `P.X` depends on its own value",
            "new P {}",
            "this needs the value of `P.X`",
            "the cycle is P.X -> P.X",
        )]
    );
}

/// A library `Util` whose struct `P` has the fields of `fields`, none of
/// which its own constants create.
fn util_struct(fields: &str) -> String {
    format!("public Zero(): int => 0;\npublic struct P {{\n{fields}}}\n")
}

#[test]
fn an_initializer_of_another_project_reports_its_error_in_its_file() {
    let lib = util_struct("    public X: int = Zero();\n");
    let (source, util) = using_util(&lib, "const C: Util.P = new Util.P {};\n");
    assert_eq!(
        codes(diagnostics(&source.db, source.project)),
        Vec::<String>::new()
    );
    let lib = file_named(&source.db, util, "src/Lib.mr");
    let note = format!(
        "{UNSUPPORTED_NOTE}\n`P.X` runs at compile time because a `P` created at compile time leaves `X` out"
    );
    assert_eq!(
        source.reported_in(lib),
        [reported(
            "MR0460",
            "a call cannot be evaluated at compile time",
            "Zero()",
            "evaluated at compile time",
            &note,
        )]
    );
    // Its cause is reported, so the constant fails without an error.
    assert_eq!(
        source.evaluated(source.constant("C")),
        &Evaluated::Failed(None)
    );
    // The library's own evaluation never runs it.
    assert_eq!(crate::diagnostics(&source.db, util), &[]);
}

#[test]
fn initializers_of_another_project_that_read_each_other_are_a_cycle() {
    let cases = [
        (
            "    public X: int = new P {}.X;\n",
            "new P {}",
            "P.X",
            "the cycle is P.X -> P.X",
        ),
        (
            "    public X: int = new P { X = 1 }.Y;\n    public Y: int = new P { Y = 1 }.X;\n",
            "new P { X = 1 }",
            "P.Y",
            "the cycle is P.X -> P.Y -> P.X",
        ),
    ];
    for (fields, at, read, note) in cases {
        let lib = util_struct(fields);
        let (source, util) = using_util(&lib, "const C: Util.P = new Util.P {};\n");
        assert_eq!(
            codes(diagnostics(&source.db, source.project)),
            Vec::<String>::new()
        );
        let lib = file_named(&source.db, util, "src/Lib.mr");
        assert_eq!(
            source.reported_in(lib),
            [reported(
                "MR0462",
                "the initializer of `P.X` depends on its own value",
                at,
                &format!("this needs the value of `{read}`"),
                note,
            )],
            "{fields}"
        );
        assert_eq!(
            source.evaluated(source.constant("C")),
            &Evaluated::Failed(None)
        );
    }
}

#[test]
fn an_initializer_two_projects_run_is_reported_by_the_one_the_other_depends_on() {
    let db = TestDb::new();
    let lib = util_struct("    public X: int = Zero();\n");
    let util = db.project(
        "util",
        ProjectKind::Lib,
        &[("src/Lib.mr", &lib)],
        Vec::new(),
    );
    let on = |name: &str, project| Dependency {
        name: name.to_owned(),
        project,
    };
    let middle = db.project(
        "middle",
        ProjectKind::Lib,
        &[("src/Middle.mr", "public const D: Util.P = new Util.P {};\n")],
        vec![on("Util", util)],
    );
    // `Top` runs nothing; the app reaches `Middle` through it.
    let top = db.project(
        "top",
        ProjectKind::Lib,
        &[("src/Top.mr", "public const T: int = 1;\n")],
        vec![on("Util", util), on("Middle", middle)],
    );
    let program = "const C: Util.P = new Util.P {};\n";
    let direct = db.project(
        "users-api",
        ProjectKind::App,
        &[("src/Program.mr", program)],
        vec![on("Util", util), on("Middle", middle)],
    );
    let through_top = db.project(
        "other-api",
        ProjectKind::App,
        &[("src/Program.mr", program)],
        vec![on("Util", util), on("Top", top)],
    );
    let codes_of = |project| codes(crate::diagnostics(&db, project));
    assert_eq!(codes_of(middle), ["MR0460"]);
    assert_eq!(codes_of(direct), Vec::<String>::new());
    assert_eq!(codes_of(through_top), Vec::<String>::new());
    assert_eq!(codes_of(top), Vec::<String>::new());
    assert_eq!(codes_of(util), Vec::<String>::new());
}

#[test]
fn a_default_that_reads_the_constant_whose_match_leaves_it_out_waits() {
    let cases = [
        (
            "const A: int = match (true) { _ => F() };\nF(x: int = A): int => x;\n",
            "a constant M3.3",
        ),
        (
            "struct P {\n    public X: int = match (true) { _ => F() };\n}\n\
             F(x: int = new P {}.X): int => x;\nconst A: int = new P {}.X;\n",
            "a `match` M3.3",
        ),
    ];
    // The call is inside a `match`, so the constant waits whatever the
    // default gives, and the default waits for what it reads.
    for (text, waits) in cases {
        let source = checked(text);
        assert_eq!(source.reported(), [], "{text}");
        assert_eq!(
            outcome(source.evaluated(source.constant("A"))),
            "a `match` M3.3",
            "{text}"
        );
        let default = source.only_default();
        assert_eq!(outcome(source.evaluated(default)), waits, "{text}");
    }
}

#[test]
fn a_failed_constant_is_reported_once() {
    let source = checked(
        "const A: int = 2_000_000_000;\nconst B: int = A * 2;\nconst C: int = B;\n\
         const D: int = B + C;\nF(x: int = B) {}\nG(): int => B;\n",
    );
    let found = source.reported();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, "MR0461");
    assert_eq!(found[0].at, "A * 2");
}

#[test]
fn diagnostics_follow_the_order_of_the_graph() {
    // The initializer of `P.X` comes before `C`, which reads it, though
    // its text comes after `D`.
    let source = checked(
        "const C: P = new P {};\nconst D: int = Zero();\nZero(): int => 0;\n\
         struct P {\n    public X: int = Zero();\n}\nconst Big: int = 2_000_000_000;\nconst E: int = Big * 2;\n",
    );
    let found: Vec<(String, usize)> = crate::diagnostics(&source.db, source.project)
        .iter()
        .map(|d| {
            (
                d.code.to_string(),
                usize::from(d.primary.span.range.start()),
            )
        })
        .collect();
    let text = source.file.text(&source.db);
    let in_p = text.find("= Zero();\n}").expect("the initializer") + 2;
    let in_d = text.find("= Zero();\nZero").expect("the constant") + 2;
    let in_e = text.find("Big * 2").expect("the product");
    assert_eq!(
        found,
        [
            ("MR0460".to_owned(), in_p),
            ("MR0460".to_owned(), in_d),
            ("MR0461".to_owned(), in_e),
        ]
    );
}

#[test]
fn a_constant_that_waits_reports_nothing() {
    let source = checked("class C {}\nF(c: C = new C()) {}\n");
    assert_eq!(source.reported(), []);
}

/// 256 KiB is enough for the front end and for evaluation one key at a
/// time, and far too little for 2,000 nested evaluations.
const SMALL_STACK: usize = 256 * 1024;

/// The length of the long chains of constants.
const CHAIN: usize = 2_000;

/// A chain of `CHAIN` constants, `C0` to `C1999`, each but `C0` reading the
/// one before it, the last one first in the text; each declared with
/// `modifier` in front.
fn reversed_chain(modifier: &str) -> String {
    (0..CHAIN)
        .rev()
        .map(|i| {
            if i == 0 {
                format!("{modifier}const C0: int = 1;\n")
            } else {
                format!("{modifier}const C{i}: int = C{} + 1;\n", i - 1)
            }
        })
        .collect()
}

/// Runs `test` on a thread with [`SMALL_STACK`].
fn on_a_small_stack(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(test)
        .expect("a thread")
        .join()
        .expect("no overflow");
}

#[test]
fn a_long_chain_lowers_without_overflowing_the_stack() {
    on_a_small_stack(|| {
        let source = unchecked(&reversed_chain(""));
        // Lowering comes first: nothing has evaluated the chain in order.
        let dump = crate::mir_dump(&source.db, source.project, source.file);
        assert!(dump.contains("C1999"), "{dump}");
        assert_eq!(
            source.evaluated(source.constant(&format!("C{}", CHAIN - 1))),
            &Evaluated::Value(int(2_000, Primitive::Int))
        );
    });
}

#[test]
fn a_long_chain_in_another_project_lowers_without_overflowing_the_stack() {
    on_a_small_stack(|| {
        let program = "const A: int = Util.C1999 + 1;\n";
        let (source, _) = using_util(&reversed_chain("public "), program);
        // Lowering comes first: nothing has evaluated the chain in order.
        let dump = crate::mir_dump(&source.db, source.project, source.file);
        assert!(dump.contains("2001"), "{dump}");
        assert_eq!(
            source.evaluated(source.constant("A")),
            &Evaluated::Value(int(2_001, Primitive::Int))
        );
    });
}

#[test]
fn a_long_chain_of_constants_does_not_overflow_the_stack() {
    on_a_small_stack(|| {
        // The last constant comes first in the text, so a walk in text
        // order would evaluate the whole chain inside its first key.
        let source = unchecked(&reversed_chain(""));
        assert_eq!(source.reported(), []);
        assert_eq!(
            source.evaluated(source.constant(&format!("C{}", CHAIN - 1))),
            &Evaluated::Value(int(2_000, Primitive::Int))
        );
    });
}

#[test]
fn editing_a_file_keeps_reverification_shallow() {
    use salsa::Setter;

    on_a_small_stack(|| {
        let db = TestDb::new();
        let chain = reversed_chain("");
        let files = [
            ("src/Chain.mr", chain.as_str()),
            ("src/Other.mr", "F(): int => 1;\n"),
        ];
        let project = app(&db, &files);
        let file = file_named(&db, project, "src/Chain.mr");
        let other = file_named(&db, project, "src/Other.mr");
        let mut source = Source { db, project, file };
        assert_eq!(source.reported(), []);
        other
            .set_text(&mut source.db)
            .to("F(): int => 2;\n".to_owned());
        // Every memo of the chain is checked again after the edit.
        let middle = source.constant(&format!("C{}", CHAIN / 2));
        assert_eq!(
            source.evaluated(middle),
            &Evaluated::Value(int(1_001, Primitive::Int))
        );
        let dump = crate::mir_dump(&source.db, source.project, source.file);
        assert!(dump.contains("2000"), "{dump}");
    });
}

#[test]
fn editing_a_reader_of_another_project_keeps_reverification_shallow() {
    use salsa::Setter;

    on_a_small_stack(|| {
        let program = "const A: int = Util.C1999 + 1;\n";
        let (mut source, util) = using_util(&reversed_chain("public "), program);
        assert_eq!(source.reported(), []);
        source
            .file
            .set_text(&mut source.db)
            .to("const A: int = Util.C1999 + 2;\n".to_owned());
        assert_eq!(
            source.evaluated(source.constant("A")),
            &Evaluated::Value(int(2_002, Primitive::Int))
        );
        let lib = file_named(&source.db, util, "src/Lib.mr");
        let dump = crate::mir_dump(&source.db, util, lib);
        assert!(dump.contains("2000"), "{dump}");
    });
}

#[test]
fn an_initializer_error_names_the_creation_that_leaves_its_field_out() {
    let p = "Zero(): int => 0;\nstruct P {\n    public X: int = Zero();\n}\n";
    // A constant creates the struct, then a default value does.
    for creation in [
        "const C: P = new P {};\n",
        "F(p: P = new P {}): int => 1;\nConsole.WriteLine(F());\n",
    ] {
        let source = checked(&format!("{p}{creation}"));
        assert_eq!(
            source.reported(),
            [reported(
                "MR0460",
                "a call cannot be evaluated at compile time",
                "Zero()",
                "evaluated at compile time",
                &format!(
                    "{UNSUPPORTED_NOTE}\n`P.X` runs at compile time because a `P` created at \
                     compile time leaves `X` out"
                ),
            )],
            "{creation}"
        );
    }
}

#[test]
fn each_failed_operation_has_its_message() {
    let failure = |error| Failure {
        range: TextRange::empty(TextSize::new(0)),
        error,
        ty: Ty::Primitive(Primitive::Int),
        operation: "1 / 0".to_owned(),
    };
    let messages: Vec<String> = [
        ArithError::Overflow,
        ArithError::DivisionByZero,
        ArithError::RemainderByZero,
        ArithError::TargetDependent,
        ArithError::TooLong(16_777_217),
    ]
    .into_iter()
    .map(|error| failed_message("X / Y", &failure(error), "nint"))
    .collect();
    assert_eq!(
        messages,
        [
            "`X / Y` overflows `nint`: 1 / 0",
            "`X / Y` divides by zero: 1 / 0",
            "`X / Y` takes a remainder of a division by zero: 1 / 0",
            "`X / Y` gives a `nint` that is not the same on every target: 1 / 0",
            "`X / Y` makes a string of 16777217 bytes, more than a constant can hold",
        ]
    );
}

#[test]
fn a_division_by_zero_names_the_division() {
    let source = checked("const Z: int = 0;\nconst Q: int = 7 % Z;\n");
    assert_eq!(
        source.reported(),
        [reported(
            "MR0461",
            "`7 % Z` takes a remainder of a division by zero: 7 % 0",
            "7 % Z",
            "evaluated at compile time",
            FAILED_NOTE,
        )]
    );
}

#[test]
fn a_construct_that_waits_holds_no_error_and_no_read() {
    let p =
        "struct P {\n    public X: int;\n    public P(x: int) { X = x; }\n}\nZero(): int => 0;\n";
    let q = "struct Q {\n    public X: int = 1;\n}\nZero(): int => 0;\n";
    let generic = "Zero(): int => 0;\nMax<T>(a: T, b: T): T => a;\n";
    let constructor = "a constructor call M2.1";
    let cases: [(String, &[&str], &[&str]); 11] = [
        (
            format!("{p}const C: P = new P(Zero());\n"),
            &["C"],
            &[constructor],
        ),
        (
            format!("{p}const C: P = new P(A);\nconst A: int = C.X;\n"),
            &["C", "A"],
            &[constructor, "a constant M2.1"],
        ),
        (
            format!("{p}F(p: P = new P(Zero())): int => 1;\n"),
            &["the default value of p in F"],
            &[constructor],
        ),
        (
            format!(
                "{p}struct R {{\n    public p: P = new P(Zero());\n}}\nconst C: R = new R {{}};\n"
            ),
            &["C"],
            &[constructor],
        ),
        (
            format!(
                "{p}struct R {{\n    public p: P = new P(C.p.X);\n}}\nconst C: R = new R {{}};\n"
            ),
            &["C"],
            &[constructor],
        ),
        (
            format!("{q}const O: Q = new Q {{}};\nconst C: Q = O with {{ X = Zero() }};\n"),
            &["C"],
            &["`with` M2.2"],
        ),
        (
            format!(
                "{q}const C: Q = O with {{ X = A }};\nconst O: Q = new Q {{}};\nconst A: int = C.X;\n"
            ),
            &["C", "A"],
            &["`with` M2.2", "a constant M2.2"],
        ),
        (
            "Zero(): int => 0;\nconst C: bool = Zero() is 0;\n".to_owned(),
            &["C"],
            &["an `is` test M3.3"],
        ),
        (
            "const C: bool = A is 0;\nconst A: int = C ? 1 : 0;\n".to_owned(),
            &["C", "A"],
            &["an `is` test M3.3", "a constant M3.3"],
        ),
        (
            format!("{generic}const C: int = Max(Zero(), 1);\n"),
            &["C"],
            &["a call of a generic function M3.9"],
        ),
        (
            format!("{generic}const C: int = Max(A, 1);\nconst A: int = C;\n"),
            &["C", "A"],
            &["a call of a generic function M3.9", "a constant M3.9"],
        ),
    ];
    for (text, names, expected) in cases {
        waits_quietly(&text, names, expected);
    }
}

#[test]
fn a_creation_of_a_type_that_waits_holds_no_error_and_no_read() {
    let cl =
        "class Cl {\n    public X: int;\n    public Cl(x: int) { X = x; }\n}\nZero(): int => 0;\n";
    let un = "union Shape {\n    Circle(r: int),\n    Square(s: int),\n}\nZero(): int => 0;\n";
    let bx = "struct Box<T> {\n    public V: T;\n    public Box(v: T) { V = v; }\n}\nZero(): int => 0;\n";
    let bm = "struct Box<T> {\n    public V: T;\n}\nZero(): int => 0;\n";
    let po = "struct Po {\n    public X: int;\n    public Y: int?;\n}\nZero(): int => 0;\n";
    let class = "an expression with a class type M2.1";
    let generic = "an expression with a generic type M3.9";
    let fc = "the default value of c in F";
    let cases: [(String, &str, &str); 11] = [
        (
            format!("{cl}const T: int = new Cl(Zero()).X;\n"),
            "T",
            class,
        ),
        (
            format!("{cl}F(c: Cl = new Cl(Zero())): int => 1;\n"),
            fc,
            class,
        ),
        (format!("{cl}const T: int = new Cl(T).X;\n"), "T", class),
        (
            format!("{cl}const T: int = new Cl {{ X = Zero() }}.X;\n"),
            "T",
            class,
        ),
        (
            format!("{cl}F(c: Cl = new Cl {{ X = Zero() }}): int => 1;\n"),
            fc,
            class,
        ),
        (
            format!("{un}const T: Shape = Shape.Circle(Zero());\n"),
            "T",
            "the initializer of a constant with a union type M3.1",
        ),
        (
            format!("{un}F(s: Shape = Shape.Circle(Zero())): int => 1;\n"),
            "the default value of s in F",
            "a union case M3.1",
        ),
        (
            format!("{bx}const T: int = new Box<int>(Zero()).V;\n"),
            "T",
            generic,
        ),
        (
            format!("{bm}const T: int = new Box<int> {{ V = Zero() }}.V;\n"),
            "T",
            generic,
        ),
        (
            format!("{po}const T: int = new Po {{ X = Zero(), Y = none }}.X;\n"),
            "T",
            "an expression with an option type M3.5",
        ),
        (
            format!("{po}F(x: int = new Po {{ X = Zero(), Y = none }}.X): int => 1;\n"),
            "the default value of x in F",
            "an expression with an option type M3.5",
        ),
    ];
    for (text, name, expected) in cases {
        waits_quietly(&text, &[name], &[expected]);
    }
}

#[test]
fn a_default_waits_as_late_as_a_constant_with_its_expression() {
    let decls = "struct P {\n    public X: int;\n    public P(x: int) { X = x; }\n}\n\
                 G<T>(t: T): int => 1;\nMax<T>(a: T, b: T): T => a;\n\
                 struct S {\n    public static V: int = 1;\n}\nenum E { A }\n";
    for value in [
        "new P(G(1))",
        "new P(Max(1, 2))",
        "new P(S.V)",
        "new P((int)E.A)",
    ] {
        let text = format!(
            "{decls}F(p: P = {value}): int => 1;\nH(): int => F();\nconst K: P = {value};\n"
        );
        let source = checked(&text);
        let found = source.outcomes(&["the default value of p in F", "K"]);
        assert_eq!(found[0], found[1], "{value}");
        assert_ne!(found[0], "a constructor call M2.1", "{value}");
        assert_eq!(source.reported(), [], "{value}");
    }
}

#[test]
fn a_constant_whose_owner_waits_waits_before_any_check() {
    let class = "the initializer of a constant of a class M2.1";
    let union = "the initializer of a constant of a union M3.1";
    /// A source, its constants as their type and name, and what each gives.
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);
    let cases: [Case; 5] = [
        (
            "class C {\n    public const K: int = K;\n}\n",
            &[("C", "K")],
            &[class],
        ),
        (
            "Zero(): int => 0;\nclass C {\n    public const K: int = Zero();\n}\n",
            &[("C", "K")],
            &[class],
        ),
        (
            "union U {\n    A(X: int);\n    public const K: int = K;\n}\n",
            &[("U", "K")],
            &[union],
        ),
        (
            "Zero(): int => 0;\nunion U {\n    A(X: int);\n    public const K: int = Zero();\n}\n",
            &[("U", "K")],
            &[union],
        ),
        (
            "class C {\n    public const A: int = B;\n    public const B: int = A;\n}\n",
            &[("C", "A"), ("C", "B")],
            &[class, class],
        ),
    ];
    for (text, keys, expected) in cases {
        let source = checked(text);
        let found: Vec<String> = keys
            .iter()
            .map(|&(ty, name)| outcome(source.evaluated(source.member_constant(ty, name))))
            .collect();
        assert_eq!(found, expected, "{text}");
        assert_eq!(source.reported(), [], "{text}");
    }
    // A constant that reads one of them waits for it, outside any cycle.
    let source = checked("const T: int = C.K;\nclass C {\n    public const K: int = T;\n}\n");
    assert_eq!(
        outcome(source.evaluated(source.member_constant("C", "K"))),
        class
    );
    assert_eq!(
        outcome(source.evaluated(source.constant("T"))),
        "a constant M2.1"
    );
    assert_eq!(source.reported(), []);
}

#[test]
fn a_call_through_a_value_that_waits_still_checks_its_arguments() {
    // Only a call of a union case creates a value of a type that waits; a
    // call of a static that holds a function is a call like any other, so
    // its arguments are checked.
    let id = "Id(x: int): int => x;\nZero(): int => 0;\n";
    for text in [
        format!(
            "{id}struct P {{\n    public static S: ((int) => int) = Id;\n}}\nconst X: int = P.S(Zero());\n"
        ),
        format!("{id}static S: ((int) => int) = Id;\nconst X: int = S(Zero());\n"),
    ] {
        let source = checked(&text);
        assert_eq!(
            source.unsupported(source.constant("X")),
            Some(("Zero()", "a call")),
            "{text}"
        );
    }
}

#[test]
fn an_initializer_of_a_type_that_waits_is_never_run() {
    // Each creation waits for its type before any initializer it leaves
    // out is read, in a constant, a default value or another initializer,
    // and each names the same milestone.
    let z = "Zero(): int => 0;\n";
    let class = "class C {\n    public X: int = Zero();\n}\n";
    let option = "struct O {\n    public X: int = Zero();\n    public Y: int? = none;\n}\n";
    let array = "struct A {\n    public X: int = Zero();\n    public Y: int[] = Todo();\n}\n";
    let generic = "struct G<T> {\n    public X: int = Zero();\n}\n";
    let nested =
        "struct N<T> {\n    public struct In {\n        public X: int = Zero();\n    }\n}\n";
    let cases = [
        (class, "new C {}.X", "an expression with a class type M2.1"),
        (
            option,
            "new O {}.X",
            "an expression with an option type M3.5",
        ),
        (array, "new A {}.X", "an expression with an array type M2.3"),
        (
            generic,
            "new G<int> {}.X",
            "an expression with a generic type M3.9",
        ),
        (
            nested,
            "new N<int>.In {}.X",
            "an expression with a generic type M3.9",
        ),
    ];
    for (decl, value, expected) in cases {
        let text = format!(
            "{z}{decl}const K: int = {value};\nF(x: int = {value}): int => x;\n\
             struct S {{\n    public Y: int = {value};\n}}\nconst L: S = new S {{}};\n"
        );
        waits_quietly(
            &text,
            &["K", "the default value of x in F", "L"],
            &[expected; 3],
        );
    }
    let lib = "public class C {\n    public X: int = Zero();\n}\npublic Zero(): int => 0;\n";
    let (source, util) = using_util(lib, "const K: int = new Util.C {}.X;\n");
    assert_eq!(
        outcome(source.evaluated(source.constant("K"))),
        "an expression with a class type M2.1"
    );
    assert_eq!(source.reported(), []);
    assert_eq!(crate::diagnostics(&source.db, util), &[]);
}
