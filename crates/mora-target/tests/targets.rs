// Test code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Built-in target descriptions, the rules every description follows, and
//! `--target` resolution (§19.4).

use std::path::{Path, PathBuf};

use mora_target::{Capability, Error, Target, builtin_names};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn test32() -> String {
    std::fs::read_to_string(repo().join("tests/targets/test32.toml")).unwrap()
}

fn x86_64_linux() -> String {
    std::fs::read_to_string(repo().join("targets/x86_64-linux.toml")).unwrap()
}

#[test]
fn every_file_in_targets_is_built_in() {
    let mut files: Vec<String> = std::fs::read_dir(repo().join("targets"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    let mut names: Vec<String> = builtin_names().map(|name| format!("{name}.toml")).collect();
    names.sort();
    assert_eq!(files, names);
}

#[test]
fn every_builtin_parses_under_its_own_name() {
    for name in builtin_names() {
        let target = Target::builtin(name).unwrap();
        assert_eq!(target.name, name);
    }
}

#[test]
fn descriptions_round_trip() {
    let mut targets: Vec<Target> = builtin_names()
        .map(|name| Target::builtin(name).unwrap())
        .collect();
    targets.push(Target::parse(&test32()).unwrap());
    for target in targets {
        let text = toml::to_string(&target).unwrap();
        assert_eq!(Target::parse(&text).unwrap(), target, "{text}");
    }
}

#[test]
fn test32_has_32_bit_pointers() {
    let target = Target::parse(&test32()).unwrap();
    assert_eq!(target.pointer_width, 32);
    assert_eq!(target.pointer_bytes(), 4);
    assert_eq!(target.align.pointer, 4);
}

#[test]
fn cfg_values_come_from_the_description() {
    let target = Target::builtin("riscv64gc-linux").unwrap();
    assert_eq!(target.cfg(), [("Os", "linux"), ("Arch", "riscv64")]);
    let target = Target::builtin("aarch64-macos").unwrap();
    assert_eq!(target.cfg(), [("Os", "macos"), ("Arch", "aarch64")]);
}

#[test]
fn has_reads_the_capability_list() {
    let target = Target::builtin("x86_64-windows").unwrap();
    assert!(target.has(Capability::Net));
    let target = Target::parse(&test32()).unwrap();
    assert!(target.has(Capability::Heap));
    assert!(!target.has(Capability::Stdio));
}

#[test]
fn an_unknown_key_is_an_error() {
    let text = x86_64_linux().replace("endian =", "colour = \"red\"\nendian =");
    let message = Target::parse(&text).unwrap_err();
    assert!(message.contains("colour"), "{message}");
}

#[test]
fn an_unknown_key_in_a_table_is_an_error() {
    let text = x86_64_linux().replace("[align]\n", "[align]\ni128 = 16\n");
    let message = Target::parse(&text).unwrap_err();
    assert!(message.contains("i128"), "{message}");
}

#[test]
fn a_missing_key_is_an_error() {
    let text = x86_64_linux().replace("endian = \"little\"\n", "");
    let message = Target::parse(&text).unwrap_err();
    assert!(message.contains("endian"), "{message}");
}

#[test]
fn rule_breaks_are_errors() {
    let cases = [
        (
            "pointer-width = 64",
            "pointer-width = 16",
            "`pointer-width` must be 32 or 64",
        ),
        ("i32 = 4", "i32 = 3", "`align.i32` must be a power of two"),
        (
            "i64 = 8",
            "i64 = 16",
            "`align.i64` must be a power of two no larger than 8",
        ),
        (
            "\"heap\", \"threads\"",
            "\"heap\", \"heap\"",
            "listed twice",
        ),
        ("\"heap\", \"threads\"", "\"heap\", \"gpu\"", "gpu"),
        (
            "flavor = \"ld.lld\"",
            "flavor = \"lld-link\"",
            "`object-format` is Elf",
        ),
        (
            "flavor = \"ld.lld\"",
            "flavor = \"wasm-ld\"",
            "reads Wasm objects",
        ),
        (
            "triple = \"x86_64-unknown-linux-musl\"",
            "triple = \"\"",
            "`triple` is empty",
        ),
        ("os = \"linux\"", "os = \"Linux\"", "`os` must be lowercase"),
        ("name = \"x86_64-linux\"", "name = \"\"", "`name` must be"),
        ("backend = \"cranelift\"", "backend = \"llvm\"", "llvm"),
    ];
    for (from, to, expected) in cases {
        let text = x86_64_linux().replacen(from, to, 1);
        assert_ne!(text, x86_64_linux(), "{from}");
        let message = Target::parse(&text).unwrap_err();
        assert!(message.contains(expected), "{to}: {message}");
    }
}

#[test]
fn resolve_takes_a_name_or_a_path() {
    assert_eq!(
        Target::resolve("x86_64-linux").unwrap().triple,
        "x86_64-unknown-linux-musl"
    );
    let path = repo().join("tests/targets/test32.toml");
    assert_eq!(
        Target::resolve(path.to_str().unwrap()).unwrap().name,
        "test32"
    );
}

#[test]
fn an_unknown_name_lists_the_builtins() {
    let err = Target::resolve("x86_64-freebsd").unwrap_err();
    assert!(matches!(err, Error::Unknown(_)));
    let message = err.to_string();
    assert!(
        message.contains("unknown target `x86_64-freebsd`"),
        "{message}"
    );
    assert!(message.contains("aarch64-macos"), "{message}");
}

#[test]
fn a_bad_file_names_its_path() {
    let err = Target::resolve("no/such/target.toml").unwrap_err();
    assert!(matches!(err, Error::Read { .. }));
    assert!(
        err.to_string().starts_with("no/such/target.toml: "),
        "{err}"
    );
    let source = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("a read error keeps its I/O error as the source");
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    assert!(std::error::Error::source(&Target::resolve("x86_64-freebsd").unwrap_err()).is_none());

    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("bad-target");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bad.toml");
    std::fs::write(
        &path,
        x86_64_linux().replace("pointer-width = 64", "pointer-width = 8"),
    )
    .unwrap();
    let message = Target::load(&path).unwrap_err().to_string();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        message.starts_with(&format!("{}: ", path.display())),
        "{message}"
    );
    assert!(message.contains("pointer-width"), "{message}");
}
