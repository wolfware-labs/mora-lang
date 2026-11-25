//! Benchmarks of reading a target description (§19.4).
//!
//! - `builtin/<name>` times `Target::builtin`, which parses the built-in
//!   description `name` and checks its rules, as every build does once for
//!   its target and `Target::host` does for each description it tries.
//!
//! `cargo bench -p mora-target --bench targets`, or `cargo xtask bench-compare`
//! to compare a branch with `origin/main`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use mora_target::{Target, builtin_names};

fn builtin(c: &mut Criterion) {
    let mut group = c.benchmark_group("builtin");
    for name in builtin_names() {
        group.bench_function(name, |b| {
            b.iter(|| black_box(Target::builtin(black_box(name)).unwrap().pointer_width));
        });
    }
    group.finish();
}

criterion_group!(benches, builtin);
criterion_main!(benches);
