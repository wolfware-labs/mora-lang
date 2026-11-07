//! The diagnostic type shared by every crate, the `MR` error-code registry and terminal rendering (§17.6).
//!
//! Every crate reports problems as a [`Diagnostic`]: a [`Code`], a
//! [`Severity`], a one-line message, a primary [`Label`], optional secondary
//! labels, an optional note and zero or more [`Fix`]es. [`render()`] prints one
//! in the terminal layout of §17.6, and [`to_json_line`] prints it for
//! `--message-format json`. Both read file paths and texts through
//! [`Sources`]. Codes come from [`codes`], generated from the
//! [`registry`].

mod code;
mod diagnostic;
mod json;
pub mod registry;
mod render;
mod sources;

pub use code::{Code, ParseCodeError};
pub use diagnostic::{Diagnostic, Fix, Label, Severity, TextEdit};
pub use json::{JsonDiagnostic, JsonEdit, JsonFix, JsonLabel, JsonSpan, to_json_line};
pub use registry::codes;
pub use render::render;
pub use sources::{Files, Sources};
