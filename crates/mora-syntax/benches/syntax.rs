//! Benchmarks of the lexer and the parser on generated source (§3, §14.4).
//!
//! - `lex/*` time [`mora_syntax::lex`] alone.
//! - `parse/*` time [`mora_syntax::parse`], which lexes, parses and builds
//!   the lossless tree.
//!
//! `small` is one file of a few dozen lines, as an editor sees on most
//! keystrokes; `large` is one file of about 10,000 lines.
//!
//! `cargo bench -p mora-syntax --bench syntax`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use mora_syntax::{lex, parse};

/// One block of declarations: a struct with methods, a class, a union, a
/// match, loops, lambdas, string interpolation and raw strings.
fn block(text: &mut String, n: usize) {
    writeln!(
        text,
        "/// A point in the plane.\n\
         public struct Point{n} {{\n\
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
         \x20   public Scaled(factor: int): Point{n} => new Point{n} {{ X = X * factor, Y = Y * factor, Name = Name }};\n\
         }}\n\
         \n\
         public class Store{n} {{\n\
         \x20   private _count: int;\n\
         \x20   private _items: List<Point{n}>;\n\
         \n\
         \x20   public Add(p: Point{n}): int {{\n\
         \x20       _items.Add(p);\n\
         \x20       _count = _count + p.Length();\n\
         \x20       return _count;\n\
         \x20   }}\n\
         \n\
         \x20   public Describe(prefix: string): string => $\"{{prefix}}: {{_count}} points (0x{n:X})\";\n\
         }}\n\
         \n\
         public Helper{n}(a: int, b: string, flag: bool): int {{\n\
         \x20   var sum = 0;\n\
         \x20   for (var i = 0; i < a; i++) {{\n\
         \x20       if (flag && i % 2 == 0) {{\n\
         \x20           sum += i * 3;\n\
         \x20       }} else {{\n\
         \x20           sum -= 1;\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   let label = $\"{{b}} {{sum}}\";\n\
         \x20   let scale = 1_000.5e-3;\n\
         \x20   return sum + label.Length;\n\
         }}\n"
    )
    .unwrap();
}

/// A source file made of `blocks` blocks followed by top-level statements.
fn source(blocks: usize) -> String {
    let mut text = String::new();
    for n in 0..blocks {
        block(&mut text, n);
    }
    for n in 0..blocks {
        writeln!(
            text,
            "let p{n} = new Point{n} {{ X = {n}, Y = 4, Name = \"p\" }};\n\
             Console.WriteLine(Helper{n}(p{n}.Length(), \"x\", true));"
        )
        .unwrap();
    }
    text
}

fn inputs() -> [(&'static str, String); 2] {
    [("small", source(1)), ("large", source(200))]
}

fn lexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("lex");
    for (name, text) in inputs() {
        group.bench_function(name, |b| {
            b.iter(|| black_box(lex(black_box(&text)).tokens.len()));
        });
    }
    group.finish();
}

fn parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    for (name, text) in inputs() {
        group.bench_function(name, |b| {
            b.iter(|| black_box(parse(black_box(&text)).errors().len()));
        });
    }
    group.finish();
}

/// The generated source must parse without errors, or the benchmarks
/// measure error recovery instead of the common path.
fn check_generated(_: &mut Criterion) {
    for (name, text) in inputs() {
        let parsed = parse(&text);
        let errors = parsed.errors();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}

criterion_group!(benches, check_generated, lexing, parsing);
criterion_main!(benches);
