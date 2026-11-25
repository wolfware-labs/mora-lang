//! Helpers shared by the tests of this crate.

use std::fmt::Write;

use mora_diag::Severity;
use mora_hir::test_support::{TestDb, app};
use mora_hir::{TupleElem, Ty};
use mora_rt_core::string::{BYTES_OFFSET, CAPACITY_OFFSET, MoraString};
use mora_span::Project;
use mora_target::Target;

use crate::{Backend, CodegenError, Cranelift, JitBackend, JitProgram, Profile, Program};

/// `text` as `src/Program.mr`, alone in an app. The front end and MIR lowering
/// must report no error for it.
///
/// # Panics
///
/// When the text has an error.
pub(crate) fn app_of(db: &TestDb, text: &str) -> Project {
    let project = app(db, &[("src/Program.mr", text)]);
    let errors: Vec<String> = mora_hir::diagnostics(db, project)
        .iter()
        .chain(mora_mir::diagnostics(db, project))
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert_eq!(errors, Vec::<String>::new(), "{text}");
    project
}

/// The built-in target `x86_64-linux`.
pub(crate) fn x86_64() -> Target {
    Target::builtin("x86_64-linux").unwrap()
}

/// The tuple of the types `tys`, with no element names.
pub(crate) fn tuple(tys: Vec<Ty>) -> Ty {
    Ty::Tuple(
        tys.into_iter()
            .map(|ty| TupleElem { name: None, ty })
            .collect(),
    )
}

/// How many structs [`struct_chain`] declares in the tests that a walk
/// recursing into each field would overflow the stack of a test thread with.
pub(crate) const CHAIN: usize = 2_000;

/// The text of `count` structs `S0` to `S{count - 1}`, each holding the
/// next in its one field `X`, and a last struct `S{count}` holding an `int`.
pub(crate) fn struct_chain(count: usize) -> String {
    // Writing to a `String` cannot fail.
    let mut text = String::new();
    for n in 0..count {
        let _ = writeln!(text, "struct S{n} {{\n    public X: S{};\n}}", n + 1);
    }
    let _ = writeln!(text, "struct S{count} {{\n    public X: int;\n}}");
    text
}

/// The text of structs `S0` to `S{last}`: `S0` holds a `long`, and each
/// other `S{i}` two of the one before it in `A` and `B`, so `S{i}` takes
/// `8 << i` bytes.
pub(crate) fn doubling(last: usize) -> String {
    doubling_from("    public A: long;\n", last)
}

/// [`doubling`] with `fields`, whole lines, as the fields of `S0`: with none,
/// every struct is empty, and `S{last}` holds `2^last` of `S0` down its
/// fields.
pub(crate) fn doubling_from(fields: &str, last: usize) -> String {
    // Writing to a `String` cannot fail.
    let mut text = format!("struct S0 {{\n{fields}}}\n");
    for i in 1..=last {
        let before = i - 1;
        let _ = writeln!(
            text,
            "struct S{i} {{\n    public A: S{before};\n    public B: S{before};\n}}"
        );
    }
    text
}

/// The symbol of the function `name` of the app `app_of` builds, which is named
/// `users-api`.
pub(crate) fn symbol(name: &str) -> String {
    crate::mangle(["UsersApi", name])
}

/// The app of `text` compiled by the JIT.
pub(crate) fn jit(text: &str, profile: Profile) -> JitProgram {
    let db = TestDb::default();
    let project = app_of(&db, text);
    let program = Program::collect(&db, project).unwrap();
    Cranelift.jit(&program, profile).unwrap()
}

/// The symbol of the function of the app `jit` compiled whose segment is
/// `name` with its parameter types: `F` finds `F(int)` when no other `F`
/// is there, and `F(int)` names that overload.
///
/// # Panics
///
/// When no function or more than one has that name.
fn symbol_named(jit: &JitProgram, name: &str) -> String {
    let found: Vec<String> = jit
        .function_symbols()
        .into_iter()
        .filter(|symbol| {
            segment(symbol).is_some_and(|segment| {
                segment == name
                    || segment
                        .strip_prefix(name)
                        .is_some_and(|rest| rest.starts_with('('))
            })
        })
        .collect();
    let [symbol] = found.as_slice() else {
        panic!("{name} names {found:?}");
    };
    symbol.clone()
}

/// The segment after `UsersApi` in a symbol of the app `app_of` builds, when
/// it is the last one.
fn segment(symbol: &str) -> Option<&str> {
    let rest = symbol.strip_prefix("_M8UsersApi")?.strip_suffix('E')?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let len: usize = rest[..digits].parse().ok()?;
    (rest.len() == digits + len).then(|| &rest[digits..])
}

/// The function `name` of the app `jit` compiled, as a function pointer of
/// type `F`. `name` is the function's name, or its name and parameter types
/// for one of several overloads.
pub(crate) fn func<F: Copy>(jit: &JitProgram, name: &str) -> F {
    let address = jit.function(&symbol_named(jit, name));
    assert_eq!(size_of::<F>(), size_of::<*const u8>());
    // SAFETY: the caller names a function whose Mora signature matches `F`
    // under the host's C convention, and `F` is a function pointer, the size
    // of the address the assertion compares.
    unsafe { std::mem::transmute_copy::<*const u8, F>(&address) }
}

/// The object file of the app of `text` for `target`.
pub(crate) fn object_of(text: &str, target: &Target, profile: Profile) -> Vec<u8> {
    build_object(text, target, profile).unwrap()
}

/// What building the object file of the app of `text` for `target` gives.
pub(crate) fn build_object(
    text: &str,
    target: &Target,
    profile: Profile,
) -> Result<Vec<u8>, CodegenError> {
    let db = TestDb::default();
    let project = app_of(&db, text);
    let program = Program::collect(&db, project).unwrap();
    Cranelift.object(&program, target, profile)
}

/// The first `len` bytes of the object of the heap string `string`, which a
/// test keeps alive: a literal's object lives as long as its program, and a
/// built one until it is released, which no test does.
///
/// # Panics
///
/// When `string` is inline, or `len` goes past the header and the
/// capacity the object's header gives.
pub(crate) fn object_bytes(string: &MoraString, len: usize) -> Vec<u8> {
    assert!(!string.is_inline(), "an inline string has no object");
    // SAFETY: a heap object starts with its header, `BYTES_OFFSET` bytes.
    let header = unsafe { std::slice::from_raw_parts(string.object(), BYTES_OFFSET) };
    let capacity = u32::from_ne_bytes(
        header[CAPACITY_OFFSET..CAPACITY_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let size = BYTES_OFFSET + capacity as usize;
    assert!(len <= size, "{len} bytes go past the object's {size}");
    // SAFETY: a heap object holds its header and then `capacity` bytes, and
    // the assertion keeps `len` within them.
    unsafe { std::slice::from_raw_parts(string.object(), len) }.to_vec()
}

/// The bytes of `string`: the ones inside the value, or those of its heap
/// object (`object_bytes`).
pub(crate) fn string_bytes(string: &MoraString) -> Vec<u8> {
    if string.is_inline() {
        return string.raw()[..string.len()].to_vec();
    }
    object_bytes(string, BYTES_OFFSET + string.len())[BYTES_OFFSET..].to_vec()
}

/// Set in the child process of a test that ends the process, as a panic of
/// generated code does.
pub(crate) const CHILD_ENV: &str = "MORA_CODEGEN_CHILD";

/// Runs `body` in the child process of [`child`] only, so that the test that
/// holds it returns at once when it runs on its own.
pub(crate) fn only_in_child(body: impl FnOnce()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
    }
}

/// Runs the ignored test `test` of this crate in a child process, and
/// returns what it wrote and how it ended. The child test returns at once
/// unless [`only_in_child`] runs it.
pub(crate) fn child(test: &str) -> std::process::Output {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_body_does_not_run_in_the_parent() {
        only_in_child(|| panic!("ran outside a child process"));
    }

    /// Runs `f` on a heap string of `ab` whose object has room for 4 bytes.
    fn with_object(f: impl FnOnce(&MoraString)) {
        let mut object = [0u32; 4];
        let header = mora_rt_core::string::object_header(1, 0, 4, cfg!(target_endian = "big"));
        let bytes: Vec<u8> = header.iter().copied().chain(*b"ab\0\0").collect();
        for (word, chunk) in object.iter_mut().zip(bytes.chunks(4)) {
            *word = u32::from_ne_bytes(chunk.try_into().unwrap());
        }
        f(&MoraString::heap(object.as_ptr().cast(), 2));
    }

    #[test]
    fn object_bytes_read_up_to_the_capacity() {
        with_object(|string| {
            assert_eq!(string_bytes(string), b"ab");
            assert_eq!(object_bytes(string, BYTES_OFFSET + 4).len(), 16);
        });
    }

    #[test]
    #[should_panic(expected = "17 bytes go past the object's 16")]
    fn object_bytes_stop_at_the_capacity() {
        with_object(|string| {
            object_bytes(string, BYTES_OFFSET + 5);
        });
    }
}
