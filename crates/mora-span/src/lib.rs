//! File ids, spans, line indexes and the base analysis database (§14.2, §14.3).
//!
//! Everything the compiler reports or caches is anchored to a [`Span`]: a
//! [`FileId`] and a byte range in that file. [`LineIndex`] turns byte offsets
//! into the line and column positions people and editors use, in UTF-8 bytes
//! for diagnostics and in UTF-16 code units for the LSP. [`Db`] is the
//! `salsa` database trait every later crate extends, with the source text and
//! project layout as its inputs.

mod db;
mod file;
mod lines;

pub use db::{Db, Dependency, Project, ProjectKind, SourceFile, line_index};
pub use file::{FileId, Span};
pub use lines::{LineCol, LineIndex, Utf16LineCol, to_text_size};
pub use text_size::{TextRange, TextSize};
