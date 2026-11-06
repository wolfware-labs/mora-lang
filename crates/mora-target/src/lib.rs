//! Target descriptions: pointer width, byte order, alignments, object format, linker flavor and platform capabilities (§19.4).
//!
//! A target is data, never a name to match on. The built-in descriptions are
//! the files in `targets/` at the repository root, compiled into this crate.
//! [`Target::resolve`] also loads a description that is not built in from a
//! path ending in `.toml`.

mod builtin;
mod error;
mod target;

pub use builtin::builtin_names;
pub use error::Error;
pub use target::{
    Align, Backend, CallConv, Capability, Endian, Linker, LinkerFlavor, ObjectFormat, Runtime,
    Target,
};
