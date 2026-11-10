//! Item trees, name resolution, types, inference and HIR bodies; defines the macro-runner interface (§14.5).
//!
//! The front of the analysis is three layers of `salsa` queries (§14.3):
//!
//! - [`parse`] gives the syntax tree of a file.
//! - [`item_tree`] gives what a file declares: its `using` directives, its
//!   items with their names, kinds, visibility and nested types, and whether
//!   it has top-level statements. It holds no text ranges, so an edit inside
//!   a body leaves it equal and nothing that reads it runs again. The ranges
//!   diagnostics need are in [`item_source_map`].
//! - [`def_map`] puts the item trees of a project together: the namespaces
//!   its folders give (§4.2), the items in each, the dependencies and the
//!   entry point (§4.6).
//!
//! [`resolve_path`] looks a path up from a file, in the order of §14.5.
//! [`signature()`] gives an item's types with their names looked up, and
//! [`body`] the HIR of one piece of code (a function, an initializer, the
//! top-level statements, a member's code), with [`body_resolution`] saying
//! what each of its names refers to, and [`infer`] the types of its
//! expressions and locals.
//! [`diagnostics`] reports what is wrong with a project's folders, the paths
//! of its `using` directives, signatures and bodies, the types in its
//! bodies, the member names of its types and its entry point.

mod arena;
mod bodies;
mod defs;
mod diverging;
mod inference;
mod items;
mod name;
mod report;
mod resolve;
mod signature;
mod suggest;
mod ty;
mod type_ref;

#[cfg(test)]
mod test_db;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use arena::arena_index;
pub use bodies::{
    Arg, ArgMode, BinaryOp, Body, BodyKind, BodyOwner, BodyResolution, BodySourceMap, Capture,
    CaptureTarget, Expr, ExprId, InterpPart, Lambda, Literal, Local, LocalFunction, LocalId,
    LocalKind, MatchArm, Pat, PatId, Path, PathResolution, ScopeData, ScopeId, Stmt, StmtId,
    Target, TypeId, UnaryOp, ValueRef, body, body_owners, body_resolution, body_source_map,
    escape_char, hir_dump, item_text, owner_name,
};
pub use defs::{
    DefMap, EntryPoint, FolderProblem, ItemId, Namespace, NamespaceId, NamespaceRef, def_map,
};
pub use diverging::is_diverging;
pub use inference::{
    Callee, Constant, Conversion, ConversionKind, Inference, converts_implicitly, infer,
    literal_constant, ty_name,
};
pub use items::{
    Item, ItemIdx, ItemKind, ItemSource, ItemSourceMap, ItemTree, Using, UsingSource, ValueKind,
    ValueMember, Visibility, item_source_map, item_tree, parse,
};
pub use name::pascal_case;
pub use report::diagnostics;
pub use resolve::{
    ResolveError, ResolveErrorKind, Resolved, STD, Scope, effective_visibility, is_visible,
    resolve_path,
};
pub use signature::signature;
pub use ty::{
    Construct, Field, FnSignature, Param, ParamMode, Primitive, Signature, TupleElem, Ty,
    Unsupported,
};
pub use type_ref::{PathRef, TypeRef};
