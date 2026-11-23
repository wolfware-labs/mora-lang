//! The checks that every value code generation lays out takes at most 2^31
//! bytes, and the stack frame of each function at most 2^30 (§13.5),
//! `MR0156`. Layouts depend on the target, so the checks run with them:
//! the sizes of types before any code is generated, and the frame of each
//! function once its code is built, before it is compiled.

use cranelift_codegen::ir;
use mora_hir::{
    BodyOwner, ItemId, ItemKind, Ty, is_generic_type, item_name_span, item_tree, ty_name,
};
use mora_mir::{LocalTy, Rvalue, StatementKind};
use mora_span::{Db, Project, Span};
use mora_target::Target;

use crate::error::CodegenError;
use crate::layout::{LayoutError, Layouts};
use crate::program::{Function, Program};

/// What register allocation and the prologue may add to a frame beyond its
/// stack slots: spilled values, saved registers, the frame's setup and the
/// arguments passed to calls on the stack. 64 KiB holds 8192 spilled words.
const FRAME_RESERVE: u64 = 1 << 16;

/// The most bytes the stack slots of one function may take: Cranelift
/// stops at a frame larger than 2^30 bytes, less [`FRAME_RESERVE`].
pub(crate) const MAX_FRAME_SLOTS: u64 = (1 << 30) - FRAME_RESERVE;

/// Checks every struct `project` declares, laid out for `target`.
///
/// # Errors
///
/// [`CodegenError::TypeTooLarge`] for the first struct, in the order of the
/// files and their declarations, that is larger than 2^31 bytes or holds
/// one that is.
pub fn check_sizes(db: &dyn Db, project: Project, target: &Target) -> Result<(), CodegenError> {
    check_structs(&Layouts::new(db, target), project)
}

/// [`check_sizes`] for the host, which the JIT runs on. The check needs a
/// target, and every built-in host lays out these types the same, so a host
/// without a built-in target is checked as `x86_64-linux`.
///
/// # Errors
///
/// As [`check_sizes`].
///
/// # Panics
///
/// When the built-in target `x86_64-linux` is missing, which its tests rule
/// out.
pub fn check_sizes_on_host(db: &dyn Db, project: Project) -> Result<(), CodegenError> {
    let target = Target::host().unwrap_or_else(|_| {
        Target::builtin("x86_64-linux")
            .unwrap_or_else(|error| panic!("the built-in target x86_64-linux: {error}"))
    });
    check_sizes(db, project, &target)
}

/// [`check_sizes`] with the layouts code generation uses.
pub(crate) fn check_structs(layouts: &Layouts<'_>, project: Project) -> Result<(), CodegenError> {
    let db = layouts.db();
    for &file in project.files(db) {
        for (item, data) in item_tree(db, file).items() {
            let id = ItemId {
                project,
                file,
                item,
            };
            if data.kind == ItemKind::Struct && !is_generic_type(db, id) {
                check(layouts, &Ty::Struct(id), None)?;
            }
        }
    }
    Ok(())
}

/// Checks the type of every local of every function of `program`, for a
/// tuple too large that no struct holds.
pub(crate) fn check_locals(
    layouts: &Layouts<'_>,
    program: &Program<'_>,
) -> Result<(), CodegenError> {
    for function in program.functions() {
        for local in &function.body.locals {
            if let LocalTy::Value(ty) = &local.ty {
                check(layouts, ty, Some(function.body.span))?;
            }
        }
    }
    Ok(())
}

/// The most bytes the array of a concatenation or interpolation may take:
/// [`MAX_FRAME_SLOTS`], or in a test of this crate the limit
/// [`with_temporary_limit`] sets, so that a small program can pass it.
fn temporary_limit() -> u64 {
    #[cfg(test)]
    if let Some(limit) = TEMPORARY_LIMIT.get() {
        return limit;
    }
    MAX_FRAME_SLOTS
}

#[cfg(test)]
std::thread_local! {
    /// The limit [`with_temporary_limit`] sets on this thread.
    static TEMPORARY_LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with `limit` bytes in place of [`MAX_FRAME_SLOTS`] for the
/// checks of [`check_temporaries`] on this thread.
#[cfg(test)]
pub(crate) fn with_temporary_limit<T>(limit: u64, f: impl FnOnce() -> T) -> T {
    TEMPORARY_LIMIT.set(Some(limit));
    let result = f();
    TEMPORARY_LIMIT.set(None);
    result
}

/// Checks the array of strings that each concatenation and interpolation of
/// `function` builds on its stack, one string a part, before its code is
/// built: an array that alone takes more than [`MAX_FRAME_SLOTS`] bytes
/// cannot be laid out in a frame. Code generation checks each function so,
/// then builds it and checks its frame ([`check_frame`]), in the order of
/// [`Program::functions`], so the first function in that order that is too
/// large is the one reported.
///
/// # Errors
///
/// [`CodegenError::FrameTooLarge`] for `function`, where [`check_frame`]
/// reports it.
pub(crate) fn check_temporaries(
    layouts: &Layouts<'_>,
    function: &Function<'_>,
) -> Result<(), CodegenError> {
    let string = layouts.of(&Ty::String);
    let statements = function
        .body
        .blocks
        .iter()
        .flat_map(|block| &block.statements);
    for statement in statements {
        let parts = match &statement.kind {
            StatementKind::Assign(_, Rvalue::Concat(operands)) => operands.len(),
            StatementKind::Assign(_, Rvalue::Interpolate(pieces)) => pieces.len(),
            _ => continue,
        };
        if Layouts::array_size(&string, parts)
            .is_none_or(|size| u64::from(size) > temporary_limit())
        {
            return Err(frame_too_large(layouts.db(), function));
        }
    }
    Ok(())
}

/// Checks the stack slots `code`, the built code of `function`, holds: the
/// locals the lowering keeps in memory and the temporaries it made, each
/// placed at its alignment as Cranelift places them.
///
/// # Errors
///
/// [`CodegenError::FrameTooLarge`] when they take more than
/// [`MAX_FRAME_SLOTS`] bytes, at the function's name, or the function's
/// span for the top-level statements and a local function.
pub(crate) fn check_frame(
    db: &dyn Db,
    function: &Function<'_>,
    code: &ir::Function,
) -> Result<(), CodegenError> {
    let mut end: u64 = 0;
    for slot in code.sized_stack_slots.values() {
        // Cranelift aligns every slot to at least a machine word, 8 bytes
        // on every target.
        let align = 1u64 << slot.align_shift.max(3);
        end = end.next_multiple_of(align) + u64::from(slot.size);
    }
    if end <= MAX_FRAME_SLOTS {
        return Ok(());
    }
    Err(frame_too_large(db, function))
}

/// [`CodegenError::FrameTooLarge`] for `function`, at its name, or its span
/// for the top-level statements and a local function: the stack slots that
/// [`check_frame`] counts passed the limit, or Cranelift found the frame too
/// large once it added what register allocation and the prologue need.
pub(crate) fn frame_too_large(db: &dyn Db, function: &Function<'_>) -> CodegenError {
    let key = function.key;
    let span = match key.owner {
        BodyOwner::Item(item) if key.local == 0 => item_name_span(
            db,
            ItemId {
                project: key.project,
                file: key.file,
                item,
            },
        ),
        _ => function.body.span,
    };
    CodegenError::FrameTooLarge {
        name: function.body.name.clone(),
        span,
    }
}

/// Lays out `ty`. A tuple too large that no struct holds is reported at
/// `function`, the body of the function with a local of it. A type with no
/// layout is left to the lowering, which reports it.
fn check(layouts: &Layouts<'_>, ty: &Ty, function: Option<Span>) -> Result<(), CodegenError> {
    let Err(LayoutError::TooLarge(ty)) = layouts.try_of(ty) else {
        return Ok(());
    };
    let db = layouts.db();
    let span = match &ty {
        Ty::Struct(item) => item_name_span(db, *item),
        _ => function.unwrap_or_else(|| {
            panic!("internal error checking sizes: {ty:?} is held by no struct")
        }),
    };
    Err(CodegenError::TypeTooLarge {
        name: ty_name(db, &ty),
        span,
    })
}

#[cfg(test)]
mod tests {
    use mora_hir::test_support::TestDb;

    use super::*;
    use crate::test_util::{app_of, doubling, x86_64};
    use crate::{Backend, Cranelift, JitBackend, Profile};

    /// The text a span covers in the one file of `project`.
    fn text_at(db: &TestDb, project: Project, span: Span) -> String {
        let file = project.files(db)[0];
        file.text(db)[span.range].to_owned()
    }

    #[test]
    fn the_largest_size_passes() {
        let db = TestDb::default();
        let project = app_of(&db, &format!("{}return 0;\n", doubling(28)));
        assert_eq!(check_sizes(&db, project, &x86_64()), Ok(()));
    }

    #[test]
    fn a_struct_past_the_largest_size_is_reported_at_its_name() {
        let db = TestDb::default();
        let project = app_of(&db, &format!("{}return 0;\n", doubling(30)));
        let Err(CodegenError::TypeTooLarge { name, span }) = check_sizes(&db, project, &x86_64())
        else {
            panic!("no error");
        };
        // S29 is the first past the limit, before S30 that holds it.
        assert_eq!(name, "S29");
        assert_eq!(text_at(&db, project, span), "S29");
        let diagnostic = CodegenError::TypeTooLarge { name, span }
            .diagnostic()
            .unwrap();
        assert_eq!(diagnostic.code.to_string(), "MR0156");
        assert_eq!(
            diagnostic.message,
            "a value of `S29` would take more than 2147483648 bytes"
        );
    }

    #[test]
    fn code_generation_stops_at_a_type_too_large() {
        let db = TestDb::default();
        let project = app_of(&db, &format!("{}return 0;\n", doubling(29)));
        let program = Program::collect(&db, project).unwrap();
        assert!(matches!(
            Cranelift.jit(&program, Profile::Debug),
            Err(CodegenError::TypeTooLarge { .. })
        ));
    }

    #[test]
    fn a_tuple_too_large_is_reported_at_its_function() {
        let db = TestDb::default();
        let text = format!(
            "{}F(a: S28): int {{\n    let t = (a, a);\n    return 1;\n}}\nreturn 0;\n",
            doubling(28)
        );
        let project = app_of(&db, &text);
        let program = Program::collect(&db, project).unwrap();
        let file = project.files(&db)[0];
        let item = mora_hir::test_support::item_named(&db, project, file, "F");
        let mir = mora_mir::mir(&db, project, file, mora_hir::BodyOwner::Item(item.item));
        let program_of_f = Program::from_body(&db, project, file, &mir.bodies[0]);
        let layouts = Layouts::new(&db, &x86_64());
        assert_eq!(check_locals(&layouts, &program), Ok(()));
        let Err(CodegenError::TypeTooLarge { name, span }) = check_locals(&layouts, &program_of_f)
        else {
            panic!("no error");
        };
        assert_eq!(name, "(S28, S28)");
        let at = text_at(&db, project, span);
        assert!(at.starts_with('{') && at.contains("let t"), "{at:?}");
    }

    /// What building `text` for x86_64 gives, with arrays of strings of at
    /// most `limit` bytes.
    fn build_within(text: &str, limit: u64) -> Result<(), CodegenError> {
        let db = TestDb::default();
        let project = app_of(&db, text);
        let program = Program::collect(&db, project).unwrap();
        with_temporary_limit(limit, || {
            Cranelift
                .object(&program, &x86_64(), Profile::Debug)
                .map(drop)
        })
    }

    #[test]
    fn a_string_built_of_too_many_parts_makes_its_frame_too_large() {
        // Three parts take 48 bytes: with a limit of 47 the build stops at
        // the function that joins them, before its code is built, and with
        // 48 it does not. The number in the interpolation keeps it from
        // becoming a `+`.
        for (joins, name) in [("a + a + a", "F"), ("$\"{a}{1}{a}\"", "G")] {
            let text = format!("{name}(a: string): string => {joins};\nlet s = {name}(\"s\");\n");
            let Err(error) = build_within(&text, 47) else {
                panic!("no error for {joins}");
            };
            assert_eq!(
                error.to_string(),
                format!("the stack frame of `UsersApi.{name}` would take more than 2^30 bytes")
            );
            assert_eq!(error.diagnostic().unwrap().code.to_string(), "MR0156");
            assert_eq!(build_within(&text, 48), Ok(()));
        }
    }

    #[test]
    fn the_first_function_too_large_is_reported() {
        // `Join` builds an array of five strings, 80 bytes, past a limit of
        // 64, and `Big` keeps a local of 2^30 bytes, past the frame's: the
        // one code generation reaches first is reported, whichever check
        // finds it.
        let functions = "Join(a: string): string => a + a + a + a + a;\n\
                         Big(): int {\n    var a: S27;\n    return 1;\n}\n";
        for (calls, first) in [
            ("let j = Join(\"a\");\nlet n = Big();\n", "Join"),
            ("let n = Big();\nlet j = Join(\"a\");\n", "Big"),
        ] {
            let text = format!("{}{functions}{calls}", doubling(27));
            let db = TestDb::default();
            let project = app_of(&db, &text);
            let program = Program::collect(&db, project).unwrap();
            let order: Vec<_> = program
                .functions()
                .iter()
                .map(|f| f.body.name.as_str())
                .collect();
            assert_eq!(order[1], format!("UsersApi.{first}"), "{order:?}");
            let Err(CodegenError::FrameTooLarge { name, .. }) = build_within(&text, 64) else {
                panic!("no error");
            };
            assert_eq!(name, format!("UsersApi.{first}"));
        }
    }

    #[test]
    fn other_errors_have_no_diagnostic() {
        assert_eq!(CodegenError::NoEntryPoint.diagnostic(), None);
    }
}
