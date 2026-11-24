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

/// The names of the files in `dir`, sorted.
fn file_names(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    files
}

#[test]
fn every_file_in_targets_is_built_in() {
    let files = file_names(&repo().join("targets"));
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
            "`triple` must start with",
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

/// The descriptions in `tests/targets/rejected/`, by file name, and a part of
/// the error each one gets.
const REJECTED: &[(&str, &str)] = &[
    (
        "aapcs-64-bit.toml",
        "`call-conv` aapcs needs a `pointer-width` of 32, not 64",
    ),
    (
        "aapcs-arch.toml",
        "`call-conv` aapcs is not a convention of `arch` `riscv32`",
    ),
    (
        "call-conv-arch.toml",
        "`call-conv` sysv64 is not a convention of `arch` `aarch64`",
    ),
    (
        "pointer-width-x86_64-32.toml",
        "`call-conv` sysv64 needs a `pointer-width` of 64, not 32",
    ),
    ("triple-control.toml", "`triple` must start with"),
    ("triple-dot-dot.toml", "`triple` must start with"),
    ("triple-leading-dash.toml", "`triple` must start with"),
    ("triple-slash.toml", "`triple` must start with"),
    ("triple-space.toml", "`triple` must start with"),
    ("triple-uppercase.toml", "`triple` must start with"),
];

#[test]
fn every_rejected_description_names_its_rule() {
    let dir = repo().join("tests/targets/rejected");
    let files = file_names(&dir);
    let listed: Vec<&str> = REJECTED.iter().map(|(file, _)| *file).collect();
    assert_eq!(files, listed);
    for (file, expected) in REJECTED {
        let err = Target::load(&dir.join(file)).unwrap_err();
        assert!(matches!(err, Error::Invalid { .. }), "{file}: {err}");
        let message = err.to_string();
        assert!(message.contains(expected), "{file}: {message}");
    }
}

#[test]
fn a_triple_is_lowercase_letters_digits_and_separators() {
    for triple in ["x86_64-unknown-linux-musl", "thumbv8m.main-none-eabi", "0x"] {
        let text = x86_64_linux().replacen("x86_64-unknown-linux-musl", triple, 1);
        assert_eq!(Target::parse(&text).unwrap().triple, triple);
    }
    let delete = format!("x{}", char::from(0x7f));
    for triple in ["", ".", "_x", "x/y", "x y", &delete, "xé"] {
        let text = x86_64_linux().replacen(
            "\"x86_64-unknown-linux-musl\"",
            &toml::Value::String(triple.to_owned()).to_string(),
            1,
        );
        let message = Target::parse(&text).unwrap_err();
        assert!(
            message.contains(
                "`triple` must start with a lowercase letter or a digit and hold only \
                 lowercase letters, digits, `_`, `.` and `-`"
            ),
            "{triple:?}: {message}"
        );
    }
}

#[test]
fn each_convention_fits_one_pointer_width_and_its_arch() {
    let cases = [
        ("sysv64", "x86_64", 64),
        ("win64", "x86_64", 64),
        ("aapcs64", "aarch64", 64),
        ("apple-aapcs64", "aarch64", 64),
        ("win-arm64", "aarch64", 64),
        ("lp64d", "riscv64", 64),
        ("aapcs", "arm", 32),
        ("aapcs", "armv7", 32),
        ("aapcs", "thumbv7em", 32),
    ];
    let with = |conv: &str, arch: &str, width: u32| {
        x86_64_linux()
            .replacen(
                "call-conv = \"sysv64\"",
                &format!("call-conv = \"{conv}\""),
                1,
            )
            .replacen("arch = \"x86_64\"", &format!("arch = \"{arch}\""), 1)
            .replacen("pointer-width = 64", &format!("pointer-width = {width}"), 1)
            .replacen("pointer = 8", &format!("pointer = {}", width / 8), 1)
    };
    for (conv, arch, width) in cases {
        Target::parse(&with(conv, arch, width)).unwrap();
        let other = if width == 64 { 32 } else { 64 };
        let message = Target::parse(&with(conv, arch, other)).unwrap_err();
        assert!(
            message.contains(&format!(
                "`call-conv` {conv} needs a `pointer-width` of {width}, not {other}"
            )),
            "{conv}: {message}"
        );
        for wrong in [
            "x86_64", "aarch64", "riscv64", "riscv32", "arm", "arm64", "arm64_32", "xarm", "wasm32",
        ] {
            let fits = wrong == arch || (conv == "aapcs" && wrong == "arm");
            let parsed = Target::parse(&with(conv, wrong, width));
            if fits {
                parsed.unwrap();
            } else {
                let message = parsed.unwrap_err();
                assert!(
                    message.contains(&format!(
                        "`call-conv` {conv} is not a convention of `arch` `{wrong}`"
                    )),
                    "{conv} on {wrong}: {message}"
                );
            }
        }
    }
}

#[test]
fn a_core_runtime_may_list_hosted_capabilities() {
    let text = test32().replacen("[\"heap\"]", "[\"heap\", \"fs\", \"net\", \"threads\"]", 1);
    let target = Target::parse(&text).unwrap();
    assert!(target.has(Capability::Fs));
    assert!(target.has(Capability::Threads));
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

#[test]
fn every_builtin_is_found_by_its_arch_and_os() {
    for name in builtin_names() {
        let t = Target::builtin(name).unwrap();
        assert_eq!(Target::for_platform(&t.arch, &t.os).unwrap(), t);
    }
}

#[test]
fn an_unknown_platform_is_an_error() {
    assert!(
        matches!(Target::for_platform("sparc", "linux"), Err(Error::Unknown(n)) if n == "sparc-linux")
    );
}

#[test]
fn the_host_is_a_builtin() {
    assert_eq!(Target::host().unwrap().arch, std::env::consts::ARCH);
}
