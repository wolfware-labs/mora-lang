//! Benchmarks of the allocation functions of the platform interface
//! (§13.1, §19.4) in the hosted layer, on the runtime the benchmark is built
//! with: the release runtime unless the `debug` feature is on.
//!
//! - `alloc-free/*` time `mora_pal_alloc` and `mora_pal_free` of one block.
//! - `alloc-grow-free/*` time an allocation, a resize to twice its size with
//!   `mora_pal_realloc`, and the free of the grown block.
//!
//! - `alloc-free-recorded/*` time the debug runtime's allocation and free,
//!   which keep a record of each block's size and alignment in front of it
//!   and check it on the free. Benchmarks build the release runtime, so these
//!   call that path directly.
//! - `good-size/*` time `mora_pal_good_size`.
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

fn alloc_free_recorded(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc-free-recorded");
    for (name, size) in SIZES {
        group.bench_function(name, |b| {
            // SAFETY: the size is not zero and the alignment a power of
            // two; the block is freed with the size and alignment it was
            // allocated with.
            b.iter(|| unsafe {
                let block = mora_rt::alloc_recorded(black_box(size), ALIGN);
                assert!(!block.is_null());
                mora_rt::free_recorded(black_box(block), size, ALIGN);
            });
        });
    }
    group.finish();
}

fn good_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("good-size");
    for (name, size) in SIZES {
        group.bench_function(name, |b| b.iter(|| pal::good_size(black_box(size), ALIGN)));
    }
    group.finish();
}

criterion_group!(
    benches,
    alloc_free,
    alloc_grow_free,
    alloc_free_recorded,
    good_size
);
criterion_main!(benches);
