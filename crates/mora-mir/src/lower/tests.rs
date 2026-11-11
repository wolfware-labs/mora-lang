//! Tests of lowering, each from source text through the front end.

use mora_hir::test_support::{TestDb, app, codes, file_named, item_named};
use mora_hir::{BodyOwner, ItemId, body, body_owners, diagnostics, item_tree, parse};
use mora_span::{Dependency, Project, ProjectKind, SourceFile};

use crate::{
    Callee, Const, Mir, Operand, Projection, Rvalue, StatementKind, TerminatorKind, mir, mir_dump,
    validate, write_mir,
};

/// A file of an app that the front end reads and checks without an error.
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
    let syntax: Vec<String> = parse(&db, file)
        .errors()
        .iter()
        .map(|error| error.code.to_string())
        .collect();
    assert_eq!(syntax, Vec::<String>::new(), "{}", file.text(&db));
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
        BodyOwner::Item(self.item(name).item)
    }

    /// The item `name` declared in the file.
    fn item(&self, name: &str) -> ItemId {
        item_named(&self.db, self.project, self.file, name)
    }

    fn mir(&self, name: &str) -> &Mir {
        self.mir_of(self.owner(name))
    }

    /// The MIR of `owner`, validated.
    fn mir_of(&self, owner: BodyOwner) -> &Mir {
        let mir = mir(&self.db, self.project, self.file, owner);
        assert_eq!(validate(&self.db, mir, false), Ok(()));
        mir
    }

    /// The owner of the initializer of the field `field` of the type `ty`.
    fn initializer(&self, ty: &str, field: &str) -> BodyOwner {
        let tree = item_tree(&self.db, self.file);
        let (item, data) = tree
            .items()
            .find(|(_, item)| item.name == ty)
            .expect("the type");
        let index = data
            .values
            .iter()
            .position(|value| value.name == field)
            .expect("the field");
        BodyOwner::Value(item, u32::try_from(index).expect("a small index"))
    }

    /// The names of the bodies of `name`, in order.
    fn bodies(&self, name: &str) -> Vec<String> {
        self.mir(name)
            .bodies
            .iter()
            .map(|body| body.name.clone())
            .collect()
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
#[should_panic(expected = "MR0016")]
fn the_helper_rejects_a_lexical_error() {
    lowered("let n = 1;\nlet s = $$\"{{n}} {n}\";\n");
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
fn a_negated_literal_is_one_constant() {
    let lowered = lowered("let b: sbyte = -128;\nlet x: long = -1;\nlet r = -2.5;\n");
    assert_in_order(
        &lowered.code("top-level"),
        &["b#0 = -128;", "x#1 = -1;", "r#2 = -2.5;"],
    );
    let body = &lowered.mir("top-level").bodies[0];
    let stored = |n: usize| match &body.blocks[0].statements[n].kind {
        crate::StatementKind::Assign(_, crate::Rvalue::Use(Operand::Const(value))) => value.clone(),
        other => panic!("not a constant: {other:?}"),
    };
    assert_eq!(stored(1), Const::Int(-128, mora_hir::Primitive::SByte));
    assert_eq!(stored(3), Const::Int(-1, mora_hir::Primitive::Long));
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
            "f#4 = 0.1;",
            "d#5 = 2.0;",
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
#[should_panic(expected = "the signature has 1 parameters but the body 0")]
fn a_parameter_without_a_local_is_an_internal_error() {
    // The parser recovers from the missing name, and the body declares no
    // local for the parameter; lowering never sees such a body from the
    // driver, which stops at the syntax error.
    let db = TestDb::new();
    let project = app(&db, &[("src/Program.mr", "F(: int) {}\n")]);
    let file = file_named(&db, project, "src/Program.mr");
    let tree = item_tree(&db, file);
    let (item, _) = tree
        .items()
        .find(|(_, item)| item.name == "F")
        .expect("the function");
    mir(&db, project, file, BodyOwner::Item(item));
}

#[test]
fn a_var_is_a_mutable_local() {
    let dump = lowered("var i = 1;\nlet j = 2;\n").dump("top-level");
    assert!(dump.contains("    let mut i#0: int;\n"), "{dump}");
    assert!(dump.contains("    let j#1: int;\n"), "{dump}");
}

#[test]
#[should_panic(expected = "generic and async functions are not lowered")]
fn a_generic_function_is_not_lowered_yet() {
    let lowered = lowered("Id<T>(move x: T): T => x;\n");
    lowered.mir("Id");
}

#[test]
#[should_panic(expected = "generic and async functions are not lowered")]
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
fn raw_interpolation_is_a_stand_in_too() {
    let lowered = lowered(
        "let s = $`n = {1}`;\nlet t = $`\\q {1}`;\nlet u = $`\n    a {2}\n      b\n    `;\n",
    );
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "s#0 = $\"n = {1}\";",
            "t#1 = $\"\\\\q {1}\";",
            "u#2 = $\"a {2}\\n  b\";",
        ],
    );
}

#[test]
fn a_hole_keeps_its_alignment_and_format() {
    let lowered =
        lowered("let n = 3;\nlet w: byte = 6;\nlet s = $\"{n:x}|{n,5}|{n,-w:F2}|{n,1 + 2}\";\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_4 = (implicit int)copy w#1;",
            "_5 = -copy _4;",
            "s#2 = $\"{copy n#0:x}|{copy n#0,5}|{copy n#0,copy _5:F2}|{copy n#0,3}\";",
        ],
    );
}

#[test]
fn an_alignment_after_a_place_with_effects_copies_the_place() {
    let lowered = lowered("var n = 3;\nlet s = $\"{n,n++}\";\n");
    assert_in_order(
        &lowered.code("top-level"),
        &[
            "_3 = copy n#0;",
            "_4 = copy n#0;",
            "n#0 = copy n#0 + 1;",
            "s#1 = $\"{copy _3,copy _4}\";",
        ],
    );
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

// Control flow.

#[test]
fn short_circuit_evaluates_right_operand_only_when_needed() {
    let lowered = lowered(
        "F(): bool => true;\nG(a: bool): bool {\n    return a && F();\n}\nH(a: bool, b: bool, c: bool): bool => a || b && c;\n",
    );
    // `F` is called only in the block reached when `a` is true.
    assert_eq!(
        lowered.code("G"),
        [
            "storage_live(_2);",
            "switch copy (*a#0) [false -> bb2] otherwise bb1;",
            "_2 = call UsersApi.F{fn UsersApi.F}() -> bb3;",
            "_2 = false;",
            "goto bb4;",
            "goto bb4;",
            "_0 = copy _2;",
            "storage_dead(_2);",
            "return;",
        ]
    );
    // `b && c` runs only where `a` is false, and `c` only where `b` is
    // true; the temporary of `b && c` ends in its own branch.
    assert_eq!(
        lowered.code("H"),
        [
            "storage_live(_4);",
            "switch copy (*a#0) [false -> bb2] otherwise bb1;",
            "_4 = true;",
            "goto bb3;",
            "storage_live(_5);",
            "switch copy (*b#1) [false -> bb5] otherwise bb4;",
            "_0 = copy _4;",
            "storage_dead(_4);",
            "return;",
            "_5 = copy (*c#2);",
            "goto bb6;",
            "_5 = false;",
            "goto bb6;",
            "_4 = copy _5;",
            "storage_dead(_5);",
            "goto bb3;",
        ]
    );
}

#[test]
fn break_out_of_nested_scope_kills_inner_locals() {
    let lowered = lowered(
        "F() {\n    var i = 0;\n    while (i < 3) {\n        let x = i;\n        {\n            let y = x;\n            if (y == 2) {\n                break;\n            }\n        }\n        i++;\n    }\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(i#0);",
            "i#0 = 0;",
            "goto bb1;",
            // bb1: the condition.
            "storage_live(_4);",
            "_4 = copy i#0 < 3;",
            "switch copy _4 [false -> bb3] otherwise bb2;",
            // bb2: the body.
            "storage_dead(_4);",
            "storage_live(x#1);",
            "x#1 = copy i#0;",
            "storage_live(y#2);",
            "y#2 = copy x#1;",
            "storage_live(_5);",
            "_5 = copy y#2 == 2;",
            "switch copy _5 [false -> bb6] otherwise bb5;",
            // bb3: the condition is false.
            "storage_dead(_4);",
            "goto bb4;",
            // bb4: after the loop.
            "_0 = ();",
            "storage_dead(i#0);",
            "return;",
            // bb5: `break`, innermost first.
            "storage_dead(_5);",
            "storage_dead(y#2);",
            "storage_dead(x#1);",
            "goto bb4;",
            // bb6: no `break`; the scopes are still open.
            "storage_dead(_5);",
            "goto bb7;",
            "storage_dead(y#2);",
            "i#0 = copy i#0 + 1;",
            "storage_dead(x#1);",
            "goto bb1;",
        ]
    );
}

#[test]
fn continue_in_for_runs_the_update() {
    let lowered = lowered(
        "F() {\n    var n = 0;\n    for (var i = 0; i < 3; i++) {\n        if (i == 1) {\n            continue;\n        }\n        n += i;\n    }\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(n#0);",
            "n#0 = 0;",
            "storage_live(i#1);",
            "i#1 = 0;",
            "goto bb1;",
            "storage_live(_3);",
            "_3 = copy i#1 < 3;",
            "switch copy _3 [false -> bb3] otherwise bb2;",
            "storage_dead(_3);",
            "storage_live(_4);",
            "_4 = copy i#1 == 1;",
            "switch copy _4 [false -> bb7] otherwise bb6;",
            "storage_dead(_3);",
            "goto bb4;",
            // bb4: after the loop, where `i` ends.
            "storage_dead(i#1);",
            "_0 = ();",
            "storage_dead(n#0);",
            "return;",
            // bb5: the update.
            "i#1 = copy i#1 + 1;",
            "goto bb1;",
            // bb6: `continue` goes to the update.
            "storage_dead(_4);",
            "goto bb5;",
            "storage_dead(_4);",
            "goto bb8;",
            "n#0 = copy n#0 + copy i#1;",
            "goto bb5;",
        ]
    );
}

#[test]
fn return_from_nested_scope_kills_locals() {
    let lowered = lowered(
        "F(n: int): int {\n    let a = n;\n    while (a > 0) {\n        let b = a;\n        if (b > 1) {\n            let c = b;\n            return c;\n        }\n    }\n    return 0;\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(a#1);",
            "a#1 = copy (*n#0);",
            "goto bb1;",
            "storage_live(_5);",
            "_5 = copy a#1 > 0;",
            "switch copy _5 [false -> bb3] otherwise bb2;",
            "storage_dead(_5);",
            "storage_live(b#2);",
            "b#2 = copy a#1;",
            "storage_live(_6);",
            "_6 = copy b#2 > 1;",
            "switch copy _6 [false -> bb6] otherwise bb5;",
            "storage_dead(_5);",
            "goto bb4;",
            "_0 = 0;",
            "storage_dead(a#1);",
            "return;",
            // bb5: `return c;` ends every local, innermost first.
            "storage_dead(_6);",
            "storage_live(c#3);",
            "c#3 = copy b#2;",
            "_0 = copy c#3;",
            "storage_dead(c#3);",
            "storage_dead(b#2);",
            "storage_dead(a#1);",
            "return;",
            "storage_dead(_6);",
            "goto bb7;",
            "storage_dead(b#2);",
            "goto bb1;",
        ]
    );
}

#[test]
fn statements_after_return_are_not_lowered() {
    let lowered = lowered(
        "F(): int {\n    return 1;\n    let x = 2;\n}\nG(): int {\n    Todo();\n    let y = 3;\n}\n",
    );
    // The block after the jump stays empty and ends in `unreachable`, and a
    // local declared only after it is not declared.
    assert_eq!(
        lowered.dump("F"),
        "\
fn UsersApi.F() -> int {
    let _0: int;

    bb0: {
        _0 = 1; // 2:12
        return; // 2:5
    }
}
"
    );
    let body = &lowered.mir("F").bodies[0];
    assert_eq!(body.blocks.len(), 1);
    assert_eq!(lowered.code("G"), ["call Todo{external}() -> !;"]);
    assert_eq!(lowered.mir("G").bodies[0].locals.len(), 1);
}

#[test]
fn dead_branches_and_loops_leave_no_blocks() {
    let lowered = lowered(
        "F(c: bool) {\n    return;\n    if (c) {\n        let a = 1;\n    }\n    while (c) {\n        let d = 3;\n    }\n}\n\
         G(c: bool) {\n    do {\n        return;\n    } while (c);\n}\n",
    );
    assert_eq!(
        lowered.dump("F"),
        "\
fn UsersApi.F(c#0: in bool) -> () {
    let _0: ();
    let c#0: in bool;

    bb0: {
        _0 = (); // 2:5
        return; // 2:5
    }
}
"
    );
    // The condition of a `do` whose body always returns is not lowered.
    assert_eq!(lowered.code("G"), ["goto bb1;", "_0 = ();", "return;"]);
}

#[test]
fn a_jump_in_unreachable_code_adds_no_terminator() {
    let lowered = lowered(
        "F() {\n    while (true) {\n        break;\n        continue;\n    }\n    return;\n    return;\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        ["goto bb1;", "goto bb2;", "_0 = ();", "return;",]
    );
}

#[test]
fn do_while_false_runs_once_shape() {
    let lowered =
        lowered("F() {\n    var once = 0;\n    do {\n        once++;\n    } while (false);\n}\n");
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(once#0);",
            "once#0 = 0;",
            "goto bb1;",
            "once#0 = copy once#0 + 1;",
            "goto bb2;",
            "goto bb3;",
            "_0 = ();",
            "storage_dead(once#0);",
            "return;",
        ]
    );
}

#[test]
fn do_while_tests_its_condition_after_the_body() {
    let lowered =
        lowered("F() {\n    var s = 0;\n    do {\n        s++;\n    } while (s < 3);\n}\n");
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(s#0);",
            "s#0 = 0;",
            "goto bb1;",
            "s#0 = copy s#0 + 1;",
            "goto bb2;",
            "storage_live(_2);",
            "_2 = copy s#0 < 3;",
            "switch copy _2 [false -> bb5] otherwise bb4;",
            "_0 = ();",
            "storage_dead(s#0);",
            "return;",
            "storage_dead(_2);",
            "goto bb1;",
            "storage_dead(_2);",
            "goto bb3;",
        ]
    );
}

#[test]
fn a_loop_whose_condition_is_true_ends_only_by_break() {
    let lowered = lowered(
        "F(): int {\n    var i = 0;\n    while (true) {\n        i++;\n    }\n}\nG(): int {\n    var i = 0;\n    do {\n        i++;\n    } while (true);\n}\nH() {\n    for (;;) {\n        break;\n    }\n}\nK(): int {\n    for (var i = 0; true; i++) {\n    }\n}\n",
    );
    // No `return` follows a loop that never ends.
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(i#0);",
            "i#0 = 0;",
            "goto bb1;",
            "i#0 = copy i#0 + 1;",
            "goto bb1;"
        ]
    );
    assert_eq!(
        lowered.code("G"),
        [
            "storage_live(i#0);",
            "i#0 = 0;",
            "goto bb1;",
            "i#0 = copy i#0 + 1;",
            "goto bb2;",
            "goto bb1;"
        ]
    );
    assert_eq!(
        lowered.code("H"),
        ["goto bb1;", "goto bb2;", "_0 = ();", "return;"]
    );
    assert_eq!(
        lowered.code("K"),
        [
            "storage_live(i#0);",
            "i#0 = 0;",
            "goto bb1;",
            "goto bb2;",
            "i#0 = copy i#0 + 1;",
            "goto bb1;"
        ]
    );
}

#[test]
fn a_body_whose_every_path_returns_needs_no_return() {
    let lowered = lowered(
        "F(c: bool): int {\n    if (c) {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "switch copy (*c#0) [false -> bb2] otherwise bb1;",
            "_0 = 1;",
            "return;",
            "_0 = 2;",
            "return;"
        ]
    );
}

#[test]
fn if_without_else_joins_after_the_branch() {
    let lowered = lowered(
        "F(c: bool) {\n    var n = 0;\n    if (c) {\n        n = 1;\n    }\n    n = 2;\n}\n",
    );
    let body = &lowered.mir("F").bodies[0];
    let TerminatorKind::Switch {
        discr,
        targets,
        otherwise,
    } = &body.blocks[0].terminator.kind
    else {
        panic!("not a switch: {:?}", body.blocks[0].terminator);
    };
    assert!(matches!(discr, Operand::Copy(_)), "{discr:?}");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, Const::Bool(false));
    assert_eq!((otherwise.0, targets[0].1.0), (1, 2));
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(n#1);",
            "n#1 = 0;",
            "switch copy (*c#0) [false -> bb2] otherwise bb1;",
            "n#1 = 1;",
            "goto bb3;",
            "goto bb3;",
            "n#1 = 2;",
            "_0 = ();",
            "storage_dead(n#1);",
            "return;",
        ]
    );
}

#[test]
fn conditional_expression_writes_one_temp() {
    let lowered = lowered("F(c: bool): int {\n    let x = c ? 1 : 2;\n    return x;\n}\n");
    assert_eq!(lowered.mir("F").bodies[0].locals.len(), 4);
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(x#1);",
            "storage_live(_3);",
            "switch copy (*c#0) [false -> bb2] otherwise bb1;",
            "_3 = 1;",
            "goto bb3;",
            "_3 = 2;",
            "goto bb3;",
            "x#1 = copy _3;",
            "storage_dead(_3);",
            "_0 = copy x#1;",
            "storage_dead(x#1);",
            "return;",
        ]
    );
}

#[test]
fn the_branches_of_a_conditional_end_their_own_temporaries() {
    let lowered =
        lowered("G(x: int): int => x;\nF(c: bool, n: int): int => n + (c ? G(n) + 1 : 0);\n");
    let code = lowered.code("F");
    // The temporaries open before the branch (`n` copied, the value of
    // `?:`) end once, after the sum; those of `G(n) + 1` end in its
    // branch.
    assert_in_order(
        &code,
        &[
            "storage_live(_3);",
            "storage_live(_4);",
            "switch copy (*c#0) [false -> bb2] otherwise bb1;",
            "_4 = 0;",
            "goto bb4;",
            "storage_dead(_5);",
            "goto bb4;",
            "_0 = copy _3 + copy _4;",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "return;",
        ],
    );
    for temp in ["storage_dead(_3);", "storage_dead(_4);"] {
        assert_eq!(
            code.iter().filter(|line| *line == temp).count(),
            1,
            "{}",
            code.join("\n")
        );
    }
}

#[test]
fn a_condition_with_temporaries_ends_them_in_both_branches() {
    let lowered =
        lowered("F(n: int) {\n    var m = n;\n    if (m + 1 > 2) {\n        m = 0;\n    }\n}\n");
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(m#1);",
            "m#1 = copy (*n#0);",
            "storage_live(_3);",
            "_3 = copy m#1 + 1;",
            "storage_live(_4);",
            "_4 = copy _3 > 2;",
            "switch copy _4 [false -> bb2] otherwise bb1;",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "m#1 = 0;",
            "goto bb3;",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "goto bb3;",
            "_0 = ();",
            "storage_dead(m#1);",
            "return;",
        ]
    );
}

#[test]
fn locals_inside_control_flow_are_declared() {
    let lowered = lowered(
        "var n = 0;\nif (n > 0) { let a = 1; } else { let b = 2; }\nwhile (n < 1) { let c = 3; n++; }\ndo { let d = 4; } while (false);\nfor (var i = 0; i < 1; i++) { let e = 5; }\nunsafe { let f = 6; }\n",
    );
    let body = &lowered.mir("top-level").bodies[0];
    let names: Vec<&str> = body
        .locals
        .iter()
        .filter_map(|decl| decl.source.as_ref())
        .map(|source| source.name.as_str())
        .collect();
    assert_eq!(names, ["n", "a", "b", "c", "d", "i", "e", "f"]);
}

#[test]
fn diverging_calls_in_value_positions_do_not_return() {
    let lowered = lowered(
        "A(): int => Todo();\nB(): int {\n    return Todo();\n}\nC() {\n    let x: int = Todo();\n}\nD() => Todo();\n",
    );
    for name in ["A", "B", "D"] {
        assert_eq!(
            lowered.code(name),
            ["call Todo{external}() -> !;"],
            "{name}"
        );
    }
    assert_eq!(
        lowered.code("C"),
        ["storage_live(x#0);", "call Todo{external}() -> !;"]
    );
}

#[test]
fn a_short_circuit_in_a_loop_condition() {
    let lowered = lowered("H(): bool => true;\nF(a: bool) {\n    while (a && H()) {\n    }\n}\n");
    // `H` is called only in bb2, which the switch reaches when `a` is true;
    // the condition's temporary ends at the start of both successors.
    assert_eq!(
        lowered.code("F"),
        [
            "goto bb1;",
            "storage_live(_2);",
            "switch copy (*a#0) [false -> bb3] otherwise bb2;",
            "_2 = call UsersApi.H{fn UsersApi.H}() -> bb4;",
            "_2 = false;",
            "goto bb5;",
            "goto bb5;",
            "switch copy _2 [false -> bb7] otherwise bb6;",
            "storage_dead(_2);",
            "goto bb1;",
            "storage_dead(_2);",
            "goto bb8;",
            "_0 = ();",
            "return;",
        ]
    );
}

#[test]
fn break_from_an_inner_loop_ends_only_its_locals() {
    let lowered = lowered(
        "F() {\n    while (true) {\n        let x = 1;\n        while (true) {\n            let y = 2;\n            break;\n        }\n        return;\n    }\n}\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "goto bb1;",
            "storage_live(x#0);",
            "x#0 = 1;",
            "goto bb2;",
            "storage_live(y#1);",
            "y#1 = 2;",
            "storage_dead(y#1);",
            "goto bb3;",
            "_0 = ();",
            "storage_dead(x#0);",
            "return;",
        ]
    );
}

#[test]
fn return_from_a_for_body_ends_the_init_local() {
    let lowered = lowered("F() {\n    for (var i = 0; i < 3; i++) {\n        return;\n    }\n}\n");
    // The update, which nothing reaches, is not lowered.
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(i#0);",
            "i#0 = 0;",
            "goto bb1;",
            "storage_live(_2);",
            "_2 = copy i#0 < 3;",
            "switch copy _2 [false -> bb3] otherwise bb2;",
            "storage_dead(_2);",
            "_0 = ();",
            "storage_dead(i#0);",
            "return;",
            "storage_dead(_2);",
            "goto bb4;",
            "storage_dead(i#0);",
            "_0 = ();",
            "return;",
        ]
    );
}

#[test]
fn a_diverging_call_as_an_operand_ends_its_expression() {
    let lowered = lowered(
        "G(x: int): int => x;\nA(): int => G(Todo());\nB(): int => 1 + Todo();\n\
         C() {\n    Console.WriteLine(Todo());\n}\nD() {\n    if (Todo()) {\n    }\n}\n\
         E() {\n    while (Todo()) {\n    }\n}\nF(a: bool): bool => a && Todo();\n\
         H(a: bool): bool => Todo() && a;\nK() {\n    let x = Todo();\n}\n",
    );
    for name in ["A", "B", "C", "D"] {
        assert_eq!(
            lowered.code(name),
            ["call Todo{external}() -> !;"],
            "{name}"
        );
    }
    assert_eq!(
        lowered.code("E"),
        ["goto bb1;", "call Todo{external}() -> !;"]
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(_2);",
            "switch copy (*a#0) [false -> bb2] otherwise bb1;",
            "call Todo{external}() -> !;",
            "_2 = false;",
            "goto bb3;",
            "_0 = copy _2;",
            "storage_dead(_2);",
            "return;",
        ]
    );
    assert_eq!(
        lowered.code("H"),
        ["storage_live(_2);", "call Todo{external}() -> !;"]
    );
    assert_eq!(
        lowered.dump("K"),
        "\
fn UsersApi.K() -> () {
    let _0: ();
    let x#0: !;

    bb0: {
        storage_live(x#0); // 18:9
        call Todo{external}() -> !; // 18:13
    }
}
"
    );
}

#[test]
fn struct_creation_is_an_aggregate_in_field_order() {
    let lowered = lowered(
        "struct P {\n    X: int;\n    Y: int;\n}\nG(): int => 1;\nH(): int => 2;\n\
         F(): P => new P { Y = 1, X = 2 };\nK(): P => new P { Y = G(), X = H() };\n",
    );
    assert_eq!(
        lowered.code("F"),
        ["_0 = UsersApi.P{struct UsersApi.P}(2, 1);", "return;"]
    );
    // The written values are evaluated in the order of the text, then
    // passed in the order of the fields.
    assert_eq!(
        lowered.code("K"),
        [
            "storage_live(_1);",
            "_1 = call UsersApi.G{fn UsersApi.G}() -> bb1;",
            "storage_live(_2);",
            "_2 = call UsersApi.H{fn UsersApi.H}() -> bb2;",
            "_0 = UsersApi.P{struct UsersApi.P}(copy _2, copy _1);",
            "storage_dead(_2);",
            "storage_dead(_1);",
            "return;",
        ]
    );
}

#[test]
fn omitted_field_calls_its_initializer() {
    let lowered = lowered(
        "struct P {\n    X: int;\n    Y: long = 7;\n    Z: int = 8;\n}\n\
         F(): P => new P { X = 1 };\nG(): P => new P { Z = 3, X = 1 };\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(_1);",
            "_1 = call UsersApi.P{struct UsersApi.P}.Y{init}() -> bb1;",
            "storage_live(_2);",
            "_2 = call UsersApi.P{struct UsersApi.P}.Z{init}() -> bb2;",
            "_0 = UsersApi.P{struct UsersApi.P}(1, copy _1, copy _2);",
            "storage_dead(_2);",
            "storage_dead(_1);",
            "return;",
        ]
    );
    assert_in_order(
        &lowered.code("G"),
        &[
            "_1 = call UsersApi.P{struct UsersApi.P}.Y{init}() -> bb1;",
            "_0 = UsersApi.P{struct UsersApi.P}(1, copy _1, 3);",
        ],
    );
    // The initializer is a body of its own, of the field's type, which
    // converts its value as inference records.
    let mut out = String::new();
    let init = lowered.mir_of(lowered.initializer("P", "Y"));
    write_mir(&mut out, &lowered.db, Some(lowered.file), init);
    assert_eq!(
        out,
        "\
fn UsersApi.P.Y() -> long {
    let _0: long;

    bb0: {
        _0 = 7; // 3:15
        return; // 3:15
    }
}
"
    );
}

#[test]
fn method_reads_fields_through_deref() {
    let lowered = lowered(
        "struct P {\n    X: int;\n    Y: int;\n    Sum(): int => X + Y;\n    \
         Both(): int => Sum() + this.X;\n}\nF(p: P): int => p.Sum();\n",
    );
    let dump = lowered.dump("Sum");
    assert!(
        dump.starts_with(
            "fn UsersApi.P.Sum(this: in P) -> int {\n    let _0: int;\n    let this: in P;\n"
        ),
        "{dump}"
    );
    assert_eq!(
        lowered.code("Sum"),
        ["_0 = copy (*this).X + copy (*this).Y;", "return;"]
    );
    // A method called inside another takes `this` on.
    assert_in_order(
        &lowered.code("Both"),
        &[
            "_2 = &in (*this);",
            "_3 = call UsersApi.P.Sum{fn UsersApi.P.Sum}(copy _2) -> bb1;",
            "_0 = copy _3 + copy (*this).X;",
        ],
    );
    // A call through a value borrows the receiver, its first argument.
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(_2);",
            "_2 = &in (*p#0);",
            "_0 = call UsersApi.P.Sum{fn UsersApi.P.Sum}(copy _2) -> bb1;",
            "storage_dead(_2);",
            "return;",
        ]
    );
}

#[test]
fn mut_method_writes_through_ref() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n    public mut Scale(k: int) { X *= k; }\n    \
         public move Take(): int => X;\n}\nF() {\n    var p = new P { X = 1 };\n    \
         p.Scale(2);\n    let n = p.Take();\n}\n",
    );
    let dump = lowered.dump("Scale");
    assert!(
        dump.starts_with("fn UsersApi.P.Scale(this: ref P, k#0: in int) -> () {"),
        "{dump}"
    );
    assert_eq!(
        lowered.code("Scale"),
        [
            "(*this).X = copy (*this).X * copy (*k#0);",
            "_0 = ();",
            "return;",
        ]
    );
    // A `move` method takes `this` by value.
    assert!(
        lowered
            .dump("Take")
            .starts_with("fn UsersApi.P.Take(this: P) -> int {"),
        "{}",
        lowered.dump("Take")
    );
    assert_eq!(lowered.code("Take"), ["_0 = copy this.X;", "return;"]);
    assert_in_order(
        &lowered.code("F"),
        &[
            "_3 = &ref p#0;",
            "call UsersApi.P.Scale{fn UsersApi.P.Scale}(copy _3, copy _5) -> bb1;",
            "n#1 = call UsersApi.P.Take{fn UsersApi.P.Take}(copy p#0) -> bb2;",
        ],
    );
}

#[test]
fn ref_argument_is_borrowed_ref() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n}\nBump(ref n: int) { n += 1; }\nF() {\n    \
         var x = 1;\n    Bump(ref x);\n    var p = new P { X = 1 };\n    Bump(ref p.X);\n}\n",
    );
    assert!(
        lowered
            .dump("Bump")
            .starts_with("fn UsersApi.Bump(n#0: ref int) -> () {"),
        "{}",
        lowered.dump("Bump")
    );
    assert_in_order(&lowered.code("Bump"), &["(*n#0) = copy (*n#0) + 1;"]);
    assert_in_order(
        &lowered.code("F"),
        &[
            "_3 = &ref x#0;",
            "call UsersApi.Bump{fn UsersApi.Bump}(copy _3) -> bb1;",
            "_4 = &ref p#1.X;",
            "call UsersApi.Bump{fn UsersApi.Bump}(copy _4) -> bb2;",
        ],
    );
}

#[test]
fn in_argument_is_borrowed_in() {
    let lowered = lowered(
        "Twice(x: int): int => x * 2;\nTake(move x: int): int => x;\nF(): int {\n    \
         let a = 3;\n    return Twice(a) + Take(a);\n}\n",
    );
    assert!(
        lowered
            .dump("Twice")
            .starts_with("fn UsersApi.Twice(x#0: in int) -> int {"),
        "{}",
        lowered.dump("Twice")
    );
    assert!(
        lowered
            .dump("Take")
            .starts_with("fn UsersApi.Take(x#0: int) -> int {"),
        "{}",
        lowered.dump("Take")
    );
    assert_in_order(
        &lowered.code("F"),
        &[
            "_2 = &in a#0;",
            "_3 = call UsersApi.Twice{fn UsersApi.Twice}(copy _2) -> bb1;",
            "_4 = call UsersApi.Take{fn UsersApi.Take}(copy a#0) -> bb2;",
        ],
    );
}

#[test]
fn compound_assignment_on_field_evaluates_place_once() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n}\nG(): int => 1;\nF() {\n    \
         var p = new P { X = 1 };\n    p.X += G();\n    p.X++;\n}\n",
    );
    let code = lowered.code("F");
    assert_in_order(
        &code,
        &[
            "storage_live(_2);",
            "_2 = copy p#0.X;",
            "storage_live(_3);",
            "_3 = call UsersApi.G{fn UsersApi.G}() -> bb1;",
            "p#0.X = copy _2 + copy _3;",
            "storage_dead(_3);",
            "storage_dead(_2);",
            "p#0.X = copy p#0.X + 1;",
        ],
    );
    let calls = code.iter().filter(|line| line.contains("call ")).count();
    assert_eq!(calls, 1, "{code:?}");
}

#[test]
fn local_function_capture_is_lifted() {
    let lowered = lowered(
        "F(n: int): int {\n    var total = 0;\n    Add(k: int) { total += k * n; }\n    \
         Add(1);\n    Add(2);\n    return total;\n}\n",
    );
    assert_eq!(lowered.bodies("F"), ["UsersApi.F", "UsersApi.F.Add"]);
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir("F"));
    let add = out
        .split("\n\n")
        .filter(|part| part.starts_with("fn UsersApi.F.Add"))
        .count();
    assert_eq!(add, 1, "{out}");
    assert!(
        out.contains("fn UsersApi.F.Add(k#3: in int, n#0: in int, total#1: ref int) -> () {"),
        "{out}"
    );
    let mir = lowered.mir("F");
    assert_in_order(
        &body_code(&lowered, "F", 0),
        &[
            "_3 = 1;",
            "_4 = &in _3;",
            "_5 = &in (*n#0);",
            "_6 = &ref total#1;",
            "call UsersApi.F.Add(copy _4, copy _5, copy _6) -> bb1;",
            "call UsersApi.F.Add(copy _8, copy _9, copy _10) -> bb2;",
        ],
    );
    let TerminatorKind::Call { callee, .. } = &mir.bodies[0].blocks[0].terminator.kind else {
        panic!("a call ends bb0");
    };
    assert_eq!(*callee, Callee::Local(1));
    assert_in_order(
        &body_code(&lowered, "F", 1),
        &[
            "_4 = copy (*k#3) * copy (*n#0);",
            "(*total#1) = copy (*total#1) + copy _4;",
            "_0 = ();",
            "return;",
        ],
    );
}

#[test]
fn local_function_bodies_follow_owner_in_text_order() {
    let lowered = lowered(
        "F(): int {\n    A(): int => 1;\n    B(): int {\n        C(): int => 2;\n        \
         return C() + A();\n    }\n    D(): int => 3;\n    return B() + D();\n}\n",
    );
    assert_eq!(
        lowered.bodies("F"),
        [
            "UsersApi.F",
            "UsersApi.F.A",
            "UsersApi.F.B",
            "UsersApi.F.B.C",
            "UsersApi.F.D"
        ]
    );
    let mir = lowered.mir("F");
    let callees = |body: usize| -> Vec<Callee> {
        mir.bodies[body]
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator.kind {
                TerminatorKind::Call { callee, .. } => Some(callee.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(callees(0), [Callee::Local(2), Callee::Local(4)]);
    assert_eq!(callees(2), [Callee::Local(3), Callee::Local(1)]);
}

#[test]
fn a_member_index_or_method_of_a_diverging_value_diverges() {
    let lowered = lowered(
        "A(): int => Todo().Length;\nB(): int => Todo()[0];\nC() {\n    Todo().Foo();\n}\n\
         D() {\n    let x: int = Todo().X.Y;\n}\n",
    );
    for name in ["A", "B", "C"] {
        assert_eq!(
            lowered.code(name),
            ["call Todo{external}() -> !;"],
            "{name}"
        );
    }
    assert_eq!(
        lowered.code("D"),
        ["storage_live(x#0);", "call Todo{external}() -> !;"]
    );
}

#[test]
fn every_owner_of_the_run_tests_lowers_and_validates() {
    let files = [
        include_str!("../../../../tests/run/arithmetic.mr"),
        include_str!("../../../../tests/run/booleans.mr"),
        include_str!("../../../../tests/run/control_flow.mr"),
        include_str!("../../../../tests/run/float_target.mr"),
        include_str!("../../../../tests/run/functions.mr"),
        include_str!("../../../../tests/run/hello.mr"),
        include_str!("../../../../tests/run/literal_values.mr"),
        include_str!("../../../../tests/run/locals.mr"),
        include_str!("../../../../tests/run/promotion.mr"),
        include_str!("../../../../tests/run/structs.mr"),
    ];
    for text in files {
        let lowered = lowered(text);
        let dump = mir_dump(&lowered.db, lowered.project, lowered.file);
        assert!(!dump.is_empty());
        for owner in body_owners(&lowered.db, lowered.file) {
            if body(&lowered.db, lowered.file, owner).root.is_some() {
                lowered.mir_of(owner);
            }
        }
    }
    let functions = lowered(include_str!("../../../../tests/run/functions.mr"));
    assert_eq!(
        functions.bodies("Factorial"),
        ["UsersApi.Factorial", "UsersApi.Factorial.Step"]
    );
    // `strings.mr` holds `name.Length` and a `foreach` over an array, which
    // are not part of the language lowered here; its other owners are.
    let strings = lowered(include_str!("../../../../tests/run/strings.mr"));
    for owner in body_owners(&strings.db, strings.file) {
        if owner != BodyOwner::TopLevel && body(&strings.db, strings.file, owner).root.is_some() {
            strings.mir_of(owner);
        }
    }
}

/// The code of the body at `index` in the MIR of `name`, as
/// [`Lowered::code`] gives it, with calls of local functions by name.
fn body_code(lowered: &Lowered, name: &str, index: usize) -> Vec<String> {
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir(name));
    let mut bodies: Vec<String> = Vec::new();
    for line in out.lines() {
        if line.starts_with("fn ") {
            bodies.push(String::new());
        }
        if let Some(body) = bodies.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    code(&bodies[index])
}

#[test]
fn a_capture_takes_the_borrow_its_uses_need() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n    public mut Scale(k: int) { X *= k; }\n    \
         public Get(): int => X;\n}\nBump(ref n: int) { n += 1; }\nF(): int {\n    \
         var a = 0;\n    var b = 0;\n    var c = 0;\n    var d = 0;\n    \
         var p = new P { X = 1 };\n    var q = new P { X = 1 };\n    var r = new P { X = 1 };\n    \
         G() {\n        a++;\n        ++b;\n        Bump(ref c);\n        p.Scale(2);\n        \
         q.X = 3;\n        let n = r.Get() + d;\n    }\n    \
         G();\n    return a;\n}\n",
    );
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir("F"));
    assert!(
        out.contains(
            "fn UsersApi.F.G(a#0: ref int, b#1: ref int, c#2: ref int, d#3: in int, \
             p#4: ref P, q#5: ref P, r#6: in P) -> () {"
        ),
        "{out}"
    );
}

#[test]
fn a_local_function_takes_what_the_ones_it_calls_take() {
    // `A` calls `B`, which calls `C`: what `C` assigns reaches `A` too.
    // `A` reads `total` itself and still takes it by `ref`. `E` declares
    // the `count` that `D` uses, so it takes nothing.
    let lowered = lowered(
        "F(): int {\n    var total = 0;\n    var other = 0;\n    \
         C() {\n        total += 1;\n        other += 0;\n    }\n    \
         B() {\n        C();\n        let o = other;\n    }\n    \
         A() {\n        let t = total;\n        B();\n    }\n    \
         E() {\n        var count = 1;\n        D() { count += 1; }\n        D();\n    }\n    \
         A();\n    E();\n    return total;\n}\n",
    );
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir("F"));
    let headers: Vec<&str> = out.lines().filter(|line| line.starts_with("fn ")).collect();
    assert_eq!(
        headers,
        [
            "fn UsersApi.F() -> int {",
            "fn UsersApi.F.C(total#0: ref int, other#1: ref int) -> () {",
            "fn UsersApi.F.B(total#0: ref int, other#1: ref int) -> () {",
            "fn UsersApi.F.A(total#0: ref int, other#1: ref int) -> () {",
            "fn UsersApi.F.E() -> () {",
            "fn UsersApi.F.E.D(count#8: ref int) -> () {",
        ]
    );
}

#[test]
fn a_local_function_inside_a_local_function_takes_its_parameters() {
    let lowered = lowered(
        "F(n: int): int {\n    G(k: int): int {\n        H(): int => k + n;\n        \
         return H();\n    }\n    return G(1);\n}\n",
    );
    assert_eq!(
        lowered.bodies("F"),
        ["UsersApi.F", "UsersApi.F.G", "UsersApi.F.G.H"]
    );
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir("F"));
    assert!(
        out.contains("fn UsersApi.F.G(k#2: in int, n#0: in int) -> int {"),
        "{out}"
    );
    assert!(
        out.contains("fn UsersApi.F.G.H(n#0: in int, k#2: in int) -> int {"),
        "{out}"
    );
    assert_in_order(
        &body_code(&lowered, "F", 1),
        &[
            "_3 = &in (*n#0);",
            "_4 = &in (*k#2);",
            "_0 = call UsersApi.F.G.H(copy _3, copy _4) -> bb1;",
        ],
    );
}

#[test]
fn a_local_function_takes_its_default_arguments() {
    let lowered = lowered("F(): int {\n    G(k: int = 5): int => k;\n    return G();\n}\n");
    assert_in_order(
        &lowered.code("F"),
        &[
            "_1 = 5;",
            "_2 = &in _1;",
            "_0 = call UsersApi.F.G(copy _2) -> bb1;",
        ],
    );
}

#[test]
fn a_method_named_alone_takes_this() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n    Plus(k: int): int => X + k;\n    \
         mut Add(k: int) { X += k; }\n    A(): int => Plus(G());\n    \
         mut B() { Add(G()); }\n}\nG(): int => 1;\n",
    );
    // An `in` receiver is read before an argument that may change it.
    assert_in_order(
        &lowered.code("A"),
        &[
            "_2 = copy (*this);",
            "_3 = &in _2;",
            "_4 = call UsersApi.G{fn UsersApi.G}() -> bb1;",
            "_5 = &in _4;",
            "_0 = call UsersApi.P.Plus{fn UsersApi.P.Plus}(copy _3, copy _5) -> bb2;",
        ],
    );
    // A `ref` receiver is the place itself.
    assert_in_order(
        &lowered.code("B"),
        &[
            "_2 = &ref (*this);",
            "_3 = call UsersApi.G{fn UsersApi.G}() -> bb1;",
            "_4 = &in _3;",
            "call UsersApi.P.Add{fn UsersApi.P.Add}(copy _2, copy _4) -> bb2;",
        ],
    );
}

#[test]
fn a_field_of_a_field_and_of_a_value_are_places() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n}\nstruct Q {\n    public Inner: P;\n    \
         Get(): int => Inner.X;\n}\nMake(): P => new P { X = 1 };\nF(): int => Make().X;\n\
         H(q: Q): int => q.Inner.X;\n",
    );
    assert_eq!(
        lowered.code("Get"),
        ["_0 = copy (*this).Inner.X;", "return;"]
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(_1);",
            "_1 = call UsersApi.Make{fn UsersApi.Make}() -> bb1;",
            "_0 = copy _1.X;",
            "storage_dead(_1);",
            "return;",
        ]
    );
    assert_eq!(lowered.code("H"), ["_0 = copy (*q#0).Inner.X;", "return;"]);
}

#[test]
fn fields_and_structs_without_effects_keep_earlier_places() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n    Two(a: int, b: int): int => a;\n    \
         M(k: int): int => Two(k, X);\n    N(k: int): int => Two(k, this.X);\n}\n\
         G(): int => 1;\nT(a: int, b: int): int => a;\nH(a: int, b: P): int => a;\n\
         K(x: int, p: P): int => T(x, p.X);\nL(x: int): int => H(x, new P { X = 1 });\n\
         W(x: int): int => H(x, new P { X = G() });\n",
    );
    for (name, borrowed) in [("K", "_3 = &in (*x#0);"), ("L", "_2 = &in (*x#0);")] {
        assert_in_order(&lowered.code(name), &[borrowed]);
    }
    for name in ["M", "N"] {
        let code = lowered.code(name);
        assert!(
            code.iter().any(|line| line == "_4 = &in (*k#0);"),
            "{name}: {code:?}"
        );
    }
    // A struct with a value that calls is evaluated after a copy of `x`.
    assert_in_order(&lowered.code("W"), &["_2 = copy (*x#0);", "_3 = &in _2;"]);
}

#[test]
fn a_step_of_a_field_whose_value_is_used_keeps_the_old_value() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n}\nF(): int {\n    var p = new P { X = 1 };\n    \
         let old = p.X++;\n    return old;\n}\n",
    );
    assert_in_order(
        &lowered.code("F"),
        &[
            "_3 = copy p#0.X;",
            "p#0.X = copy p#0.X + 1;",
            "old#1 = copy _3;",
        ],
    );
}

#[test]
fn fields_are_numbered_among_the_instance_fields() {
    // A constant and a static field belong to the type: a value holds `X`
    // and `Y` only, as fields 0 and 1.
    let lowered = lowered(
        "struct P {\n    public const K: int = 1;\n    public X: int;\n    \
         public static S: int = 2;\n    public Y: int = 3;\n}\n\
         F(): P => new P { X = 1 };\nG(p: P): int => p.Y;\n",
    );
    assert_eq!(
        lowered.code("F"),
        [
            "storage_live(_1);",
            "_1 = call UsersApi.P{struct UsersApi.P}.Y{init}() -> bb1;",
            "_0 = UsersApi.P{struct UsersApi.P}(1, copy _1);",
            "storage_dead(_1);",
            "return;",
        ]
    );
    let TerminatorKind::Call { callee, .. } = &lowered.mir("F").bodies[0].blocks[0].terminator.kind
    else {
        panic!("a call");
    };
    assert_eq!(*callee, Callee::Initializer(lowered.item("P"), 1));
    let g = &lowered.mir("G").bodies[0];
    let projections: Vec<&[Projection]> = g
        .blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|statement| match &statement.kind {
            StatementKind::Assign(_, Rvalue::Use(Operand::Copy(place))) => {
                Some(place.projections.as_slice())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        projections,
        [&[Projection::Deref, Projection::Field(1)][..]]
    );
}

#[test]
fn an_initializer_that_uses_an_instance_member_stops_in_the_front_end() {
    let db = TestDb::new();
    let project = app(
        &db,
        &[(
            "src/Program.mr",
            "struct P {\n    public X: int = 1;\n    public Y: int = X;\n}\n\
             F(): P => new P();\n",
        )],
    );
    assert_eq!(codes(diagnostics(&db, project)), ["MR0152"]);
}

#[test]
fn a_nested_type_that_uses_the_type_around_it_stops_in_the_front_end() {
    let db = TestDb::new();
    let project = app(
        &db,
        &[(
            "src/Program.mr",
            "struct P {\n    public X: int;\n    struct In {\n        R(): int => X;\n    }\n}\n",
        )],
    );
    assert_eq!(codes(diagnostics(&db, project)), ["MR0149"]);
}

#[test]
fn an_initializer_of_plain_values_lowers() {
    let lowered = lowered(
        "struct P {\n    Z: int;\n    public Y: int = 2 + 3;\n}\n\
         F(): P => new P { Z = 1 };\n",
    );
    assert_in_order(
        &lowered.code("F"),
        &["_1 = call UsersApi.P{struct UsersApi.P}.Y{init}() -> bb1;"],
    );
    let init = lowered.mir_of(lowered.initializer("P", "Y"));
    assert_eq!(init.bodies.len(), 1);
}

#[test]
#[should_panic(expected = "a static or constant member is not lowered (M1.11)")]
fn a_constant_in_an_initializer_is_not_lowered_yet() {
    let lowered = lowered(
        "struct P {\n    const K: int = 4;\n    public X: int = K + 1;\n}\n\
         F(): P => new P();\n",
    );
    lowered.mir_of(lowered.initializer("P", "X"));
}

#[test]
#[should_panic(expected = "only the initializer of a field is lowered")]
fn a_getter_is_not_lowered() {
    let lowered = lowered("struct P {\n    public X: int;\n    public Get: int => X;\n}\n");
    lowered.mir_of(lowered.initializer("P", "Get"));
}

#[test]
#[should_panic(expected = "a method of a type other than a struct is not lowered")]
fn a_method_of_a_class_is_not_lowered() {
    let lowered = lowered("class C {\n    M() {}\n}\n");
    lowered.mir("M");
}

#[test]
#[should_panic(expected = "a member that is not a field of a struct is not lowered")]
fn a_getter_read_is_not_lowered() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n    public Get: int => X;\n}\nF(p: P): int => p.Get;\n",
    );
    lowered.mir("F");
}

#[test]
#[should_panic(expected = "only the initializer of a field is lowered")]
fn the_initializer_of_a_static_field_is_not_lowered() {
    let lowered = lowered("struct P {\n    public X: int;\n    public static S: int = 1;\n}\n");
    lowered.mir_of(lowered.initializer("P", "S"));
}

#[test]
#[should_panic(expected = "a static or constant member is not lowered (M1.11)")]
fn a_static_field_named_in_a_method_is_not_lowered() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n    public static S: int = 1;\n    \
         Get(): int => S;\n}\n",
    );
    lowered.mir("Get");
}

#[test]
fn local_function_parameters_take_their_modes() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n}\nF(): int {\n    var x = 1;\n    \
         G(ref k: int) { k += 1; }\n    G(ref x);\n    let p = new P { X = 1 };\n    \
         H(move q: P): int => q.X;\n    let a = H(p);\n    M(move q: int): int => q;\n    \
         return a + M(move x);\n}\n",
    );
    let mut out = String::new();
    write_mir(&mut out, &lowered.db, Some(lowered.file), lowered.mir("F"));
    let headers: Vec<&str> = out.lines().filter(|line| line.starts_with("fn ")).collect();
    assert_eq!(
        headers,
        [
            "fn UsersApi.F() -> int {",
            "fn UsersApi.F.G(k#2: ref int) -> () {",
            "fn UsersApi.F.H(q#5: P) -> int {",
            "fn UsersApi.F.M(q#8: int) -> int {",
        ]
    );
    assert_in_order(&body_code(&lowered, "F", 1), &["(*k#2) = copy (*k#2) + 1;"]);
    assert_eq!(body_code(&lowered, "F", 2), ["_0 = copy q#5.X;", "return;"]);
    assert_in_order(
        &body_code(&lowered, "F", 0),
        &[
            "_4 = &ref x#0;",
            "call UsersApi.F.G(copy _4) -> bb1;",
            "a#6 = call UsersApi.F.H(copy p#3) -> bb2;",
            "_6 = call UsersApi.F.M(copy x#0) -> bb3;",
        ],
    );
}

#[test]
fn this_in_a_local_function_is_lifted() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n    \
         Get(): int {\n        G(): int => X;\n        return G();\n    }\n    \
         mut Set() {\n        H() { X = 2; }\n        H();\n    }\n    \
         mut Look(): int {\n        K(): int => X;\n        return K();\n    }\n}\n",
    );
    assert_eq!(
        lowered.bodies("Get"),
        ["UsersApi.P.Get", "UsersApi.P.Get.G"]
    );
    // A `mut` method's local function that only reads takes `this` as `in`.
    let mut out = String::new();
    write_mir(
        &mut out,
        &lowered.db,
        Some(lowered.file),
        lowered.mir("Look"),
    );
    assert!(
        out.contains("fn UsersApi.P.Look.K(this: in P) -> int {"),
        "{out}"
    );
    assert_in_order(&body_code(&lowered, "Look", 0), &["_2 = &in (*this);"]);
    let mut out = String::new();
    write_mir(
        &mut out,
        &lowered.db,
        Some(lowered.file),
        lowered.mir("Get"),
    );
    assert!(
        out.contains("fn UsersApi.P.Get.G(this: in P) -> int {"),
        "{out}"
    );
    assert_eq!(
        body_code(&lowered, "Get", 1),
        ["_0 = copy (*this).X;", "return;"]
    );
    assert_in_order(
        &body_code(&lowered, "Get", 0),
        &[
            "_2 = &in (*this);",
            "_0 = call UsersApi.P.Get.G(copy _2) -> bb1;",
        ],
    );
    let mut out = String::new();
    write_mir(
        &mut out,
        &lowered.db,
        Some(lowered.file),
        lowered.mir("Set"),
    );
    assert!(
        out.contains("fn UsersApi.P.Set.H(this: ref P) -> () {"),
        "{out}"
    );
    assert_in_order(&body_code(&lowered, "Set", 1), &["(*this).X = 2;"]);
    assert_in_order(
        &body_code(&lowered, "Set", 0),
        &[
            "_2 = &ref (*this);",
            "call UsersApi.P.Set.H(copy _2) -> bb1;",
        ],
    );
}

#[test]
#[should_panic(expected = "a static or constant member is not lowered (M1.11)")]
fn a_constant_read_through_a_value_is_not_lowered() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n    public const K: int = 2;\n}\nF(p: P): int => p.K;\n",
    );
    lowered.mir("F");
}

#[test]
#[should_panic(expected = "a static or constant member is not lowered (M1.11)")]
fn a_static_read_through_this_is_not_lowered() {
    let lowered = lowered(
        "struct P {\n    public X: int;\n    public static mut N: int = 2;\n    \
         Get(): int => this.N;\n}\n",
    );
    lowered.mir("Get");
}

#[test]
fn local_functions_of_one_name_get_their_own_names() {
    let lowered = lowered(
        "F(a: bool): int {\n    if (a) {\n        G(): int => 1;\n        return G();\n    } else {\n        \
         G(): int => 2;\n        return G();\n    }\n}\n",
    );
    assert_eq!(
        lowered.bodies("F"),
        ["UsersApi.F", "UsersApi.F.G", "UsersApi.F.G#2"]
    );
    assert_in_order(
        &body_code(&lowered, "F", 0),
        &[
            "_0 = call UsersApi.F.G() -> bb3;",
            "_0 = call UsersApi.F.G#2() -> bb4;",
        ],
    );
}

#[test]
#[should_panic(expected = "a method of a generic struct is not lowered")]
fn a_method_of_a_generic_struct_is_not_lowered() {
    let lowered = lowered("struct B<T> {\n    M() {}\n}\n");
    lowered.mir("M");
}

#[test]
#[should_panic(expected = "a call of a generic or async local function is not lowered")]
fn a_call_of_a_generic_local_function_is_not_lowered() {
    let lowered = lowered("F(): int {\n    Id<T>(move x: T): T => x;\n    return Id(1);\n}\n");
    lowered.mir("F");
}

#[test]
fn blocks_are_numbered_without_gaps() {
    // The join of the `if` is never reached, as both arms return, so
    // lowering drops it and the blocks after it move down.
    let lowered = lowered(
        "F(n: int): int {\n    if (n < 2) {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n\
         G(n: int): int {\n    if (n < 2) {\n        return 1;\n    }\n    return 2;\n}\n",
    );
    for name in ["F", "G"] {
        let dump = lowered.dump(name);
        let body = &lowered.mir(name).bodies[0];
        for index in 0..body.blocks.len() {
            assert!(dump.contains(&format!("bb{index}: {{")), "{dump}");
        }
        assert!(
            !dump.contains(&format!("bb{}", body.blocks.len())),
            "{dump}"
        );
        assert!(body.blocks.iter().all(|block| {
            block
                .terminator
                .kind
                .successors()
                .iter()
                .all(|target| target.index() < body.blocks.len())
        }));
    }
    assert_eq!(lowered.mir("F").bodies[0].blocks.len(), 3);
}

mod numbering {
    use mora_span::{FileId, Span, TextRange, TextSize};

    use super::super::number_densely;
    use crate::{
        Block, BlockId, Body, Const, Operand, Statement, StatementKind, Terminator, TerminatorKind,
    };

    fn span() -> Span {
        Span::new(FileId::new(0), TextRange::empty(TextSize::new(0)))
    }

    fn block(statements: usize, kind: TerminatorKind) -> Block {
        Block {
            statements: (0..statements)
                .map(|_| Statement {
                    kind: StatementKind::Nop,
                    span: span(),
                })
                .collect(),
            terminator: Terminator { kind, span: span() },
        }
    }

    fn body(blocks: Vec<Block>) -> Body {
        Body {
            name: "F".to_owned(),
            params: 0,
            locals: Vec::new(),
            blocks,
            span: span(),
        }
    }

    fn kinds(body: &Body) -> Vec<&TerminatorKind> {
        body.blocks.iter().map(|b| &b.terminator.kind).collect()
    }

    #[test]
    fn only_empty_unreachable_blocks_nothing_jumps_to_are_dropped() {
        let switch = TerminatorKind::Switch {
            discr: Operand::Const(Const::Bool(true)),
            targets: vec![(Const::Bool(true), BlockId(2))],
            otherwise: BlockId(4),
        };
        let mut body = body(vec![
            block(0, switch),
            // Holds a statement: stays.
            block(1, TerminatorKind::Unreachable),
            block(0, TerminatorKind::Goto(BlockId(5))),
            // Empty, ends in `unreachable`, nothing jumps to it: dropped.
            block(0, TerminatorKind::Unreachable),
            // Empty and ends in `unreachable`, but a switch jumps to it.
            block(0, TerminatorKind::Unreachable),
            // Empty, ends in something else: stays.
            block(0, TerminatorKind::Return),
        ]);
        number_densely(&mut body);
        assert_eq!(
            kinds(&body),
            [
                &TerminatorKind::Switch {
                    discr: Operand::Const(Const::Bool(true)),
                    targets: vec![(Const::Bool(true), BlockId(2))],
                    otherwise: BlockId(3),
                },
                &TerminatorKind::Unreachable,
                &TerminatorKind::Goto(BlockId(4)),
                &TerminatorKind::Unreachable,
                &TerminatorKind::Return,
            ]
        );
        assert_eq!(body.blocks[1].statements.len(), 1);
    }

    #[test]
    fn the_entry_block_stays_even_when_it_is_empty_and_unreachable() {
        let mut body = body(vec![
            block(0, TerminatorKind::Unreachable),
            block(0, TerminatorKind::Return),
        ]);
        number_densely(&mut body);
        assert_eq!(
            kinds(&body),
            [&TerminatorKind::Unreachable, &TerminatorKind::Return]
        );
    }
}

#[test]
#[should_panic(expected = "the parameter `xs` has a type that is not lowered")]
fn a_parameter_of_no_concrete_type_is_an_internal_error() {
    let lowered = lowered("F(xs: int[]) {}\n");
    lowered.mir("F");
}

#[test]
#[should_panic(expected = "the return type is not lowered")]
fn a_return_type_that_is_not_concrete_is_an_internal_error() {
    let lowered = lowered("F(): int[] => Todo();\n");
    lowered.mir("F");
}

#[test]
fn a_call_reads_the_body_of_its_callee_only_for_a_default() {
    use salsa::Setter;

    let mut db = TestDb::new();
    let callee = |n: u32| format!("G(x: int, y: int = 2): int => x + {n};\n");
    let project = app(
        &db,
        &[
            (
                "src/Program.mr",
                "F(): int => G(1, 2);\nH(): int => G(1);\n",
            ),
            ("src/Other.mr", &callee(1)),
        ],
    );
    let (f, h, file) = {
        let lowered = in_project(db.clone(), project);
        (lowered.owner("F"), lowered.owner("H"), lowered.file)
    };
    let other = file_named(&db, project, "src/Other.mr");
    mir(&db, project, file, f);
    mir(&db, project, file, h);
    db.take_executed();
    other.set_text(&mut db).to(callee(12345));
    mir(&db, project, file, f);
    mir(&db, project, file, h);
    let lowered: Vec<String> = db
        .take_executed()
        .into_iter()
        .filter(|key| key.starts_with("mir("))
        .collect();
    // An edit inside `G` lowers again only the caller that reads its
    // default value.
    assert_eq!(lowered.len(), 1, "{lowered:?}");
}

#[test]
fn the_dump_docs_show_a_real_body() {
    let lowered = lowered(
        "Clamp(n: int): int {\n    var acc = n;\n    if (acc < 0) {\n        acc = 0;\n    }\n    \
         return acc;\n}\n",
    );
    let docs: String = include_str!("../dump.rs")
        .lines()
        .map_while(|line| line.strip_prefix("//!"))
        .map(|line| format!("{}\n", line.strip_prefix(' ').unwrap_or(line)))
        .collect();
    let example = docs
        .split_once("```text\n")
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(example, _)| example)
        .expect("the example of the module docs");
    assert_eq!(example, lowered.dump("Clamp"));
}

#[test]
fn a_conditional_of_assignments_is_a_statement() {
    let lowered = lowered("F(c: bool) {\n    var x = 0;\n    c ? x = 1 : x = 2;\n}\n");
    assert_in_order(
        &lowered.code("F"),
        &[
            "switch copy (*c#0) [false -> bb2] otherwise bb1;",
            "x#1 = 1;",
            "goto bb3;",
            "x#1 = 2;",
            "goto bb3;",
        ],
    );
}

#[test]
fn a_conditional_of_assignments_in_a_local_function_is_a_statement() {
    let lowered = lowered(
        "F(c: bool) {\n    var x = 0;\n    var y = 0;\n    G() => c ? x = 1 : y = 2;\n    G();\n}\n",
    );
    assert_eq!(lowered.bodies("F").len(), 2);
    let code = body_code(&lowered, "F", 1);
    assert!(code.iter().any(|l| l.contains("= 1;")), "{code:?}");
    assert!(code.iter().any(|l| l.contains("= 2;")), "{code:?}");
}

#[test]
fn a_conditional_of_writes_is_a_statement() {
    let lowered =
        lowered("F(c: bool) {\n    c ? Console.WriteLine(\"a\") : Console.WriteLine(\"b\");\n}\n");
    assert_in_order(
        &lowered.code("F"),
        &[
            "call Console.WriteLine{external}(\"a\") -> bb3;",
            "call Console.WriteLine{external}(\"b\") -> bb5;",
        ],
    );
}

#[test]
fn a_conditional_statement_ends_the_temporaries_of_each_branch() {
    let lowered = lowered(
        "F(c: bool) {\n    c ? A(G() + 1) : B();\n}\nA(x: int) {}\nB() {}\nG(): int => 1;\n",
    );
    // The `then` branch ends its own temporaries before the join; the
    // `else` branch made none, and the validator accepts the join.
    assert_in_order(
        &lowered.code("F"),
        &[
            "storage_live(_2);",
            "call UsersApi.A{fn UsersApi.A}(copy _4) -> bb4;",
            "storage_dead(_4);",
            "storage_dead(_3);",
            "storage_dead(_2);",
            "goto bb5;",
        ],
    );
}

#[test]
fn a_conditional_of_calls_is_a_statement() {
    let lowered = lowered("F(c: bool) {\n    c ? A() : B();\n}\nA() {}\nB() {}\n");
    let code = lowered.code("F");
    assert!(code.iter().any(|l| l.contains("UsersApi.A")), "{code:?}");
    assert!(code.iter().any(|l| l.contains("UsersApi.B")), "{code:?}");
}

#[test]
fn a_captured_this_is_borrowed_after_the_arguments() {
    let lowered = lowered(
        "struct P {\n    public mut X: int;\n    mut Inc(): int { X += 1; return X; }\n    \
         mut Run(): int {\n        var n = 1;\n        \
         G(k: int): int => X + k + n;\n        return G(Inc() + n++);\n    }\n}\n",
    );
    let code = body_code(&lowered, "Run", 0);
    let inc = code
        .iter()
        .position(|l| l.contains("P.Inc"))
        .expect("the argument call");
    let call = code
        .iter()
        .position(|l| l.contains("P.Run.G"))
        .expect("the local call");
    let borrow = code
        .iter()
        .position(|l| l.ends_with("&in (*this);"))
        .unwrap_or_else(|| panic!("{code:#?}"));
    assert!(inc < borrow && borrow < call, "{code:#?}");
    assert!(code[call].contains("(copy _"), "{code:#?}");
}
