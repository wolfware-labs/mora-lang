//! `cargo xtask runtime`: builds the runtime static libraries of §13.2 for
//! each target into `target/runtime/<target>/`, and with `--test` links a C
//! program against each one and checks its exit code.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{paths, tools};

/// The C program `--test` links: its `__mora_main` returns [`EXIT_CODE`].
pub const TEST_PROGRAM: &str = "crates/mora-rt/tests/c/exit_code.c";

/// The code the test program's `__mora_main` returns.
pub const EXIT_CODE: i32 = 42;

/// One build of the runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    /// The file the toolchain ships.
    pub file: &'static str,
    /// The cargo features that select it.
    pub features: &'static str,
}

/// The release runtime and the debug runtime (§13.2).
pub const VARIANTS: [Variant; 2] = [
    Variant {
        file: "libmora_rt.a",
        features: "",
    },
    Variant {
        file: "libmora_rt_debug.a",
        features: "debug",
    },
];

/// A runtime library and the system libraries a program linking it needs.
#[derive(Debug)]
pub struct Built {
    /// Where the library was copied.
    pub lib: PathBuf,
    /// Linker arguments such as `-lunwind`, as rustc reports them.
    pub native_libs: Vec<String>,
}

/// The linker arguments in rustc's `native-static-libs` note.
pub fn native_libs(rustc_output: &str) -> Option<Vec<String>> {
    rustc_output.lines().find_map(|line| {
        let (_, libs) = line.split_once("native-static-libs:")?;
        Some(libs.split_whitespace().map(str::to_owned).collect())
    })
}

/// The triple of the machine running the build.
pub fn host() -> Result<String, String> {
    let output = Command::new(tools::rustc())
        .arg("-vV")
        .output()
        .map_err(|err| format!("cannot run rustc: {err}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "rustc -vV printed no host".to_owned())
}

/// Builds `variant` for `target` with the release profile.
pub fn build(root: &Path, target: &str, variant: Variant) -> Result<Built, String> {
    let mut command = Command::new(tools::cargo());
    // Without color: CI forces it on, and the escape codes would end up in
    // the linker arguments read from rustc's note.
    command
        .current_dir(root)
        .args(["rustc", "-p", "mora-rt", "--release", "--locked"])
        .args(["--color", "never", "--target", target]);
    if !variant.features.is_empty() {
        command.args(["--features", variant.features]);
    }
    command.args(["--", "--print", "native-static-libs"]);
    let output = command
        .output()
        .map_err(|err| format!("cannot run cargo: {err}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "{target} {}: cargo failed:\n{stderr}",
            variant.file
        ));
    }
    let native_libs = native_libs(&stderr).ok_or_else(|| {
        format!(
            "{target} {}: rustc printed no native-static-libs",
            variant.file
        )
    })?;
    // Cargo runs in `root`, so it resolves the target directory as `paths` does.
    let target_dir = paths::target_dir(root);
    let built = target_dir.join(target).join("release/libmora_rt.a");
    let dir = target_dir.join("runtime").join(target);
    let lib = dir.join(variant.file);
    fs::create_dir_all(&dir)
        .and_then(|()| fs::copy(&built, &lib))
        .map_err(|err| format!("cannot copy {}: {err}", built.display()))?;
    Ok(Built { lib, native_libs })
}

/// Links [`TEST_PROGRAM`] against `built` with `$CC` (`cc` by default) and
/// the flags in `$CFLAGS`, runs it and checks that it exits with
/// [`EXIT_CODE`]. The host must be able to run programs for `target`.
pub fn test(root: &Path, target: &str, built: &Built) -> Result<(), String> {
    let cc = tools::cc();
    let stem = built
        .lib
        .file_stem()
        .map_or_else(Default::default, |s| s.to_string_lossy());
    let exe = built
        .lib
        .with_file_name(format!("{stem}_test{}", env::consts::EXE_SUFFIX));
    let mut command = Command::new(&cc);
    let cflags = env::var("CFLAGS").unwrap_or_default();
    command
        .current_dir(root)
        .args(cflags.split_whitespace())
        .arg(TEST_PROGRAM)
        .arg(&built.lib)
        .arg("-o")
        .arg(&exe);
    // Rust ships some libraries a target needs, such as musl's libunwind,
    // beside its standard library. The C compiler links the test here;
    // `mora build` links with `target/sysroot` instead (§14.14).
    if let Some(dir) = self_contained(target) {
        command.arg(format!("-L{}", dir.display()));
    }
    command.args(&built.native_libs);
    let output = command
        .output()
        .map_err(|err| format!("cannot run {}: {err}", cc.to_string_lossy()))?;
    if !output.status.success() {
        return Err(format!(
            "{target}: linking {} failed:\n{}",
            built.lib.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let status = Command::new(&exe)
        .status()
        .map_err(|err| format!("cannot run {}: {err}", exe.display()))?;
    match status.code() {
        Some(EXIT_CODE) => Ok(()),
        code => Err(format!(
            "{}: exited with {code:?}, expected {EXIT_CODE}",
            exe.display()
        )),
    }
}

/// The `self-contained` directory of the Rust standard library for `target`,
/// if it has one.
fn self_contained(target: &str) -> Option<PathBuf> {
    let output = Command::new(tools::rustc())
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let dir = Path::new(&sysroot)
        .join("lib/rustlib")
        .join(target)
        .join("lib/self-contained");
    dir.is_dir().then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_native_libraries_from_rustc() {
        let output = "   Compiling mora-rt v0.0.0\nnote: Link against the following native artifacts when linking against this static library.\nnote: native-static-libs: -lunwind -lc\n";
        assert_eq!(
            native_libs(output),
            Some(vec!["-lunwind".to_owned(), "-lc".to_owned()])
        );
        assert_eq!(native_libs("Finished"), None);
    }

    #[test]
    fn the_test_program_returns_the_expected_code() {
        let program = fs::read_to_string(paths::root().join(TEST_PROGRAM)).unwrap();
        assert!(program.contains(&format!("return {EXIT_CODE};")));
    }
}
