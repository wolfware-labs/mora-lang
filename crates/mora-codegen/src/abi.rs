//! How a value or a function crosses a call: the pass mode of each parameter
//! and of the result.

use std::collections::HashSet;

use mora_hir::Ty;
use mora_mir::{Body, BorrowMode, LocalId, LocalTy};

use crate::layout::{Layout, Layouts, Scalar, Shape};

/// How one value is passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassMode {
    /// Nothing: the value has size 0.
    Ignore,
    /// In one register, as a scalar.
    Direct(Scalar),
    /// In this many pointer-sized words.
    Words(u8),
    /// By a pointer to a copy the callee owns.
    Indirect,
}

impl PassMode {
    /// The mode of a value of `layout` on a target whose pointer is
    /// `pointer_bytes` wide.
    pub fn of(layout: &Layout, pointer_bytes: u32) -> PassMode {
        if layout.size == 0 {
            return PassMode::Ignore;
        }
        if let Shape::Scalar(scalar) = layout.shape {
            return PassMode::Direct(scalar);
        }
        if layout.size <= pointer_bytes {
            PassMode::Words(1)
        } else if layout.size <= 2 * pointer_bytes {
            PassMode::Words(2)
        } else {
            PassMode::Indirect
        }
    }
}

/// The pass modes of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnAbi {
    /// One per parameter, in order.
    pub params: Vec<PassMode>,
    /// The result.
    pub ret: PassMode,
}

impl FnAbi {
    /// The pass modes of `body`: its parameters are the locals `1..=params`
    /// and its result is `_0`. An `in` parameter that passes the value it
    /// borrows (`borrowed_value`) is passed as that value.
    pub fn of(body: &Body, layouts: &Layouts<'_>) -> FnAbi {
        let mode = |local: LocalId| {
            let ty = &body.locals[local.index()].ty;
            let layout = borrowed_value(ty, layouts).unwrap_or_else(|| layouts.of_local(ty));
            PassMode::of(&layout, layouts.pointer_bytes())
        };
        FnAbi {
            params: body.param_ids().map(mode).collect(),
            ret: mode(LocalId::RETURN),
        }
    }
}

/// The layout of the value that a local of type `ty` passes in place of its
/// address, when it is an `in` borrow of a plain value of at most two words
/// (§10.3, §13.7); `None` for any other local, which a larger or non-plain
/// `in` borrow and every `ref` borrow pass by address.
pub(crate) fn borrowed_value(ty: &LocalTy, layouts: &Layouts<'_>) -> Option<Layout> {
    let LocalTy::Borrow(BorrowMode::In, ty) = ty else {
        return None;
    };
    let layout = layouts.of(ty);
    (layout.size <= 2 * layouts.pointer_bytes() && is_plain(ty, layouts)).then_some(layout)
}

/// Whether `ty` is plain: it holds no reference of any kind (§10.3), so a
/// copy of its bytes is a value of it. A string holds one to its heap
/// object, and a type a later milestone lowers may hold one. The fields
/// are walked with a stack of their own, as deep as the program nests
/// structs, and each type once: a struct that holds another twice, nested
/// as deep as the program writes, has as many fields inside it as it has
/// paths down to them.
fn is_plain(ty: &Ty, layouts: &Layouts<'_>) -> bool {
    let mut pending = vec![ty.clone()];
    let mut seen = HashSet::new();
    while let Some(ty) = pending.pop() {
        match ty {
            Ty::Primitive(_) => {}
            Ty::Tuple(_) | Ty::Struct(_) => {
                let fields = layouts.fields(&ty).into_iter().map(|(_, field)| field);
                pending.extend(fields.filter(|field| seen.insert(field.clone())));
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_hir::{BodyOwner, Primitive, Ty};
    use mora_target::Target;

    use super::*;
    use crate::test_util::{CHAIN, app_of, struct_chain, tuple, x86_64};

    fn mode(ty: &Ty, target: &Target) -> PassMode {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, target);
        PassMode::of(&layouts.of(ty), layouts.pointer_bytes())
    }

    fn prim(p: Primitive) -> Ty {
        Ty::Primitive(p)
    }

    #[test]
    fn pass_modes_follow_size() {
        let t = x86_64();
        let int = prim(Primitive::Int);
        let long = prim(Primitive::Long);
        assert_eq!(mode(&Ty::UNIT, &t), PassMode::Ignore);
        assert_eq!(
            mode(&int, &t),
            PassMode::Direct(Scalar::Int {
                bytes: 4,
                signed: true
            })
        );
        assert_eq!(
            mode(&tuple(vec![int.clone(), int.clone()]), &t),
            PassMode::Words(1)
        );
        assert_eq!(
            mode(&tuple(vec![long.clone(), int.clone()]), &t),
            PassMode::Words(2)
        );
        assert_eq!(
            mode(&tuple(vec![long.clone(), long.clone()]), &t),
            PassMode::Words(2)
        );
        assert_eq!(
            mode(&tuple(vec![long.clone(), long.clone(), long]), &t),
            PassMode::Indirect
        );
        assert_eq!(mode(&Ty::String, &t), PassMode::Words(2));
        let mut narrow = x86_64();
        narrow.pointer_width = 32;
        narrow.align.pointer = 4;
        assert_eq!(mode(&Ty::String, &narrow), PassMode::Indirect);
    }

    #[test]
    fn a_plain_in_parameter_nested_deep_passes_its_value() {
        let db = TestDb::new();
        let text = format!("{}F(s: S0): int => 1;\n", struct_chain(CHAIN));
        let project = app_of(&db, &text);
        let file = file_named(&db, project, "src/Program.mr");
        let item = item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(
            FnAbi::of(&mir.bodies[0], &layouts).params,
            [PassMode::Words(1)]
        );
    }

    #[test]
    fn a_plain_check_looks_at_each_type_once() {
        // `E60` holds 2^60 empty structs down its paths; it is zero-sized
        // and plain, so `in` passes it as a value.
        let mut text = "struct E0 {}\n".to_owned();
        for i in 1..=60 {
            let before = i - 1;
            text.push_str(&format!(
                "struct E{i} {{\n    public A: E{before};\n    public B: E{before};\n}}\n"
            ));
        }
        text.push_str("F(e: E60): int => 1;\n");
        let db = TestDb::new();
        let project = app_of(&db, &text);
        let file = file_named(&db, project, "src/Program.mr");
        let item = item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        let layouts = Layouts::new(&db, &x86_64());
        let param = &mir.bodies[0].locals[1].ty;
        assert_eq!(borrowed_value(param, &layouts).map(|l| l.size), Some(0));
    }

    #[test]
    fn fn_abi_reads_parameters_and_return_slot() {
        let db = TestDb::new();
        let project = app_of(
            &db,
            "struct S {\n    public A: long;\n    public B: int;\n}\n\n\
             F(a: int, move s: S): S {\n    return s;\n}\n\
             struct W {\n    public S: string;\n}\n\
             G(s: string, t: (long, long, long), q: S, ref r: int, u: (), w: W, o: (string,)) {}\n",
        );
        let file = file_named(&db, project, "src/Program.mr");
        let item = item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(
            FnAbi::of(&mir.bodies[0], &layouts),
            FnAbi {
                params: vec![
                    PassMode::Direct(Scalar::Int {
                        bytes: 4,
                        signed: true
                    }),
                    PassMode::Words(2)
                ],
                ret: PassMode::Words(2),
            }
        );
        // An `in` string, or a struct or a tuple that holds one, holds a
        // reference, and a triple is too large, so they pass their address,
        // as a `ref` does; a plain pair of words and a zero-sized value pass
        // as values.
        let item = item_named(&db, project, file, "G");
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        assert_eq!(
            FnAbi::of(&mir.bodies[0], &layouts).params,
            [
                PassMode::Direct(Scalar::Pointer),
                PassMode::Direct(Scalar::Pointer),
                PassMode::Words(2),
                PassMode::Direct(Scalar::Pointer),
                PassMode::Ignore,
                PassMode::Direct(Scalar::Pointer),
                PassMode::Direct(Scalar::Pointer),
            ]
        );
    }
}
