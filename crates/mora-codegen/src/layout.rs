//! The size, alignment and field offsets of every value type (§19.4).
//!
//! This is the only place that computes them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use mora_hir::{Primitive, Ty, instance_fields};
use mora_mir::LocalTy;
use mora_span::Db;
use mora_target::Target;

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

/// Why a type has no layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// A value would take more than 2^31 bytes. The type is the innermost
    /// struct that does, or the tuple when no struct holds it.
    TooLarge(Ty),
    /// Lowering gives the type no layout: `Ty::Unsupported`, `Ty::Error`,
    /// or a struct that holds itself.
    None(Ty),
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

    /// The database the types are read from.
    pub(crate) fn db(&self) -> &'db dyn Db {
        self.db
    }

    /// The size of a pointer in bytes.
    pub fn pointer_bytes(&self) -> u32 {
        self.target.pointer_bytes()
    }

    /// The size of `layout` rounded up to whole words of the target, which
    /// is what a slot needs so that a load of words stays inside it. A
    /// layout is at most 2^31 bytes, a multiple of every word size, so this
    /// does not overflow.
    pub fn word_size(&self, layout: &Layout) -> u32 {
        layout.size.next_multiple_of(self.pointer_bytes())
    }

    /// The alignment of `layout`, at least that of a word, which is what a
    /// slot needs so that a load or store of words is aligned: riscv64
    /// traps or emulates one that is not.
    pub fn word_align(&self, layout: &Layout) -> u32 {
        layout.align.max(self.target.align.pointer)
    }

    /// The layout of a value of `ty`.
    ///
    /// # Panics
    ///
    /// With an internal error for a type that lowering does not give a
    /// layout: `Ty::Unsupported` and `Ty::Error`, and a struct that holds
    /// itself; and for one larger than 2^31 bytes, which code generation
    /// rejects before it lays out anything.
    pub fn of(&self, ty: &Ty) -> Layout {
        self.try_of(ty).unwrap_or_else(|error| match error {
            LayoutError::None(ty) => panic!("internal error computing a layout: {ty:?} has none"),
            LayoutError::TooLarge(ty) => {
                panic!("internal error computing a layout: {ty:?} takes more than {MAX_SIZE} bytes")
            }
        })
    }

    /// The layout of a value of `ty`, or why it has none.
    ///
    /// # Errors
    ///
    /// [`LayoutError::TooLarge`] for a value larger than 2^31 bytes, and
    /// [`LayoutError::None`] for a type lowering does not give a layout.
    pub fn try_of(&self, ty: &Ty) -> Result<Layout, LayoutError> {
        if let Some(layout) = self.cache.borrow().get(ty) {
            return Ok(layout.clone());
        }
        self.compute_fields_first(ty)?;
        Ok(self.cache.borrow()[ty].clone())
    }

    /// Computes the layout of `ty` and of every type its fields hold, each
    /// after the types of its own fields, with a stack of its own: a chain
    /// of structs as long as the program writes must not overflow the
    /// thread's stack.
    ///
    /// # Errors
    ///
    /// [`LayoutError::None`] for a struct that holds itself, whose size has
    /// no end, or a type with no layout, and [`LayoutError::TooLarge`] for
    /// one larger than 2^31 bytes, naming the innermost struct on the way
    /// to it.
    fn compute_fields_first(&self, ty: &Ty) -> Result<(), LayoutError> {
        // Each type, and whether its fields are on the stack above it.
        let mut stack = vec![(ty.clone(), false)];
        // The types whose fields are being computed: one that comes back
        // holds itself.
        let mut open = HashSet::new();
        while let Some((ty, expanded)) = stack.pop() {
            if self.cache.borrow().contains_key(&ty) {
                continue;
            }
            if expanded {
                open.remove(&ty);
                let Some(layout) = self.compute(&ty)? else {
                    // The struct that holds it, or it when none does.
                    let holder = std::iter::once(&ty)
                        .chain(
                            stack
                                .iter()
                                .rev()
                                .filter(|(_, held)| *held)
                                .map(|(ty, _)| ty),
                        )
                        .find(|ty| matches!(ty, Ty::Struct(_)))
                        .unwrap_or(&ty);
                    return Err(LayoutError::TooLarge(holder.clone()));
                };
                self.cache.borrow_mut().insert(ty, layout);
                continue;
            }
            if !open.insert(ty.clone()) {
                return Err(LayoutError::None(ty));
            }
            let fields = self.field_tys(&ty);
            stack.push((ty, true));
            stack.extend(fields.into_iter().map(|field| (field, false)));
        }
        Ok(())
    }

    /// The offset of field `index` of the struct or tuple type `ty` from the
    /// start of the value, and the type of the field; `None` when `ty` is
    /// not an aggregate or has no such field.
    pub fn field(&self, ty: &Ty, index: u32) -> Option<(u32, Ty)> {
        let Shape::Aggregate(offsets) = self.of(ty).shape else {
            return None;
        };
        let index = index as usize;
        Some((*offsets.get(index)?, self.field_tys(ty).get(index)?.clone()))
    }

    /// The offset and type of every field of the struct or tuple type `ty`,
    /// in declaration order; none for any other type.
    pub fn fields(&self, ty: &Ty) -> Vec<(u32, Ty)> {
        let Shape::Aggregate(offsets) = self.of(ty).shape else {
            return Vec::new();
        };
        offsets.into_iter().zip(self.field_tys(ty)).collect()
    }

    /// The types of the fields of `ty` in declaration order: the instance
    /// fields of a struct or the elements of a tuple; none for any other
    /// type.
    fn field_tys(&self, ty: &Ty) -> Vec<Ty> {
        match ty {
            Ty::Tuple(elems) => elems.iter().map(|elem| elem.ty.clone()).collect(),
            Ty::Struct(item) => instance_fields(self.db, *item)
                .map(|(_, field)| field.ty.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The layout of `count` values of `element`, one after another: an
    /// aggregate whose fields are the elements, and the offset of each.
    pub fn array(&self, element: &Layout, count: u32) -> (Layout, Vec<u32>) {
        let offsets: Vec<u32> = (0..count).map(|index| index * element.size).collect();
        let layout = Layout {
            size: element.size * count,
            align: element.align,
            shape: Shape::Aggregate(offsets.clone()),
        };
        (layout, offsets)
    }

    /// The layout of a local: a borrow is a pointer.
    pub fn of_local(&self, ty: &LocalTy) -> Layout {
        match ty {
            LocalTy::Value(ty) => self.of(ty),
            LocalTy::Borrow(..) => self.pointer(),
        }
    }

    /// The layout of `ty`, once the layouts of its fields are in the cache;
    /// `None` when it is larger than [`MAX_SIZE`] bytes.
    ///
    /// # Errors
    ///
    /// [`LayoutError::None`] for a type with no layout.
    fn compute(&self, ty: &Ty) -> Result<Option<Layout>, LayoutError> {
        Ok(match ty {
            Ty::Primitive(primitive) => Some(self.primitive(*primitive)),
            Ty::String => Some(Layout {
                size: mora_rt_core::string::SIZE as u32,
                align: self.target.align.pointer,
                shape: Shape::String,
            }),
            Ty::Tuple(_) | Ty::Struct(_) | Ty::Never => {
                let cache = self.cache.borrow();
                let fields = self
                    .field_tys(ty)
                    .iter()
                    .map(|ty| cache[ty].clone())
                    .collect();
                drop(cache);
                self.aggregate(fields)
            }
            Ty::Unsupported(_) | Ty::Error => return Err(LayoutError::None(ty.clone())),
        })
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
    /// `None` when the aggregate is larger than [`MAX_SIZE`] bytes.
    fn aggregate(&self, fields: Vec<Layout>) -> Option<Layout> {
        let mut order: Vec<usize> = (0..fields.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(fields[i].align));
        let mut offsets = vec![0; fields.len()];
        let mut end: u32 = 0;
        let mut align: u32 = 1;
        for i in order {
            end = fits(end.checked_next_multiple_of(fields[i].align))?;
            offsets[i] = end;
            end = fits(end.checked_add(fields[i].size))?;
            align = align.max(fields[i].align);
        }
        Some(Layout {
            size: fits(end.checked_next_multiple_of(align))?,
            align,
            shape: Shape::Aggregate(offsets),
        })
    }
}

/// The largest size of a type in bytes: every offset inside one fits the
/// signed 32-bit offset of a load or a store.
pub(crate) const MAX_SIZE: u32 = 1 << 31;

/// `size`, when it is known and at most [`MAX_SIZE`].
fn fits(size: Option<u32>) -> Option<u32> {
    size.filter(|&size| size <= MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use mora_hir::Ty;
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_mir::LocalTy;

    use super::*;
    use crate::test_util::{CHAIN, app_of, doubling, struct_chain, tuple, x86_64};

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
        let [ty] = struct_tys(db, text, [name]);
        ty
    }

    /// The structs `names` of the one app of `text`.
    fn struct_tys<const N: usize>(db: &TestDb, text: &str, names: [&str; N]) -> [Ty; N] {
        let project = app_of(db, text);
        let file = file_named(db, project, "src/Program.mr");
        names.map(|name| Ty::Struct(item_named(db, project, file, name)))
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
    fn an_array_repeats_its_element() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let string = layouts.of(&Ty::String);
        let (three, offsets) = layouts.array(&string, 3);
        assert_eq!(offsets, [0, 16, 32]);
        assert_eq!(three.size, 48);
        assert_eq!(three.align, string.align);
        assert_eq!(three.shape, Shape::Aggregate(vec![0, 16, 32]));
        let (none, offsets) = layouts.array(&string, 0);
        assert_eq!((none.size, offsets.len()), (0, 0));
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
    fn a_field_has_its_offset_and_type() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let ty = struct_ty(
            &db,
            "struct S {\n    public A: byte;\n    public B: long;\n    public C: int;\n}\n",
            "S",
        );
        assert_eq!(layouts.field(&ty, 0), Some((12, prim(Primitive::Byte))));
        assert_eq!(layouts.field(&ty, 1), Some((0, prim(Primitive::Long))));
        assert_eq!(layouts.field(&ty, 2), Some((8, prim(Primitive::Int))));
        assert_eq!(layouts.field(&ty, 3), None);
        let pair = tuple(vec![prim(Primitive::Bool), prim(Primitive::Int)]);
        assert_eq!(layouts.field(&pair, 0), Some((4, prim(Primitive::Bool))));
        assert_eq!(layouts.field(&pair, 1), Some((0, prim(Primitive::Int))));
        assert_eq!(layouts.field(&prim(Primitive::Int), 0), None);
        assert_eq!(layouts.field(&Ty::Never, 0), None);
    }

    #[test]
    fn sizes_round_up_to_whole_words() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let size = |ty: &Ty| layouts.word_size(&layouts.of(ty));
        assert_eq!(size(&Ty::UNIT), 0);
        assert_eq!(size(&prim(Primitive::Byte)), 8);
        assert_eq!(size(&tuple(vec![prim(Primitive::Int); 3])), 16);
        assert_eq!(size(&prim(Primitive::Long)), 8);
        let mut narrow = x86_64();
        narrow.pointer_width = 32;
        narrow.align.pointer = 4;
        let layouts = Layouts::new(&db, &narrow);
        assert_eq!(layouts.word_size(&layouts.of(&prim(Primitive::Short))), 4);
    }

    #[test]
    fn slots_align_to_at_least_a_word() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let align = |ty: &Ty| layouts.word_align(&layouts.of(ty));
        assert_eq!(align(&prim(Primitive::Byte)), 8);
        assert_eq!(align(&tuple(vec![prim(Primitive::Int); 3])), 8);
        assert_eq!(align(&Ty::String), 8);
        let mut wide = x86_64();
        wide.align.i64 = 16;
        let layouts = Layouts::new(&db, &wide);
        assert_eq!(layouts.word_align(&layouts.of(&prim(Primitive::Long))), 16);
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
    fn a_long_chain_of_structs_has_a_layout() {
        let db = TestDb::new();
        let first = struct_ty(&db, &struct_chain(CHAIN), "S0");
        assert_eq!(
            Layouts::new(&db, &x86_64()).of(&first),
            Layout {
                size: 4,
                align: 4,
                shape: Shape::Aggregate(vec![0])
            }
        );
    }

    #[test]
    fn a_struct_held_twice_is_laid_out_once() {
        let db = TestDb::new();
        let ty = struct_ty(
            &db,
            "struct P {\n    public X: int;\n}\nstruct Q {\n    public A: P;\n    public B: (P, P);\n}\n",
            "Q",
        );
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(layouts.of(&ty).size, 12);
        // Q, P, `(P, P)` and `int`.
        assert_eq!(layouts.cache.borrow().len(), 4);
    }

    #[test]
    #[should_panic(expected = "internal error computing a layout: Struct")]
    fn a_struct_that_holds_itself_has_no_layout() {
        let db = TestDb::new();
        let ty = struct_ty(&db, "struct S {\n    public A: (int, S);\n}\n", "S");
        Layouts::new(&db, &x86_64()).of(&ty);
    }

    #[test]
    fn fields_have_their_offsets_and_types_in_order() {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, &x86_64());
        let pair = tuple(vec![prim(Primitive::Bool), prim(Primitive::Int)]);
        assert_eq!(
            layouts.fields(&pair),
            [(4, prim(Primitive::Bool)), (0, prim(Primitive::Int))]
        );
        assert_eq!(layouts.fields(&prim(Primitive::Int)), []);
    }

    #[test]
    #[should_panic(expected = "internal error computing a layout")]
    fn an_error_type_has_no_layout() {
        let db = TestDb::new();
        Layouts::new(&db, &x86_64()).of(&Ty::Error);
    }

    #[test]
    fn a_type_of_the_largest_size_has_a_layout() {
        let db = TestDb::new();
        let ty = struct_ty(&db, &doubling(28), "S28");
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(layouts.of(&ty).size, 1 << 31);
        assert_eq!(layouts.word_size(&layouts.of(&ty)), 1 << 31);
    }

    #[test]
    fn a_type_past_the_largest_size_is_too_large() {
        let db = TestDb::new();
        let [ty, inner] = struct_tys(&db, &doubling(29), ["S29", "S28"]);
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(layouts.try_of(&ty), Err(LayoutError::TooLarge(ty.clone())));
        // The structs inside it still have their layouts.
        assert_eq!(layouts.try_of(&inner).map(|l| l.size), Ok(1 << 31));
    }

    #[test]
    fn a_type_whose_size_passes_the_integer_range_names_the_first_too_large() {
        let db = TestDb::new();
        // S29 is the first struct past the limit; S30 holds it.
        let [outer, first] = struct_tys(&db, &doubling(30), ["S30", "S29"]);
        assert_eq!(
            Layouts::new(&db, &x86_64()).try_of(&outer),
            Err(LayoutError::TooLarge(first))
        );
    }

    #[test]
    fn a_tuple_too_large_names_the_struct_that_holds_it() {
        let db = TestDb::new();
        let text = format!(
            "{}struct Pair {{\n    public P: (S28, S28);\n}}\n",
            doubling(28)
        );
        let [pair, s28] = struct_tys(&db, &text, ["Pair", "S28"]);
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(
            layouts.try_of(&pair),
            Err(LayoutError::TooLarge(pair.clone()))
        );
        // A tuple no struct holds is named itself.
        let tuple = tuple(vec![s28.clone(), s28]);
        assert_eq!(
            layouts.try_of(&tuple),
            Err(LayoutError::TooLarge(tuple.clone()))
        );
    }

    #[test]
    #[should_panic(expected = "internal error computing a layout: Struct")]
    fn a_layout_too_large_is_an_internal_error() {
        let db = TestDb::new();
        let ty = struct_ty(&db, &doubling(29), "S29");
        Layouts::new(&db, &x86_64()).of(&ty);
    }
}
