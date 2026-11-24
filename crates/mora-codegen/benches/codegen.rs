//! Benchmarks of object emission on a generated project (§14.13, §20.6).
//!
//! - `cold/*` time `Cranelift.object` for the host target on a project whose
//!   front end and MIR queries have already run, so each time is code
//!   generation alone. The top-level statements call every free function,
//!   which call the methods, so that every body is in the program.
//!   `object-debug` and `object-release` differ in the optimization level.
//!
//! `cargo bench -p mora-codegen --bench codegen`, or `cargo xtask bench-compare`
//! to compare a branch with `origin/main`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write;
use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use mora_codegen::{Backend, Cranelift, Profile, Program};
use mora_diag::Severity;
use mora_hir::test_support::{TestDb, app};
use mora_span::Project;
use mora_target::Target;

/// Files of free functions and structs in the generated project.
const FILES: usize = 10;

/// One file of the generated project: structs with methods and free
/// functions that use integer loops, `if`, struct creation and string
/// concatenation.
fn file_text(file: usize) -> String {
    let mut text = String::new();
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
             \x20       if (total > 100) {{\n\
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
             \x20   while (i < a) {{\n\
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
             public Label{n}(a: string, b: string): string => a + b;\n"
        )
        .unwrap();
    }
    text
}

/// The free functions of each file.
const FUNCTIONS: usize = 6;

/// The top-level statements: a call of every free function of every file,
/// so that code generation reaches all of them.
fn entry_text() -> String {
    let mut text = String::new();
    for file in 0..FILES {
        for f in 0..FUNCTIONS {
            writeln!(
                text,
                "Console.WriteLine(Helper{file}_{f}({f}, true));\n\
                 Console.WriteLine(Label{file}_{f}(\"a\", \"b\"));"
            )
            .unwrap();
        }
    }
    text
}

/// The generated project: an entry point and `FILES` files.
fn texts() -> Vec<(String, String)> {
    let mut files = vec![("src/Program.mr".to_owned(), entry_text())];
    for file in 0..FILES {
        files.push((format!("src/File{file}.mr"), file_text(file)));
    }
    files
}

/// The project in `db`, which must have no error, or the benchmark measures
/// error recovery instead of the common path.
fn project(db: &TestDb) -> Project {
    let texts = texts();
    let files: Vec<(&str, &str)> = texts
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let project = app(db, &files);
    let errors: Vec<String> = mora_hir::diagnostics(db, project)
        .iter()
        .chain(mora_mir::diagnostics(db, project))
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    project
}

fn cold(c: &mut Criterion) {
    let host = Target::host().unwrap();
    let db = TestDb::default();
    let project = project(&db);
    // Collecting runs every query the build needs.
    let program = Program::collect(&db, project).unwrap();
    // The entry, the free functions of each file, and the two methods of
    // the one struct of each file that its helpers create.
    assert_eq!(program.functions().len(), 1 + FILES * (FUNCTIONS * 2 + 2));
    let mut group = c.benchmark_group("cold");
    group.bench_function("object-debug", |b| {
        b.iter(|| {
            black_box(
                Cranelift
                    .object(&program, &host, Profile::Debug)
                    .unwrap()
                    .len(),
            )
        });
    });
    group.bench_function("object-release", |b| {
        b.iter(|| {
            black_box(
                Cranelift
                    .object(&program, &host, Profile::Release)
                    .unwrap()
                    .len(),
            )
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().measurement_time(Duration::from_secs(10));
    targets = cold
}
criterion_main!(benches);
