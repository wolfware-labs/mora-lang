//! Tests of the MIR data types.

use mora_hir::test_support::{TestDb, app, file_named};
use mora_hir::{ItemId, Primitive, Ty, item_tree};
use mora_span::{FileId, Span, TextRange, TextSize};

use crate::{
    Block, BlockId, Body, BorrowMode, Const, LocalDecl, LocalId, LocalTy, Operand, Place,
    Projection, Rvalue, Statement, StatementKind, Terminator, TerminatorKind,
};

fn span() -> Span {
    Span::new(FileId::new(0), TextRange::empty(TextSize::new(0)))
}

fn decl(ty: LocalTy) -> LocalDecl {
    LocalDecl {
        ty,
        mutable: false,
        source: None,
    }
}

fn int() -> Ty {
    Ty::Primitive(Primitive::Int)
}

fn body_of(locals: Vec<LocalDecl>, blocks: Vec<Block>, params: usize) -> Body {
    Body {
        name: "F".into(),
        params,
        locals,
        blocks,
        span: span(),
    }
}

fn goto(target: BlockId) -> Terminator {
    Terminator {
        kind: TerminatorKind::Goto(target),
        span: span(),
    }
}

fn ret() -> Terminator {
    Terminator {
        kind: TerminatorKind::Return,
        span: span(),
    }
}

/// A struct `P { A: int; B: bool }` and its item.
fn point(db: &TestDb) -> ItemId {
    let project = app(
        db,
        &[(
            "src/Program.mr",
            "struct P {\n    public A: int;\n    public B: bool;\n}\n",
        )],
    );
    let file = file_named(db, project, "src/Program.mr");
    let tree = item_tree(db, file);
    let (item, _) = tree.items().next().expect("the struct");
    ItemId {
        project,
        file,
        item,
    }
}

#[test]
fn body_constructors_round_trip() {
    let one = Statement {
        kind: StatementKind::Assign(
            Place::local(LocalId::RETURN),
            Rvalue::Use(Operand::Const(Const::Int(1, Primitive::Int))),
        ),
        span: span(),
    };
    let body = body_of(
        vec![decl(LocalTy::Value(Ty::UNIT))],
        vec![
            Block {
                statements: vec![one.clone()],
                terminator: goto(BlockId(1)),
            },
            Block {
                statements: vec![],
                terminator: ret(),
            },
        ],
        0,
    );
    assert_eq!(body.local(LocalId::RETURN).ty, LocalTy::Value(Ty::UNIT));
    assert_eq!(BlockId::ENTRY, BlockId(0));
    assert_eq!(body.block(BlockId::ENTRY).statements, vec![one]);
    assert_eq!(body.block(BlockId(1)).terminator, ret());
    assert_eq!(LocalId::RETURN, LocalId(0));
}

#[test]
fn indexes_expose_their_position() {
    assert_eq!(LocalId(3).index(), 3);
    assert_eq!(BlockId(5).index(), 5);
    assert_ne!(LocalId(1), LocalId(2));
}

#[test]
fn a_place_without_projections_has_its_local_type() {
    let db = TestDb::new();
    let body = body_of(
        vec![decl(LocalTy::Value(Ty::UNIT)), decl(LocalTy::Value(int()))],
        vec![],
        0,
    );
    assert_eq!(body.place_ty(&Place::local(LocalId(1)), &db), int());
    assert_eq!(body.place_ty(&Place::local(LocalId::RETURN), &db), Ty::UNIT);
}

#[test]
fn deref_unwraps_a_borrow_and_field_reads_the_struct() {
    let db = TestDb::new();
    let p = point(&db);
    let body = body_of(
        vec![
            decl(LocalTy::Value(Ty::UNIT)),
            decl(LocalTy::Borrow(BorrowMode::In, Ty::Struct(p))),
            decl(LocalTy::Value(Ty::Struct(p))),
        ],
        vec![],
        2,
    );
    let place = |local: u32, projections: Vec<Projection>| Place {
        local: LocalId(local),
        projections,
    };
    assert_eq!(
        body.place_ty(&place(1, vec![Projection::Deref]), &db),
        Ty::Struct(p)
    );
    assert_eq!(
        body.place_ty(&place(2, vec![Projection::Field(0)]), &db),
        int()
    );
    assert_eq!(
        body.place_ty(&place(2, vec![Projection::Field(1)]), &db),
        Ty::Primitive(Primitive::Bool)
    );
    assert_eq!(
        body.place_ty(
            &place(1, vec![Projection::Deref, Projection::Field(1)]),
            &db
        ),
        Ty::Primitive(Primitive::Bool)
    );
}

#[test]
#[should_panic(expected = "dereference of a value local")]
fn deref_of_a_value_local_panics() {
    let db = TestDb::new();
    let body = body_of(vec![decl(LocalTy::Value(int()))], vec![], 0);
    body.place_ty(
        &Place {
            local: LocalId(0),
            projections: vec![Projection::Deref],
        },
        &db,
    );
}

#[test]
#[should_panic(expected = "no local _7")]
fn a_missing_local_panics() {
    let db = TestDb::new();
    let body = body_of(vec![decl(LocalTy::Value(int()))], vec![], 0);
    body.place_ty(&Place::local(LocalId(7)), &db);
}

#[test]
#[should_panic(expected = "field 2 out of range")]
fn a_field_out_of_range_panics() {
    let db = TestDb::new();
    let p = point(&db);
    let body = body_of(vec![decl(LocalTy::Value(Ty::Struct(p)))], vec![], 0);
    body.place_ty(
        &Place {
            local: LocalId(0),
            projections: vec![Projection::Field(2)],
        },
        &db,
    );
}

#[test]
#[should_panic(expected = "field of a type that is not a struct")]
fn a_field_of_a_non_struct_panics() {
    let db = TestDb::new();
    let body = body_of(vec![decl(LocalTy::Value(int()))], vec![], 0);
    body.place_ty(
        &Place {
            local: LocalId(0),
            projections: vec![Projection::Field(0)],
        },
        &db,
    );
}

#[test]
#[should_panic(expected = "dereference of a value local")]
fn a_second_deref_panics() {
    let db = TestDb::new();
    let body = body_of(
        vec![decl(LocalTy::Borrow(BorrowMode::Ref, int()))],
        vec![],
        0,
    );
    body.place_ty(
        &Place {
            local: LocalId(0),
            projections: vec![Projection::Deref, Projection::Deref],
        },
        &db,
    );
}

#[test]
#[should_panic(expected = "dereference of a value local")]
fn a_deref_after_a_field_panics() {
    let db = TestDb::new();
    let p = point(&db);
    let body = body_of(
        vec![decl(LocalTy::Borrow(BorrowMode::In, Ty::Struct(p)))],
        vec![],
        0,
    );
    body.place_ty(
        &Place {
            local: LocalId(0),
            projections: vec![Projection::Field(0), Projection::Deref],
        },
        &db,
    );
}
