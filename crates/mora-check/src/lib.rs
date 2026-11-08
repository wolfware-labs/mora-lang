//! HIR checks and lints: definite assignment, exhaustiveness, mutability, naming and strong cycles (§14.2).
//!
//! [`diagnostics`] reports what the control flow of a project's bodies
//! breaks: a local read before it is definitely assigned (`MR0181`), a
//! `let` local assigned again (`MR0182`), `break` or `continue` outside a
//! loop (`MR0183`), and a function that can end without returning a value
//! (`MR0184`). Each file's bodies are checked by one query, which runs
//! again only when that file changes.

mod flow;

use mora_diag::Diagnostic;
use mora_hir::{body, body_owners, body_source_map, def_map};
use mora_span::{Db, Project, SourceFile};

/// The control-flow errors of every body of `project`, file by file.
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn Db, project: Project) -> Vec<Diagnostic> {
    def_map(db, project)
        .files()
        .flat_map(|file| file_diagnostics(db, project, file).iter().cloned())
        .collect()
}

/// The control-flow errors of the bodies of `file`, in the order of the
/// text.
#[salsa::tracked(returns(ref))]
fn file_diagnostics(db: &dyn Db, project: Project, file: SourceFile) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for owner in body_owners(db, file) {
        let body = body(db, file, owner);
        let map = body_source_map(db, file, owner);
        flow::check(db, project, file, owner, body, map, &mut out);
    }
    out.sort_by_key(|diagnostic| diagnostic.primary.span.range.start());
    out
}

#[cfg(test)]
mod tests;
