//! The database and helpers the tests of this crate share.

use mora_span::Project;

pub(crate) use crate::test_support::{TestDb, app, codes, file_named, item_named, messages};
use crate::*;

/// What `src/Program.mr` alone in an app reports.
pub(crate) fn diagnostics_of(text: &str) -> Vec<mora_diag::Diagnostic> {
    let db = TestDb::new();
    let project = app(&db, &[("src/Program.mr", text)]);
    diagnostics(&db, project).clone()
}

pub(crate) fn path(text: &str) -> Vec<String> {
    text.split('.').map(str::to_owned).collect()
}

/// The full name of what `text` resolves to from the file at `from`.
pub(crate) fn resolve(
    db: &TestDb,
    project: Project,
    from: &str,
    text: &str,
) -> Result<String, String> {
    let scope = Scope::file(project, file_named(db, project, from));
    match resolve_path(db, scope, &path(text)) {
        Ok(Resolved::Namespace(namespace)) => {
            Ok(def_map(db, namespace.project).namespace_path(namespace.id))
        }
        Ok(Resolved::Item(item)) => Ok(item_name(db, item)),
        Ok(Resolved::Functions(items)) => Ok(format!("{} functions", items.len())),
        Ok(Resolved::External) => Ok("external".into()),
        Err(error) => Err(match error.kind {
            ResolveErrorKind::Unresolved { .. } => format!("unresolved at {}", error.segment),
            ResolveErrorKind::Ambiguous(candidates) => format!("ambiguous: {}", candidates.len()),
            ResolveErrorKind::NotVisible(item) => format!("hidden: {}", item_name(db, item)),
        }),
    }
}

pub(crate) fn item_name(db: &TestDb, item: ItemId) -> String {
    let map = def_map(db, item.project);
    let namespace = map.file_namespace(item.file).unwrap();
    format!(
        "{}.{}",
        map.namespace_path(namespace),
        item_tree(db, item.file).item(item.item).name
    )
}

/// The signature of the item `name` in the file at `path`.
pub(crate) fn signature_of(db: &TestDb, project: Project, path: &str, name: &str) -> Signature {
    let file = file_named(db, project, path);
    signature(db, item_named(db, project, file, name)).clone()
}

pub(crate) fn function(signature: Signature) -> FnSignature {
    match signature {
        Signature::Function(function) => function,
        other => panic!("not a function: {other:?}"),
    }
}

pub(crate) fn unsupported(ty: &Ty) -> (Construct, usize) {
    match ty {
        Ty::Unsupported(it) => (it.construct, it.parts.len()),
        other => panic!("not unsupported: {other:?}"),
    }
}
