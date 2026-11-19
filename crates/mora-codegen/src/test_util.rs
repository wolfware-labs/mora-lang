//! Helpers shared by the tests of this crate.

use mora_diag::Severity;
use mora_hir::test_support::{TestDb, app};
use mora_rt_core::string::{BYTES_OFFSET, MoraString};
use mora_span::Project;
use mora_target::Target;

use crate::{Backend, Cranelift, JitBackend, JitProgram, Profile, Program};

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

/// The function `name` of the app `jit` compiled, as a function pointer of
/// type `F`.
pub(crate) fn func<F: Copy>(jit: &JitProgram, name: &str) -> F {
    let address = jit.function(&symbol(name));
    assert_eq!(size_of::<F>(), size_of::<*const u8>());
    // SAFETY: the caller names a function whose Mora signature matches `F`
    // under the host's C convention, and `F` is a function pointer, the size
    // of the address the assertion compares.
    unsafe { std::mem::transmute_copy::<*const u8, F>(&address) }
}

/// The object file of the app of `text` for `target`.
pub(crate) fn object_of(text: &str, target: &Target, profile: Profile) -> Vec<u8> {
    let db = TestDb::default();
    let project = app_of(&db, text);
    let program = Program::collect(&db, project).unwrap();
    Cranelift.object(&program, target, profile).unwrap()
}

/// The first `len` bytes of the object of the heap string `string`, which a
/// test keeps alive: a literal's object lives as long as its program, and a
/// built one until it is released, which no test does.
pub(crate) fn object_bytes(string: &MoraString, len: usize) -> Vec<u8> {
    assert!(!string.is_inline(), "an inline string has no object");
    // SAFETY: a heap object holds its header and `capacity` bytes, and the
    // callers read no more than the header and the string's length.
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
}
