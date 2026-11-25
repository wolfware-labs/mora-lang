//! Benchmarks of the allocation functions of the platform interface
//! (§13.1, §19.4) in the hosted layer, on the runtime the benchmark is built
//! with: the release runtime unless the `debug` feature is on.
//!
//! - `alloc-free/*` time `mora_pal_alloc` and `mora_pal_free` of one block.
//! - `alloc-grow-free/*` time an allocation, a resize to twice its size with
//!   `mora_pal_realloc`, and the free of the grown block.
//!
//! Each runs at a few block sizes aligned to 8: a small object, a block of a
//! few hundred bytes, and a page.
//!
//! `cargo bench -p mora-rt --bench pal`, or `cargo xtask bench-compare` to
//! compare a branch with `origin/main`.

// Benchmark code may unwrap (CONTRIBUTING.md, Code rules).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// A benchmark of `mora-rt`, a crate allowed `unsafe` code (CONTRIBUTING.md).
#![allow(unsafe_code)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use mora_rt_core::pal;
// The platform layer whose `mora_pal_*` functions the core's calls reach.
use mora_rt as _;

/// The alignment of every block.
const ALIGN: usize = 8;

/// The block sizes, by name.
const SIZES: [(&str, usize); 3] = [("16", 16), ("256", 256), ("4096", 4096)];

fn alloc_free(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc-free");
    for (name, size) in SIZES {
        group.bench_function(name, |b| {
            // SAFETY: the size is not zero and the alignment a power of
            // two; the block is freed with the size and alignment it was
            // allocated with.
            b.iter(|| unsafe {
                let block = pal::alloc(black_box(size), ALIGN).unwrap();
                pal::free(black_box(block), size, ALIGN);
            });
        });
    }
    group.finish();
}

fn alloc_grow_free(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc-grow-free");
    for (name, size) in SIZES {
        group.bench_function(name, |b| {
            // SAFETY: the sizes are not zero and the alignment a power of
            // two; the grown block is freed with its new size.
            b.iter(|| unsafe {
                let block = pal::alloc(black_box(size), ALIGN).unwrap();
                let block = pal::realloc(block, size, ALIGN, black_box(2 * size)).unwrap();
                pal::free(black_box(block), 2 * size, ALIGN);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, alloc_free, alloc_grow_free);
criterion_main!(benches);
