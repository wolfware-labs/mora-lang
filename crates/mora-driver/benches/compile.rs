//! Benchmarks of the compile pipeline on a generated project (§14.1, §20.6).
//!
//! - `cold/check` runs [`mora_driver::check`] on a fresh database: every
//!   pass up to the sizes of the types, constant evaluation included.
//! - `cold/jit-debug` runs [`mora_driver::jit`] on a fresh database: the
//!   same passes, then code generation for this process in the debug
//!   profile.
//! - `cold/jit-error` runs [`mora_driver::jit`] on a fresh database on the
//!   same project plus a function, called from the top-level statements,
//!   whose frame is too large: the same passes, then code generation, whose
//!   failure on that function becomes the program's error.
//!
//! Each file of the project declares two constants, one read from the
//! other, that the struct methods and the helpers read; structs with
//! methods; free functions that use integer loops, `if` and struct
//! creation; and as many functions whose lambda captures a local. The
//! top-level statements call every helper, so code generation reaches them;
//! the functions with a lambda are checked but not called, since closures
//! come with a later milestone.
//!
//! `cargo bench -p mora-driver --bench compile`, or `cargo xtask bench-compare`
//! to compare a branch with `origin/main`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write;
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use mora_codegen::Profile;
use mora_driver::BuildError;
use mora_hir::test_support::{TestDb, app, codes, doubling};
use mora_span::Project;

/// Files of structs, constants and functions in the generated project.
const FILES: usize = 10;

/// The free functions of each file that the top-level statements call.
const FUNCTIONS: usize = 6;

/// One file of the generated project.
fn file_text(file: usize) -> String {
    let mut text = String::new();
    writeln!(
        text,
        "const Limit{file}: int = {file} * 10 + 100;\n\
         const Half{file}: int = Limit{file} / 2;\n"
    )
    .unwrap();
    for s in 0..4 {
        let n = format!("{file}_{s}");
        writeln!(
            text,
            "public struct Point{n} {{\n\
             \x20   public X: int;\n\
             \x20   public Y: int;\n\
             \n\
             \x20   public Length(): int {{\n\
             \x20       var total = X * X + Y * Y;\n\
             \x20       if (total > Limit{file}) {{\n\
             \x20           total = total - 1;\n\
             \x20       }} else {{\n\
             \x20           total = total + 1;\n\
             \x20       }}\n\
             \x20       while (total > 1000) {{\n\
             \x20           total = total / 2;\n\
             \x20       }}\n\
             \x20       return total;\n\
             \x20   }}\n\
             \n\
             \x20   public Scaled(factor: int): Point{n} => new Point{n} {{ X = X * factor, Y = Y * factor }};\n\
             }}\n"
        )
        .unwrap();
    }
    for f in 0..FUNCTIONS {
        let n = format!("{file}_{f}");
        writeln!(
            text,
            "public Helper{n}(a: int, flag: bool): int {{\n\
             \x20   var sum = 0;\n\
             \x20   var i = 0;\n\
             \x20   while (i < a && i < Half{file}) {{\n\
             \x20       if (flag && i % 2 == 0) {{\n\
             \x20           sum = sum + i;\n\
             \x20       }} else {{\n\
             \x20           sum = sum - 1;\n\
             \x20       }}\n\
             \x20       i = i + 1;\n\
             \x20   }}\n\
             \x20   let p = new Point{file}_0 {{ X = sum, Y = a }};\n\
             \x20   return p.Scaled(2).Length();\n\
             }}\n\
             \n\
             public Shifted{n}(a: int): int {{\n\
             \x20   let offset = a + {f};\n\
             \x20   let shift = (x: int) => x + offset;\n\
             \x20   return shift(a);\n\
             }}\n"
        )
        .unwrap();
    }
    text
}

/// The top-level statements: a call of every helper of every file, so that
/// code generation reaches all of them, and of `TooLarge` when `too_large`.
fn entry_text(too_large: bool) -> String {
    let mut text = "var total = 0;\n".to_owned();
    for file in 0..FILES {
        for f in 0..FUNCTIONS {
            writeln!(text, "total = total + Helper{file}_{f}({f}, true);").unwrap();
        }
    }
    if too_large {
        text.push_str("total = total + TooLarge();\n");
    }
    text.push_str("return total % 100;\n");
    text
}

/// The last of the structs in [`too_large_text`]: `S28` takes 2^31 bytes,
/// the most a type may take, and more than the stack slots of one function
/// may (about 2^30 bytes).
const TOO_LARGE: usize = 28;

/// The structs of `doubling` up to `S{TOO_LARGE}`, and a function
/// `TooLarge` whose local of the last one makes its frame too large for code
/// generation.
fn too_large_text() -> String {
    let mut text = doubling(TOO_LARGE);
    writeln!(
        text,
        "public TooLarge(): int {{\n\
         \x20   var t: S{TOO_LARGE};\n\
         \x20   return 0;\n\
         }}"
    )
    .unwrap();
    text
}

/// The project in a fresh database: an entry point and `FILES` files, plus
/// the file of [`too_large_text`] when `too_large`.
fn generate(too_large: bool) -> (TestDb, Project) {
    let mut texts = vec![("src/Program.mr".to_owned(), entry_text(too_large))];
    for file in 0..FILES {
        texts.push((format!("src/File{file}.mr"), file_text(file)));
    }
    if too_large {
        texts.push(("src/TooLarge.mr".to_owned(), too_large_text()));
    }
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let db = TestDb::default();
    let project = app(&db, &files);
    (db, project)
}

fn cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold");
    group.bench_function("check", |b| {
        b.iter_batched(
            || generate(false),
            |(db, project)| {
                black_box(mora_driver::check(&db, black_box(project)).len());
                (db, project)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("jit-debug", |b| {
        b.iter_batched(
            || generate(false),
            |(db, project)| {
                black_box(mora_driver::jit(&db, black_box(project), Profile::Debug).is_ok());
                (db, project)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("jit-error", |b| {
        b.iter_batched(
            || generate(true),
            |(db, project)| {
                black_box(mora_driver::jit(&db, black_box(project), Profile::Debug).is_err());
                (db, project)
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

/// The generated project must be valid and build, or the benchmarks
/// measure error recovery instead of the common path.
fn check_generated(_: &mut Criterion) {
    let (db, project) = generate(false);
    let found = mora_driver::check(&db, project);
    assert!(found.is_empty(), "{found:#?}");
    if let Err(error) = mora_driver::jit(&db, project, Profile::Debug) {
        panic!("the generated project does not build: {error:?}");
    }
    let (db, project) = generate(true);
    let error = mora_driver::jit(&db, project, Profile::Debug).err();
    assert!(
        matches!(&error, Some(BuildError::Program(d)) if codes(d) == ["MR0156"]),
        "the project with a frame too large must fail in code generation: {error:?}"
    );
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(10));
    targets = check_generated, cold
}
criterion_main!(benches);
