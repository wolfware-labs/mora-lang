//! Tests of lowering, each from source text through the front end.

use mora_hir::test_support::{TestDb, app, codes, file_named};
use mora_hir::{BodyOwner, diagnostics, item_tree};
use mora_span::{Dependency, Project, ProjectKind, SourceFile};

use crate::{Callee, Const, Mir, Operand, TerminatorKind, mir, mir_dump, validate, write_mir};

/// A file of an app, checked by the front end without a diagnostic.
struct Lowered {
    db: TestDb,
    project: Project,
    file: SourceFile,
}

/// `text` as `src/Program.mr`, alone in an app.
fn lowered(text: &str) -> Lowered {
    let db = TestDb::new();
    let project = app(&db, &[("src/Program.mr", text)]);
    in_project(db, project)
}

fn in_project(db: TestDb, project: Project) -> Lowered {
    let file = file_named(&db, project, "src/Program.mr");
    assert_eq!(
        codes(diagnostics(&db, project)),
        Vec::<String>::new(),
        "{}",
        file.text(&db)
    );
    Lowered { db, project, file }
}

impl Lowered {
    /// The owner of the function `name`, or of the top-level statements
    /// for `top-level`.
    fn owner(&self, name: &str) -> BodyOwner {
        if name == "top-level" {
            return BodyOwner::TopLevel;
        }
        let tree = item_tree(&self.db, self.file);
        let (item, _) = tree
            .items()
            .find(|(_, item)| item.name == name)
            .expect("the function");
        BodyOwner::Item(item)
    }

    fn mir(&self, name: &str) -> &Mir {
        let mir = mir(&self.db, self.project, self.file, self.owner(name));
        assert_eq!(validate(&self.db, mir, false), Ok(()));
        mir
    }

    /// The dump of the body of `name`.
    fn dump(&self, name: &str) -> String {
        let mut out = String::new();
        write_mir(&mut out, &self.db, Some(self.file), self.mir(name));
        out
    }

    /// The statements and terminators of the body of `name`, one per line,
    /// without their spans.
    fn code(&self, name: &str) -> Vec<String> {
        code(&self.dump(name))
    }
}

/// The statements and terminators of a dump, without their spans.
fn code(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|line| line.split_once(" // "))
        .map(|(code, _)| code.trim().to_owned())
        .collect()
}

/// Asserts that `lines` holds `expected` in order, other lines between them.
#[track_caller]
fn assert_in_order(lines: &[String], expected: &[&str]) {
    let mut rest = lines.iter();
    for want in expected {
        assert!(
            rest.any(|line| line == want),
            "`{want}` missing or out of order in:\n{}",
            lines.join("\n")
        );
    }
}

#[test]
fn lowers_arithmetic_with_promotion() {
    let lowered = lowered("let b1: byte = 1;\nlet b2: byte = 2;\nlet s = b1 + b2;\n");
    assert_eq!(
        lowered.dump("top-level"),
        "\
fn top-level() -> () {
    let _0: ();
    let b1#0: byte;
    let b2#1: byte;
    let s#2: int;
    let _4: int;
    let _5: int;

    bb0: {
        storage_live(b1#0); // 1:5
        b1#0 = 1; // 1:16
        storage_live(b2#1); // 2:5
        b2#1 = 2; // 2:16
        storage_live(s#2); // 3:5
        storage_live(_4); // 3:9
        _4 = (implicit int)copy b1#0; // 3:9
        storage_live(_5); // 3:14
        _5 = (implicit int)copy b2#1; // 3:14
        s#2 = copy _4 + copy _5; // 3:9
        storage_dead(_5); // 3:9
        storage_dead(_4); // 3:9
        _0 = (); // 3:17
        storage_dead(s#2); // 3:17
        storage_dead(b2#1); // 3:17
        storage_dead(b1#0); // 3:17
        return; // 3:17
    }
}
"
    );
}

#[test]
fn lowers_wrapping_ops() {
    let lowered =
        lowered("var a = 1;\nlet b = a +% 2;\nlet c = a -% b;\nlet d = a *% c;\nlet e = a + b;\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "b#1 = copy a#0 +% 2;",
            "c#2 = copy a#0 -% copy b#1;",
            "d#3 = copy a#0 *% copy c#2;",
            "e#4 = copy a#0 + copy b#1;",
        ],
    );
}

#[test]
fn every_operator_keeps_its_meaning() {
    let lowered = lowered(
        "let a = 7;\nlet b = 2;\nlet t = true;\n\
         let r = (a - b, a * b, a / b, a % b, a << b, a >> b, a >>> b, a & b, a | b, a ^ b);\n\
         let c = (a == b, a != b, a < b, a <= b, a > b, a >= b, t & t, t | t, t ^ t);\n",
    );
    let code = lowered.code("top-level");
    assert_in_order(
        &code,
        &[
            "_6 = copy a#0 - copy b#1;",
            "_7 = copy a#0 * copy b#1;",
            "_8 = copy a#0 / copy b#1;",
            "_9 = copy a#0 % copy b#1;",
            "_10 = copy a#0 << copy b#1;",
            "_11 = copy a#0 >> copy b#1;",
            "_12 = copy a#0 >>> copy b#1;",
            "_13 = copy a#0 & copy b#1;",
            "_14 = copy a#0 | copy b#1;",
            "_15 = copy a#0 ^ copy b#1;",
            "r#3 = (copy _6, copy _7, copy _8, copy _9, copy _10, copy _11, copy _12, copy _13, copy _14, copy _15);",
            "_16 = copy a#0 == copy b#1;",
            "_17 = copy a#0 != copy b#1;",
            "_18 = copy a#0 < copy b#1;",
            "_19 = copy a#0 <= copy b#1;",
            "_20 = copy a#0 > copy b#1;",
            "_21 = copy a#0 >= copy b#1;",
            "_22 = copy t#2 & copy t#2;",
            "_23 = copy t#2 | copy t#2;",
            "_24 = copy t#2 ^ copy t#2;",
        ],
    );
}

#[test]
fn unary_operators_and_negated_literals() {
    let lowered = lowered(
        "let a = 5;\nlet t = true;\nlet u: uint = 3;\nlet s: sbyte = -128;\n\
         let n = -a;\nlet c = ~a;\nlet f = !t;\nlet p = +a;\nlet w = -u;\nlet m = -1.5;\nlet k = -1u;\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "s#3 = -128;",
            "n#4 = -copy a#0;",
            "c#5 = ~copy a#0;",
            "f#6 = !copy t#1;",
            "p#7 = copy a#0;",
            "_12 = (implicit long)copy u#2;",
            "w#8 = -copy _12;",
            "m#9 = -1.5;",
            "_13 = (implicit long)1;",
            "k#10 = -copy _13;",
        ],
    );
}

#[test]
fn literals_and_inferred_constants_are_constants() {
    let lowered = lowered(
        "let c = 'a';\nlet s = \"a\\tb\";\nlet t = false;\nlet r = 1.5;\nlet f: float = 0.1;\n\
         let d: double = 2;\nlet top: byte = 200 + 55;\nlet q: double = 1.0 / 4;\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "c#0 = 'a';",
            "s#1 = \"a\\tb\";",
            "t#2 = false;",
            "r#3 = 1.5;",
            "f#4 = 0.10000000149011612;",
            "d#5 = 2;",
            "top#6 = 255;",
            "q#7 = 0.25;",
        ],
    );
    let body = &lowered.mir("top-level").bodies[0];
    let assigned: Vec<_> = body.blocks[0]
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            crate::StatementKind::Assign(_, crate::Rvalue::Use(Operand::Const(value))) => {
                Some(value.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        assigned.contains(&Const::Real(2.0f64.to_bits(), mora_hir::Primitive::Double)),
        "{assigned:?}"
    );
    assert!(
        assigned.contains(&Const::Int(255, mora_hir::Primitive::Byte)),
        "{assigned:?}"
    );
}

#[test]
fn casts_and_conversions_are_casts() {
    let lowered = lowered(
        "let l = 300;\nlet b = (byte)l;\nvar w: long = 1;\nw = l;\nlet i = (int)l;\nlet x = (long)b + 1;\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "b#1 = (byte)copy l#0;",
            "w#2 = (implicit long)copy l#0;",
            "i#3 = (int)copy l#0;",
            "_6 = (long)copy b#1;",
            "x#4 = copy _6 + 1;",
        ],
    );
}

#[test]
fn compound_assignment_casts_back() {
    let lowered = lowered("var b: byte = 1;\nb += 1;\nvar n = 2;\nn *= 3;\nb <<= 2;\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "storage_live(_3);",
            "_3 = (implicit int)copy b#0;",
            "storage_live(_4);",
            "_4 = (implicit int)1;",
            "storage_live(_5);",
            "_5 = copy _3 + copy _4;",
            "b#0 = (byte)copy _5;",
            "storage_dead(_5);",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "n#1 = copy n#1 * 3;",
            "_6 = (implicit int)copy b#0;",
            "_7 = copy _6 << 2;",
            "b#0 = (byte)copy _7;",
        ],
    );
}

#[test]
fn compound_assignment_on_a_string_appends() {
    let lowered = lowered("var s = \"a\";\ns += \"b\";\nlet t = s + \"c\";\n");
    assert_in_order(
        &lowered.code("top-level"),
        &["s#0 = copy s#0 + \"b\";", "t#1 = copy s#0 + \"c\";"],
    );
}

#[test]
fn postfix_increment_yields_old_value() {
    let lowered = lowered("var i = 1;\nlet j = i++;\ni--;\nlet k = ++i;\n--i;\n");
    assert_eq!(
        lowered.code("top-level"),
        [
            "storage_live(i#0);",
            "i#0 = 1;",
            "storage_live(j#1);",
            "storage_live(_4);",
            "_4 = copy i#0;",
            "i#0 = copy i#0 + 1;",
            "j#1 = copy _4;",
            "storage_dead(_4);",
            "i#0 = copy i#0 - 1;",
            "storage_live(k#2);",
            "i#0 = copy i#0 + 1;",
            "k#2 = copy i#0;",
            "i#0 = copy i#0 - 1;",
            "_0 = ();",
            "storage_dead(k#2);",
            "storage_dead(j#1);",
            "storage_dead(i#0);",
            "return;",
        ]
    );
}

#[test]
fn a_step_of_a_real_adds_a_real_one() {
    let lowered = lowered("var x = 1.5;\nx++;\nvar f: float = 1;\nf--;\n");
    let body = &lowered.mir("top-level").bodies[0];
    let ones: Vec<_> = body.blocks[0]
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            crate::StatementKind::Assign(_, crate::Rvalue::Binary(_, _, Operand::Const(one))) => {
                Some(one.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ones,
        [
            Const::Real(1.0f64.to_bits(), mora_hir::Primitive::Double),
            Const::Real(1.0f64.to_bits(), mora_hir::Primitive::Float)
        ]
    );
}

#[test]
fn a_place_read_before_an_operand_with_effects_is_copied_first() {
    let lowered = lowered("var x = 1;\nlet y = x + x++;\nlet z = x + 1;\nlet w = x++ + x;\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_5 = copy x#0;",
            "_6 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "y#1 = copy _5 + copy _6;",
            "z#2 = copy x#0 + 1;",
            "_7 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "w#3 = copy _7 + copy x#0;",
        ],
    );
}

#[test]
fn only_an_operand_that_may_change_the_place_copies_it() {
    let lowered = lowered(
        "var x = 1;\nlet a = x + -x;\nlet b = x + x * 2;\nlet c = x + (int)x;\n\
         let d = x + -x++;\nlet e = x + (1 + x++);\nlet v = x++ + x++;\n",
    );
    assert_eq!(
        lowered.code("top-level")[2..],
        [
            "storage_live(a#1);",
            "storage_live(_8);",
            "_8 = -copy x#0;",
            "a#1 = copy x#0 + copy _8;",
            "storage_dead(_8);",
            "storage_live(b#2);",
            "storage_live(_9);",
            "_9 = copy x#0 * 2;",
            "b#2 = copy x#0 + copy _9;",
            "storage_dead(_9);",
            "storage_live(c#3);",
            "storage_live(_10);",
            "_10 = (int)copy x#0;",
            "c#3 = copy x#0 + copy _10;",
            "storage_dead(_10);",
            "storage_live(d#4);",
            "storage_live(_11);",
            "_11 = copy x#0;",
            "storage_live(_12);",
            "_12 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "storage_live(_13);",
            "_13 = -copy _12;",
            "d#4 = copy _11 + copy _13;",
            "storage_dead(_13);",
            "storage_dead(_12);",
            "storage_dead(_11);",
            "storage_live(e#5);",
            "storage_live(_14);",
            "_14 = copy x#0;",
            "storage_live(_15);",
            "_15 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "storage_live(_16);",
            "_16 = 1 + copy _15;",
            "e#5 = copy _14 + copy _16;",
            "storage_dead(_16);",
            "storage_dead(_15);",
            "storage_dead(_14);",
            "storage_live(v#6);",
            "storage_live(_17);",
            "_17 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "storage_live(_18);",
            "_18 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "v#6 = copy _17 + copy _18;",
            "storage_dead(_18);",
            "storage_dead(_17);",
            "_0 = ();",
            "storage_dead(v#6);",
            "storage_dead(e#5);",
            "storage_dead(d#4);",
            "storage_dead(c#3);",
            "storage_dead(b#2);",
            "storage_dead(a#1);",
            "storage_dead(x#0);",
            "return;",
        ]
    );
}

#[test]
fn a_step_whose_value_is_used_last_is_not_copied() {
    let lowered = lowered("var x = 1;\nlet p = 1 + ++x;\nlet t = (1, ++x);\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "x#0 = copy x#0 + 1;",
            "p#1 = 1 + copy x#0;",
            "x#0 = copy x#0 + 1;",
            "t#2 = (1, copy x#0);",
        ],
    );
}

#[test]
#[should_panic(expected = "the call has no callee")]
fn a_call_inference_does_not_check_is_an_internal_error() {
    let lowered = lowered("Id<T>(move x: T): T => x;\nId(1);\n");
    lowered.mir("top-level");
}

#[test]
fn a_var_is_a_mutable_local() {
    let dump = lowered("var i = 1;\nlet j = 2;\n").dump("top-level");
    assert!(dump.contains("    let mut i#0: int;\n"), "{dump}");
    assert!(dump.contains("    let j#1: int;\n"), "{dump}");
}

#[test]
#[should_panic(expected = "methods, generic functions and async functions are not lowered")]
fn a_method_is_not_lowered_yet() {
    let lowered = lowered("struct P {\n    public X: int;\n    public Get(): int => X;\n}\n");
    lowered.mir("Get");
}

#[test]
#[should_panic(expected = "methods, generic functions and async functions are not lowered")]
fn a_generic_function_is_not_lowered_yet() {
    let lowered = lowered("Id<T>(move x: T): T => x;\n");
    lowered.mir("Id");
}

#[test]
#[should_panic(expected = "methods, generic functions and async functions are not lowered")]
fn an_async_function_is_not_lowered_yet() {
    let lowered = lowered("async Wait() {}\n");
    lowered.mir("Wait");
}

#[test]
fn assignment_stores_the_converted_value() {
    let lowered = lowered("var x = 1;\nx = 2;\nvar t = (1, true);\nt = (x, false);\n");
    assert_in_order(
        &lowered.code("top-level"),
        &["x#0 = 2;", "t#1 = (1, true);", "t#1 = (copy x#0, false);"],
    );
}

#[test]
fn interpolation_is_a_stand_in() {
    let lowered = lowered("let n = 3;\nlet s = $\"n = {n}, {{n + 1}} = {n + 1}\\n\";\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_3 = copy n#0 + 1;",
            "s#1 = $\"n = {copy n#0}, {{n + 1}} = {copy _3}\\n\";",
        ],
    );
}

#[test]
#[should_panic(expected = "raw")]
fn raw_interpolation_is_an_internal_error() {
    let lowered = lowered("let n = 3;\nlet s = $`n = {n}`;\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "a hole with an alignment or a format is not lowered")]
fn a_formatted_hole_is_an_internal_error() {
    let lowered = lowered("let n = 3;\nlet s = $\"{n:x}\";\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "a hole with an alignment or a format is not lowered")]
fn an_aligned_hole_is_an_internal_error() {
    let lowered = lowered("let n = 3;\nlet s = $\"{n,4}\";\n");
    lowered.mir("top-level");
}

#[test]
fn external_call_is_a_statement_without_dest() {
    let lowered = lowered("let x = 1;\nConsole.WriteLine(x);\nConsole.WriteLine(\"done\");\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "call Console.WriteLine{external}(copy x#0) -> bb1;",
            "call Console.WriteLine{external}(\"done\") -> bb2;",
            "_0 = ();",
        ],
    );
    let body = &lowered.mir("top-level").bodies[0];
    let TerminatorKind::Call { callee, dest, .. } = &body.blocks[0].terminator.kind else {
        panic!("not a call: {:?}", body.blocks[0].terminator);
    };
    assert_eq!(
        *callee,
        Callee::External(vec!["Console".to_owned(), "WriteLine".to_owned()])
    );
    assert_eq!(*dest, None);
}

#[test]
#[should_panic(expected = "external")]
fn external_call_value_used_is_internal_error() {
    let lowered = lowered("Console.WriteLine(Console.ReadLine());\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "external")]
fn external_call_stored_in_a_local_is_internal_error() {
    let lowered = lowered("let line = Console.ReadLine();\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "external")]
fn external_call_stored_in_a_typed_local_is_internal_error() {
    let lowered = lowered("let line: string = Console.ReadLine();\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "has no type")]
fn untyped_value_is_internal_error() {
    let lowered = lowered("Console.WriteLine([1, 2]);\n");
    lowered.mir("top-level");
}

#[test]
#[should_panic(expected = "the local `f` has no type")]
fn untyped_local_is_internal_error() {
    let lowered = lowered("let f = (x: int) => x;\n");
    lowered.mir("top-level");
}

#[test]
fn the_message_names_the_body_and_the_expression() {
    let lowered = lowered("F() {\n    Console.WriteLine([1, 2]);\n}\n");
    let owner = lowered.owner("F");
    let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mir(&lowered.db, lowered.project, lowered.file, owner);
    }))
    .expect_err("an internal error");
    let message = message
        .downcast_ref::<String>()
        .expect("a formatted message");
    assert_eq!(
        message,
        "internal error lowering UsersApi.F: the value has no type: `[1, 2]` at 2:23"
    );
}

#[test]
fn a_diverging_call_does_not_return() {
    let lowered = lowered("F(): int {\n    Todo();\n}\n");
    assert_eq!(
        lowered.code("F"),
        ["call Todo{external}() -> !;"],
        "{}",
        lowered.dump("F")
    );
}

#[test]
fn unit_body_returns_unit() {
    let lowered = lowered("F() {\n    let x = 1;\n}\n");
    assert_eq!(
        lowered.dump("F"),
        "\
fn UsersApi.F() -> () {
    let _0: ();
    let x#0: int;

    bb0: {
        storage_live(x#0); // 2:9
        x#0 = 1; // 2:13
        _0 = (); // 3:1
        storage_dead(x#0); // 3:1
        return; // 3:1
    }
}
"
    );
}

#[test]
fn a_block_ends_the_storage_of_its_locals() {
    let lowered = lowered("{\n    let x = 1;\n}\nlet y = 2;\n");
    let dump = lowered.dump("top-level");
    assert_in_order(
        &code(&dump),
        &[
            "storage_live(x#0);",
            "x#0 = 1;",
            "storage_dead(x#0);",
            "storage_live(y#1);",
            "storage_dead(y#1);",
        ],
    );
    assert!(dump.contains("storage_dead(x#0); // 3:1\n"), "{dump}");
}

#[test]
fn parameters_are_borrowed_and_read_through_a_dereference() {
    let lowered = lowered("Add(a: int, b: int): int {\n    return a + b;\n}\n");
    assert_eq!(
        lowered.dump("Add"),
        "\
fn UsersApi.Add(a#0: in int, b#1: in int) -> int {
    let _0: int;
    let a#0: in int;
    let b#1: in int;

    bb0: {
        _0 = copy (*a#0) + copy (*b#1); // 2:12
        return; // 2:5
    }
}
"
    );
}

#[test]
fn return_ends_the_storage_of_every_local() {
    let lowered = lowered(
        "F(n: int): int {\n    let x = n;\n    {\n        let y = x;\n        return y + 1;\n    }\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(x#1);",
            "x#1 = copy (*n#0);",
            "storage_live(y#2);",
            "y#2 = copy x#1;",
            "_0 = copy y#2 + 1;",
            "storage_dead(y#2);",
            "storage_dead(x#1);",
            "return;",
        ]
    );
}

#[test]
fn return_without_a_value_returns_unit() {
    let lowered = lowered("F() {\n    return;\n}\n");
    assert_eq!(lowered.code("F"), ["_0 = ();", "return;"]);
}

#[test]
fn a_return_value_is_converted() {
    let lowered = lowered("F(b: byte): long {\n    return b;\n}\n");
    assert_eq!(
        lowered.code("F"),
        ["_0 = (implicit long)copy (*b#0);", "return;"]
    );
}

#[test]
fn expression_bodies_return_their_value() {
    let lowered = lowered("Twice(x: int): int => x * 2;\nHello() => Console.WriteLine(\"hi\");\n");
    assert_eq!(lowered.code("Twice"), ["_0 = copy (*x#0) * 2;", "return;"]);
    assert_eq!(
        lowered.code("Hello"),
        [
            "call Console.WriteLine{external}(\"hi\") -> bb1;",
            "_0 = ();",
            "return;"
        ]
    );
}

#[test]
fn calls_pass_in_arguments_as_borrows() {
    let lowered =
        lowered("Add(a: int, b: int): int => a + b;\nlet x = 1;\nlet r = Add(x, 2);\nAdd(r, r);\n");
    assert_eq!(
        lowered.code("top-level"),
        [
            "storage_live(x#0);",
            "x#0 = 1;",
            "storage_live(r#1);",
            "storage_live(_3);",
            "_3 = &in x#0;",
            "storage_live(_4);",
            "_4 = 2;",
            "storage_live(_5);",
            "_5 = &in _4;",
            "r#1 = call UsersApi.Add{fn UsersApi.Add}(copy _3, copy _5) -> bb1;",
            "storage_dead(_5);",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "storage_live(_6);",
            "_6 = &in r#1;",
            "storage_live(_7);",
            "_7 = &in r#1;",
            "call UsersApi.Add{fn UsersApi.Add}(copy _6, copy _7) -> bb2;",
            "storage_dead(_7);",
            "storage_dead(_6);",
            "_0 = ();",
            "storage_dead(r#1);",
            "storage_dead(x#0);",
            "return;",
        ]
    );
}

#[test]
fn an_in_parameter_is_passed_on_as_a_borrow_of_its_value() {
    let lowered = lowered("Id(n: int): int => n;\nTwice(n: int): int => Id(n) + Id(n + 1);\n");
    assert_in_order(
        &lowered.code("Twice"),
        &[
            "_2 = &in (*n#0);",
            "_3 = call UsersApi.Id{fn UsersApi.Id}(copy _2) -> bb1;",
            "_4 = copy (*n#0) + 1;",
            "_5 = &in _4;",
            "_6 = call UsersApi.Id{fn UsersApi.Id}(copy _5) -> bb2;",
            "_0 = copy _3 + copy _6;",
            "return;",
        ],
    );
}

#[test]
fn ref_and_move_arguments_follow_their_modes() {
    let lowered = lowered(
        "Bump(ref x: int) {\n    x += 1;\n    x = x * 2;\n}\nTake(move s: string) {}\n\
         var n = 1;\nBump(ref n);\nlet s = \"a\";\nTake(s);\nTake(\"b\");\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_3 = &ref n#0;",
            "call UsersApi.Bump{fn UsersApi.Bump}(copy _3) -> bb1;",
            "call UsersApi.Take{fn UsersApi.Take}(copy s#1) -> bb2;",
            "call UsersApi.Take{fn UsersApi.Take}(\"b\") -> bb3;",
        ],
    );
    assert!(
        lowered
            .dump("Bump")
            .starts_with("fn UsersApi.Bump(x#0: ref int) -> () {\n"),
        "{}",
        lowered.dump("Bump")
    );
    assert_in_order(
        &lowered.code("Bump"),
        &["(*x#0) = copy (*x#0) + 1;", "(*x#0) = copy (*x#0) * 2;"],
    );
    assert!(
        lowered
            .dump("Take")
            .starts_with("fn UsersApi.Take(s#0: string) -> () {\n"),
        "{}",
        lowered.dump("Take")
    );
}

#[test]
fn an_argument_is_converted_to_its_parameter() {
    let lowered = lowered("Wide(v: long, move w: long) {}\nlet b: byte = 1;\nWide(b, b);\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_2 = (implicit long)copy b#0;",
            "_3 = &in _2;",
            "_4 = (implicit long)copy b#0;",
            "call UsersApi.Wide{fn UsersApi.Wide}(copy _3, copy _4) -> bb1;",
        ],
    );
}

#[test]
fn an_argument_before_one_with_effects_is_copied_first() {
    let lowered = lowered(
        "Pair(a: int, move b: int) {}\nvar x = 1;\nPair(x, x++);\nPair(x, x);\nPair(1, ++x);\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_2 = copy x#0;",
            "_3 = &in _2;",
            "_4 = copy x#0;",
            "x#0 = copy x#0 + 1;",
            "call UsersApi.Pair{fn UsersApi.Pair}(copy _3, copy _4) -> bb1;",
            "_5 = &in x#0;",
            "call UsersApi.Pair{fn UsersApi.Pair}(copy _5, copy x#0) -> bb2;",
            "_6 = 1;",
            "_7 = &in _6;",
            "x#0 = copy x#0 + 1;",
            "call UsersApi.Pair{fn UsersApi.Pair}(copy _7, copy x#0) -> bb3;",
        ],
    );
}

#[test]
fn named_arguments_run_in_written_order_and_pass_in_parameter_order() {
    let lowered = lowered(
        "Pair(move a: int, move b: int) {}\nOne(): int => 1;\nTwo(): int => 2;\nPair(b: Two(), a: One());\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_1 = call UsersApi.Two{fn UsersApi.Two}() -> bb1;",
            "_2 = call UsersApi.One{fn UsersApi.One}() -> bb2;",
            "call UsersApi.Pair{fn UsersApi.Pair}(copy _2, copy _1) -> bb3;",
        ],
    );
}

#[test]
fn an_omitted_argument_takes_its_constant_default() {
    let lowered = lowered(
        "O(move a: int, move b: int = 2, move c: int = 1 + 2, move d: int = -1, move e: long = 4u, f: string = \"x\") {}\n\
         O(1);\nO(1, e: 5, b: 0);\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_1 = (implicit long)4;",
            "_2 = \"x\";",
            "_3 = &in _2;",
            "call UsersApi.O{fn UsersApi.O}(1, 2, 3, -1, copy _1, copy _3) -> bb1;",
            "_4 = \"x\";",
            "_5 = &in _4;",
            "call UsersApi.O{fn UsersApi.O}(1, 0, 3, -1, 5, copy _5) -> bb2;",
        ],
    );
}

#[test]
#[should_panic(expected = "constant evaluation")]
fn a_default_that_needs_evaluation_is_an_internal_error() {
    let lowered = lowered("Zero(): int => 0;\nO(move a: int = Zero()) {}\nO();\n");
    lowered.mir("top-level");
}

#[test]
fn a_call_of_a_unit_function_can_be_stored() {
    let lowered = lowered("Nothing() {}\nlet u = Nothing();\n");
    assert_in_order(
        &lowered.code("top-level"),
        &["u#0 = call UsersApi.Nothing{fn UsersApi.Nothing}() -> bb1;"],
    );
}

#[test]
fn an_expression_statement_evaluates_its_value() {
    let lowered = lowered("One(): int => 1;\nlet x = 1;\nOne() + x;\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_2 = call UsersApi.One{fn UsersApi.One}() -> bb1;",
            "_3 = copy _2 + copy x#0;",
        ],
    );
}

#[test]
fn a_dependency_on_std_rejects_the_stand_ins() {
    let db = TestDb::new();
    let std = db.project("std", ProjectKind::Lib, &[("src/Lib.mr", "")], Vec::new());
    let project = db.project(
        "users-api",
        ProjectKind::App,
        &[("src/Program.mr", "let n = 1;\nlet s = $\"{n}\";\n")],
        vec![Dependency {
            name: "Std".to_owned(),
            project: std,
        }],
    );
    let lowered = in_project(db, project);
    let owner = lowered.owner("top-level");
    let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mir(&lowered.db, lowered.project, lowered.file, owner);
    }))
    .expect_err("the validator rejects the stand-in");
    let message = message.downcast_ref::<String>().expect("a message");
    assert!(
        message.contains("interpolation is not lowered once std is loaded"),
        "{message}"
    );
}

#[test]
fn another_dependency_keeps_the_stand_ins() {
    let db = TestDb::new();
    let util = db.project("util", ProjectKind::Lib, &[("src/Lib.mr", "")], Vec::new());
    let project = db.project(
        "users-api",
        ProjectKind::App,
        &[("src/Program.mr", "let n = 1;\nlet s = $\"{n}\";\n")],
        vec![Dependency {
            name: "Util".to_owned(),
            project: util,
        }],
    );
    let lowered = in_project(db, project);
    assert_in_order(&lowered.code("top-level"), &["s#1 = $\"{copy n#0}\";"]);
}

#[test]
fn mir_dump_prints_every_body_in_order() {
    let lowered = lowered("struct P {\n    public X: int;\n}\nOne(): int => 1;\nlet x = One();\n");
    let dump = mir_dump(&lowered.db, lowered.project, lowered.file);
    let top = lowered.dump("top-level");
    let one = lowered.dump("One");
    assert_eq!(dump, format!("{top}\n{one}"));
}

#[test]
fn mir_dump_of_a_file_without_code_is_empty() {
    let lowered = lowered("struct P {\n    public X: int;\n}\n");
    assert_eq!(mir_dump(&lowered.db, lowered.project, lowered.file), "");
}
