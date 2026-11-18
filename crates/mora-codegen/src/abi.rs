//! How a value or a function crosses a call: the pass mode of each parameter
//! and of the result.

use mora_mir::{Body, LocalId};

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
    /// By a pointer to memory the caller owns.
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
    /// and its result is `_0`.
    pub fn of(body: &Body, layouts: &Layouts<'_>) -> FnAbi {
        let mode = |local: usize| {
            PassMode::of(
                &layouts.of_local(&body.locals[local].ty),
                layouts.pointer_bytes(),
            )
        };
        FnAbi {
            params: (1..=body.params).map(mode).collect(),
            ret: mode(LocalId::RETURN.0 as usize),
        }
    }
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::{TestDb, file_named, item_named};
    use mora_hir::{BodyOwner, Primitive, TupleElem, Ty};
    use mora_target::Target;

    use super::*;
    use crate::test_util::app_of;

    fn x86_64() -> Target {
        Target::builtin("x86_64-linux").expect("the built-in target")
    }

    fn mode(ty: &Ty, target: &Target) -> PassMode {
        let db = TestDb::new();
        let layouts = Layouts::new(&db, target);
        PassMode::of(&layouts.of(ty), layouts.pointer_bytes())
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
    fn fn_abi_reads_parameters_and_return_slot() {
        let db = TestDb::new();
        let project = app_of(
            &db,
            "struct S {\n    public A: long;\n    public B: int;\n}\n\n\
             F(a: int, move s: S): S {\n    return s;\n}\n",
        );
        let file = file_named(&db, project, "src/Program.mr");
        let item = item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, BodyOwner::Item(item.item));
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(
            FnAbi::of(&mir.bodies[0], &layouts),
            FnAbi {
                params: vec![PassMode::Direct(Scalar::Pointer), PassMode::Words(2)],
                ret: PassMode::Words(2),
            }
        );
    }
}
