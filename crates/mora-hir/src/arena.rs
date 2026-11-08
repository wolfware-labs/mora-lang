//! Tables kept parallel to an arena.

use la_arena::Idx;

/// The position of `idx` in a table indexed like its arena: the item tables
/// next to [`crate::ItemTree`], the ranges of a [`crate::BodySourceMap`], or
/// a later pass's table by local.
pub fn arena_index<T>(idx: Idx<T>) -> usize {
    // A `u32` always fits in the `usize` of the hosts the compiler runs on.
    u32::from(idx.into_raw()) as usize
}
