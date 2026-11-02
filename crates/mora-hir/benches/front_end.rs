//! Benchmarks of the front end on a generated project (§14.3, §20.6).
//!
//! - `cold/*` time one query layer that has not run yet. Setup runs the
//!   layers below it, so each time is that layer's own: `item_tree` and
//!   `bodies` exclude parsing, `signatures` excludes the def map, and
//!   `body_resolution` excludes lowering the bodies. `cold/diagnostics`
//!   starts from a fresh database and is the whole front end.
//! - `edit/*` change one file of a checked project and check it again.
//!
//! `cargo bench -p mora-hir --bench front_end`, or `cargo xtask bench-compare`
//! to compare a branch with `origin/main`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write;
use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use mora_hir::{
    ItemId, body, body_owners, body_resolution, def_map, diagnostics, item_tree, parse, signature,
};
use mora_span::{FileId, Project, ProjectKind, SourceFile};
use salsa::Setter;

/// Folders of the generated project, each a namespace.
const FOLDERS: usize = 10;
/// Files in each folder.
const FILES: usize = 10;

#[salsa::db]
#[derive(Clone, Default)]
struct Database {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl mora_span::Db for Database {}

/// One file of the generated project: structs, a class and free functions
/// whose bodies use locals, control flow, calls and interpolation. `edit`
/// changes one function body without changing any declaration.
fn file_text(folder: usize, file: usize, edit: u32) -> String {
    let mut text = String::new();
    let n = format!("{folder}_{file}");
    for s in 0..4 {
        writeln!(
            text,
            "public struct Point{n}_{s} {{\n\
             \x20   public X: int;\n\
             \x20   public Y: int;\n\
             \x20   public Name: string;\n\
             \n\
             \x20   public Length(): int {{\n\
             \x20       let dx = X * X;\n\
             \x20       var total = dx + Y * Y;\n\
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
             \x20   public Scaled(factor: int): Point{n}_{s} => new Point{n}_{s} {{ X = X * factor, Y = Y * factor, Name = Name }};\n\
             }}\n"
        )
        .unwrap();
    }
    writeln!(
        text,
        "public class Store{n} {{\n\
         \x20   private _count: int;\n\
         \x20   private _last: Point{n}_0;\n\
         \n\
         \x20   public Add(p: Point{n}_0): int {{\n\
         \x20       _last = p;\n\
         \x20       _count = _count + p.Length();\n\
         \x20       return _count;\n\
         \x20   }}\n\
         \n\
         \x20   public Describe(prefix: string): string => $\"{{prefix}}: {{_count}} points\";\n\
         }}\n"
    )
    .unwrap();
    for f in 0..6 {
        // Only the first helper's body depends on `edit`.
        let factor = if f == 0 { edit } else { 1 };
        writeln!(
            text,
            "public Helper{n}_{f}(a: int, b: string, flag: bool): int {{\n\
             \x20   var sum = 0;\n\
             \x20   for (var i = 0; i < a; i++) {{\n\
             \x20       if (flag && i % 2 == 0) {{\n\
             \x20           sum += i * {factor};\n\
             \x20       }} else {{\n\
             \x20           sum -= 1;\n\
             \x20       }}\n\
             \x20   }}\n\
             \x20   let label = $\"{{b}} {{sum}}\";\n\
             \x20   return sum + label.Length;\n\
             }}\n"
        )
        .unwrap();
    }
    text
}

/// The generated project: 101 files, about 17,000 lines.
struct Generated {
    db: Database,
    project: Project,
    files: Vec<SourceFile>,
}

fn generate() -> Generated {
    let db = Database::default();
    let mut files = Vec::new();
    let mut next = 0;
    let mut add = |db: &Database, path: String, text: String| {
        let file = SourceFile::new(db, FileId::new(next), path, text);
        next += 1;
        file
    };
    files.push(add(
        &db,
        "src/Program.mr".into(),
        "Console.WriteLine(\"hello\");\n".into(),
    ));
    for folder in 0..FOLDERS {
        for file in 0..FILES {
            let path = format!("src/Feature{folder}/File{file}.mr");
            files.push(add(&db, path, file_text(folder, file, 1)));
        }
    }
    let project = Project::new(
        &db,
        "bench-app".into(),
        files.clone(),
        "BenchApp".into(),
        ProjectKind::App,
        "src/Program.mr".into(),
        Vec::new(),
    );
    Generated { db, project, files }
}

/// Runs `parse` on every file.
fn parsed() -> Generated {
    let g = generate();
    for &file in &g.files {
        black_box(parse(&g.db, file));
    }
    g
}

/// Runs `item_tree` on every file.
fn lowered() -> Generated {
    let g = generate();
    for &file in &g.files {
        black_box(item_tree(&g.db, file));
    }
    g
}

/// Runs `def_map` and lowers every body, for the resolution benchmark.
fn bodies_lowered() -> Generated {
    let g = lowered();
    black_box(def_map(&g.db, g.project));
    for &file in &g.files {
        for owner in body_owners(&g.db, file) {
            black_box(body(&g.db, file, owner));
        }
    }
    g
}

/// Every item of every file, for the signature benchmark.
fn items(g: &Generated) -> Vec<ItemId> {
    g.files
        .iter()
        .flat_map(|&file| {
            item_tree(&g.db, file).items().map(move |(item, _)| ItemId {
                project: g.project,
                file,
                item,
            })
        })
        .collect()
}

fn cold(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold");
    // Each iteration takes tens of milliseconds, so 100 samples need longer
    // than criterion's default five seconds.
    group
        .sample_size(50)
        .measurement_time(Duration::from_secs(8));
    group.bench_function("parse", |b| {
        b.iter_batched(
            generate,
            |g| {
                for &file in &g.files {
                    black_box(parse(&g.db, file).errors().len());
                }
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("item_tree", |b| {
        b.iter_batched(
            parsed,
            |g| {
                for &file in &g.files {
                    black_box(item_tree(&g.db, file).items().count());
                }
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("def_map", |b| {
        b.iter_batched(
            lowered,
            |g| {
                black_box(def_map(&g.db, g.project).files().count());
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("signatures", |b| {
        b.iter_batched(
            || {
                let g = lowered();
                black_box(def_map(&g.db, g.project));
                let items = items(&g);
                (g, items)
            },
            |(g, items)| {
                for &item in &items {
                    black_box(signature(&g.db, item));
                }
                (g, items)
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("bodies", |b| {
        b.iter_batched(
            parsed,
            |g| {
                for &file in &g.files {
                    for owner in body_owners(&g.db, file) {
                        black_box(body(&g.db, file, owner).exprs.len());
                    }
                }
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("body_resolution", |b| {
        b.iter_batched(
            bodies_lowered,
            |g| {
                for &file in &g.files {
                    for owner in body_owners(&g.db, file) {
                        black_box(body_resolution(&g.db, g.project, file, owner).paths.len());
                    }
                }
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("diagnostics", |b| {
        b.iter_batched(
            generate,
            |g| {
                black_box(diagnostics(&g.db, g.project).len());
                g
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn incremental(c: &mut Criterion) {
    let mut group = c.benchmark_group("edit");
    group.bench_function("body", |b| {
        let mut g = generate();
        let file = g.files[1];
        let texts = [file_text(0, 0, 1), file_text(0, 0, 2)];
        black_box(diagnostics(&g.db, g.project).len());
        let mut edit = 0;
        b.iter(|| {
            edit = 1 - edit;
            file.set_text(&mut g.db).to(texts[edit].clone());
            black_box(diagnostics(&g.db, g.project).len())
        });
    });
    group.bench_function("signature", |b| {
        let mut g = generate();
        let file = g.files[1];
        let original = file_text(0, 0, 1);
        let changed = original.replace("Helper0_0_0(a: int", "Helper0_0_0(a: long");
        black_box(diagnostics(&g.db, g.project).len());
        let mut flip = false;
        b.iter(|| {
            flip = !flip;
            let text = if flip { &changed } else { &original };
            file.set_text(&mut g.db).to(text.clone());
            black_box(diagnostics(&g.db, g.project).len())
        });
    });
    group.finish();
}

/// The generated project must be valid, or the benchmarks measure error
/// recovery instead of the common path.
fn check_generated(_: &mut Criterion) {
    let g = generate();
    for &file in &g.files {
        let errors = parse(&g.db, file).errors();
        assert!(errors.is_empty(), "{}: {errors:?}", file.path(&g.db));
    }
    let found = diagnostics(&g.db, g.project);
    assert!(found.is_empty(), "{found:#?}");
}

criterion_group!(benches, check_generated, cold, incremental);
criterion_main!(benches);
