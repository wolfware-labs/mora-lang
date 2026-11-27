//! Benchmarks of the compile pipeline on a generated project (§14.1, §20.6).
//!
//! - `cold/check` runs [`mora_driver::check`] on a fresh database: every
//!   pass up to the sizes of the types, constant evaluation included.
//! - `cold/jit-debug` runs [`mora_driver::jit`] on a fresh database: the
//!   same passes, then code generation for this process in the debug
//!   profile.
//!
//! Each file of the project declares structs with methods, a constant with
//! an initializer that the helpers read, free functions that use integer
//! loops, `if` and struct creation, and a function whose lambda captures a
//! local. The top-level statements call every helper, so code generation
//! reaches them; the functions with a lambda are checked but not called,
//! since closures come with a later milestone.
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
use mora_hir::test_support::{TestDb, app};
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
/// code generation reaches all of them.
fn entry_text() -> String {
    let mut text = "var total = 0;\n".to_owned();
    for file in 0..FILES {
        for f in 0..FUNCTIONS {
            writeln!(text, "total = total + Helper{file}_{f}({f}, true);").unwrap();
        }
    }
    text.push_str("return total % 100;\n");
    text
}

/// The project in a fresh database: an entry point and `FILES` files.
fn generate() -> (TestDb, Project) {
    let mut texts = vec![("src/Program.mr".to_owned(), entry_text())];
    for file in 0..FILES {
        texts.push((format!("src/File{file}.mr"), file_text(file)));
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
            generate,
            |(db, project)| {
                black_box(mora_driver::check(&db, black_box(project)).len());
                (db, project)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("jit-debug", |b| {
        b.iter_batched(
            generate,
            |(db, project)| {
                black_box(mora_driver::jit(&db, black_box(project), Profile::Debug).is_ok());
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
    let (db, project) = generate();
    let found = mora_driver::check(&db, project);
    assert!(found.is_empty(), "{found:#?}");
    if let Err(error) = mora_driver::jit(&db, project, Profile::Debug) {
        panic!("the generated project does not build: {error:?}");
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(10));
    targets = check_generated, cold
}
criterion_main!(benches);
