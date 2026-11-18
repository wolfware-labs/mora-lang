//! The size, alignment and field offsets of every value type (§19.4).
//!
//! This is the only place that computes them.

use std::cell::RefCell;
use std::collections::HashMap;

use mora_hir::{Primitive, Ty, instance_fields};
use mora_mir::LocalTy;
use mora_span::Db;
use mora_target::Target;

/// The size of a `string` value in bytes. Task 2 replaces it with the
/// runtime's own constant.
const STRING_SIZE: u32 = 16;

/// How a value of a type is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Its size in bytes, a multiple of the alignment.
    pub size: u32,
    /// Its alignment in bytes.
    pub align: u32,
    /// What it is made of.
    pub shape: Shape,
}

/// What a value is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    /// One machine value.
    Scalar(Scalar),
    /// A `string`.
    String,
    /// A struct or a tuple: the offset of each field, in declaration order.
    Aggregate(Vec<u32>),
}

/// A machine value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    /// An integer of `bytes` bytes.
    Int {
        /// Its width in bytes.
        bytes: u8,
        /// Whether it is signed.
        signed: bool,
    },
    /// A 32-bit float.
    Float,
    /// A 64-bit float.
    Double,
    /// A `bool`, one byte.
    Bool,
    /// A `char`, four bytes.
    Char,
    /// A pointer.
    Pointer,
}

/// The layouts of the types of one program on one target.
pub struct Layouts<'db> {
    db: &'db dyn Db,
    target: Target,
    cache: RefCell<HashMap<Ty, Layout>>,
}

impl<'db> Layouts<'db> {
    /// The layouts on `target`.
    pub fn new(db: &'db dyn Db, target: &Target) -> Layouts<'db> {
        Layouts {
            db,
            target: target.clone(),
            cache: RefCell::default(),
        }
    }

    /// The size of a pointer in bytes.
    pub fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    /// The layout of a value of `ty`.
    ///
    /// # Panics
    ///
    /// With an internal error for a type that lowering does not give a
    /// layout: `Ty::Unsupported` and `Ty::Error`.
    pub fn of(&self, ty: &Ty) -> Layout {
        if let Some(layout) = self.cache.borrow().get(ty) {
            return layout.clone();
        }
        let layout = self.compute(ty);
        self.cache.borrow_mut().insert(ty.clone(), layout.clone());
        layout
    }

    /// The layout of a local: a borrow is a pointer.
    pub fn of_local(&self, ty: &LocalTy) -> Layout {
        match ty {
            LocalTy::Value(ty) => self.of(ty),
            LocalTy::Borrow(..) => self.pointer(),
        }
    }

    fn compute(&self, ty: &Ty) -> Layout {
        match ty {
            Ty::Primitive(primitive) => self.primitive(*primitive),
            Ty::String => Layout {
                size: STRING_SIZE,
                align: self.target.align.pointer,
                shape: Shape::String,
            },
            Ty::Tuple(elems) => {
                self.aggregate(elems.iter().map(|elem| self.of(&elem.ty)).collect())
            }
            Ty::Struct(item) => self.aggregate(
                instance_fields(self.db, *item)
                    .map(|(_, field)| self.of(&field.ty))
                    .collect(),
            ),
            Ty::Never => self.aggregate(Vec::new()),
            Ty::Unsupported(_) | Ty::Error => {
                panic!("internal error computing a layout: {ty:?} has none")
            }
        }
    }

    fn pointer(&self) -> Layout {
        Layout {
            size: self.pointer_bytes(),
            align: self.target.align.pointer,
            shape: Shape::Scalar(Scalar::Pointer),
        }
    }

    fn primitive(&self, primitive: Primitive) -> Layout {
        let align = &self.target.align;
        let int = |bytes: u8, signed: bool, align: u32| Layout {
            size: u32::from(bytes),
            align,
            shape: Shape::Scalar(Scalar::Int { bytes, signed }),
        };
        let one = |size: u32, align: u32, scalar: Scalar| Layout {
            size,
            align,
            shape: Shape::Scalar(scalar),
        };
        match primitive {
            Primitive::Bool => one(1, 1, Scalar::Bool),
            Primitive::Byte => int(1, false, 1),
            Primitive::SByte => int(1, true, 1),
            Primitive::Short => int(2, true, align.i16),
            Primitive::UShort => int(2, false, align.i16),
            Primitive::Int => int(4, true, align.i32),
            Primitive::UInt => int(4, false, align.i32),
            Primitive::Long => int(8, true, align.i64),
            Primitive::ULong => int(8, false, align.i64),
            // A pointer is at most eight bytes wide.
            Primitive::NInt => int(self.pointer_bytes() as u8, true, align.pointer),
            Primitive::NUInt => int(self.pointer_bytes() as u8, false, align.pointer),
            Primitive::Float => one(4, align.f32, Scalar::Float),
            Primitive::Double => one(8, align.f64, Scalar::Double),
            Primitive::Char => one(4, align.i32, Scalar::Char),
        }
    }

    /// Fields go by decreasing alignment, declaration order breaking ties.
    fn aggregate(&self, fields: Vec<Layout>) -> Layout {
        let mut order: Vec<usize> = (0..fields.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(fields[i].align));
        let mut offsets = vec![0; fields.len()];
        let mut end: u32 = 0;
        let mut align: u32 = 1;
        for i in order {
            end = end.next_multiple_of(fields[i].align);
            offsets[i] = end;
            end += fields[i].size;
            align = align.max(fields[i].align);
        }
        Layout {
            size: end.next_multiple_of(align),
            align,
            shape: Shape::Aggregate(offsets),
        }
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_hir::{TupleElem, Ty};
    use mora_mir::LocalTy;
    use mora_target::Target;

    use super::*;
    use crate::test_util::app_of;

    fn x86_64() -> Target {
        Target::builtin("x86_64-linux").expect("the built-in target")
    }

    fn tuple(tys: Vec<Ty>) -> Ty {
        Ty::Tuple(
            tys.into_iter()
                .map(|ty| TupleElem { name: None, ty })
                .collect(),
        )
    }

    fn prim(p: Primitive) -> Ty {
        Ty::Primitive(p)
    }

    fn scalar(size: u32, align: u32, scalar: Scalar) -> Layout {
        Layout {
            size,
            align,
            shape: Shape::Scalar(scalar),
        }
    }

    fn int_layout(bytes: u8, signed: bool) -> Layout {
        scalar(
            u32::from(bytes),
            u32::from(bytes),
            Scalar::Int { bytes, signed },
        )
    }

    /// The type of the struct `name` of `text`.
    fn struct_ty(db: &TestDb, text: &str, name: &str) -> Ty {
        let project = app_of(db, text);
        let file = file_named(db, project, "src/Program.mr");
        Ty::Struct(item_named(db, project, file, name))
    }

    #[test]
    fn primitives_take_their_sizes_and_the_target_alignments() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let cases = [
            (Primitive::Int, int_layout(4, true)),
            (Primitive::UInt, int_layout(4, false)),
            (Primitive::Byte, int_layout(1, false)),
            (Primitive::SByte, int_layout(1, true)),
            (Primitive::Short, int_layout(2, true)),
            (Primitive::UShort, int_layout(2, false)),
            (Primitive::Long, int_layout(8, true)),
            (Primitive::ULong, int_layout(8, false)),
            (Primitive::NInt, int_layout(8, true)),
            (Primitive::NUInt, int_layout(8, false)),
            (Primitive::Bool, scalar(1, 1, Scalar::Bool)),
            (Primitive::Char, scalar(4, 4, Scalar::Char)),
            (Primitive::Float, scalar(4, 4, Scalar::Float)),
            (Primitive::Double, scalar(8, 8, Scalar::Double)),
        ];
        for (primitive, expected) in cases {
            assert_eq!(layouts.of(&prim(primitive)), expected, "{primitive:?}");
        }
    }

    #[test]
    fn target_alignments_are_read_per_type() {
        let mut target = x86_64();
        target.align.i16 = 2;
        target.align.i32 = 4;
        target.align.i64 = 4;
        target.align.f32 = 4;
        target.align.f64 = 4;
        target.align.pointer = 8;
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &target);
        assert_eq!(layouts.of(&prim(Primitive::Long)).align, 4);
        assert_eq!(layouts.of(&prim(Primitive::ULong)).align, 4);
        assert_eq!(layouts.of(&prim(Primitive::Double)).align, 4);
        assert_eq!(layouts.of(&prim(Primitive::Short)).align, 2);
        assert_eq!(layouts.of(&prim(Primitive::UShort)).align, 2);
        target.align.i16 = 1;
        target.align.i32 = 2;
        target.align.f32 = 8;
        let layouts = Layouts::new(&db, &target);
        assert_eq!(layouts.of(&prim(Primitive::Short)).align, 1);
        assert_eq!(layouts.of(&prim(Primitive::UShort)).align, 1);
        assert_eq!(layouts.of(&prim(Primitive::Int)).align, 2);
        assert_eq!(layouts.of(&prim(Primitive::UInt)).align, 2);
        assert_eq!(layouts.of(&prim(Primitive::Char)).align, 2);
        assert_eq!(layouts.of(&prim(Primitive::Float)).align, 8);
    }

    #[test]
    fn struct_fields_go_by_decreasing_alignment_then_declaration_order() {
        let db = TestDb::new();
        let ty = struct_ty(
            &db,
            "struct S {\n    public A: byte;\n    public B: long;\n    public C: int;\n}\n",
            "S",
        );
        let l = Layouts::new(&db, &x86_64()).of(&ty);
        assert_eq!(
            l,
            Layout {
                size: 16,
                align: 8,
                shape: Shape::Aggregate(vec![12, 0, 8])
            }
        );
    }

    #[test]
    fn equal_alignments_keep_declaration_order() {
        let db = TestDb::new();
        let ty = struct_ty(
            &db,
            "struct S {\n    public A: int;\n    public B: int;\n}\n",
            "S",
        );
        let l = Layouts::new(&db, &x86_64()).of(&ty);
        assert_eq!(
            l,
            Layout {
                size: 8,
                align: 4,
                shape: Shape::Aggregate(vec![0, 4])
            }
        );
    }

    #[test]
    fn tuples_lay_out_like_structs() {
        let db = TestDb::new();
        let ty = tuple(vec![prim(Primitive::Bool), prim(Primitive::Int)]);
        let l = Layouts::new(&db, &x86_64()).of(&ty);
        assert_eq!(
            l,
            Layout {
                size: 8,
                align: 4,
                shape: Shape::Aggregate(vec![4, 0])
            }
        );
    }

    #[test]
    fn nested_aggregates_use_the_inner_alignment() {
        let db = TestDb::new();
        let s = struct_ty(
            &db,
            "struct S {\n    public A: byte;\n    public B: long;\n    public C: int;\n}\n",
            "S",
        );
        let ty = tuple(vec![prim(Primitive::Byte), s]);
        let l = Layouts::new(&db, &x86_64()).of(&ty);
        assert_eq!(
            l,
            Layout {
                size: 24,
                align: 8,
                shape: Shape::Aggregate(vec![16, 0])
            }
        );
    }

    #[test]
    fn unit_never_and_empty_structs_are_zero_sized() {
        let db = TestDb::new();
        let empty = struct_ty(&db, "struct E {\n}\n", "E");
        let layouts = Layouts::new(&db, &x86_64());
        let zero = Layout {
            size: 0,
            align: 1,
            shape: Shape::Aggregate(vec![]),
        };
        assert_eq!(layouts.of(&Ty::UNIT), zero);
        assert_eq!(layouts.of(&Ty::Never), zero);
        assert_eq!(layouts.of(&empty), zero);
    }

    #[test]
    fn strings_are_sixteen_bytes_with_pointer_alignment() {
        let db = TestDb::new();
        let l = Layouts::new(&db, &x86_64()).of(&Ty::String);
        assert_eq!(
            l,
            Layout {
                size: 16,
                align: 8,
                shape: Shape::String
            }
        );
    }

    #[test]
    fn a_32_bit_target_changes_only_pointer_sized_types() {
        let mut target = x86_64();
        target.pointer_width = 32;
        target.align.pointer = 4;
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &target);
        assert_eq!(layouts.pointer_bytes(), 4);
        assert_eq!(layouts.of(&prim(Primitive::NInt)), int_layout(4, true));
        assert_eq!(layouts.of(&prim(Primitive::NUInt)), int_layout(4, false));
        assert_eq!(
            layouts.of(&Ty::String),
            Layout {
                size: 16,
                align: 4,
                shape: Shape::String
            }
        );
        assert_eq!(
            layouts.of_local(&LocalTy::Borrow(
                mora_mir::BorrowMode::In,
                prim(Primitive::Int)
            )),
            scalar(4, 4, Scalar::Pointer)
        );
        assert_eq!(layouts.of(&prim(Primitive::Int)), int_layout(4, true));
    }

    #[test]
    fn borrows_are_pointers() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let int = prim(Primitive::Int);
        assert_eq!(
            layouts.of_local(&LocalTy::Borrow(mora_mir::BorrowMode::In, int.clone())),
            scalar(8, 8, Scalar::Pointer)
        );
        assert_eq!(layouts.of_local(&LocalTy::Value(int)), int_layout(4, true));
    }

    #[test]
    fn layouts_are_cached() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let ty = prim(Primitive::Int);
        let first = layouts.of(&ty);
        assert_eq!(layouts.cache.borrow().len(), 1);
        assert_eq!(layouts.of(&ty), first);
        assert_eq!(layouts.cache.borrow().len(), 1);
    }

    #[test]
    #[should_panic(expected = "internal error computing a layout")]
    fn an_error_type_has_no_layout() {
        let db = TestDb::new();
        Layouts::new(&db, &x86_64()).of(&Ty::Error);
    }
}
