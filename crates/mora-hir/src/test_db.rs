//! The database and helpers the tests of this crate share.

use std::sync::{Arc, Mutex};

use mora_diag::Diagnostic;
use mora_span::{Dependency, FileId, Project, ProjectKind, SourceFile};

use crate::*;

/// A database that records every query execution.
#[salsa::db]
#[derive(Clone)]
pub(crate) struct TestDb {
    storage: salsa::Storage<Self>,
    executed: Arc<Mutex<Vec<String>>>,
    next_file: Arc<Mutex<u32>>,
}

impl TestDb {
    pub(crate) fn new() -> Self {
        let executed = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&executed);
        let storage = salsa::Storage::new(Some(Box::new(move |event: salsa::Event| {
            if let salsa::EventKind::WillExecute { database_key } = event.kind {
                log.lock().unwrap().push(format!("{database_key:?}"));
            }
        })));
        Self {
            storage,
            executed,
            next_file: Arc::default(),
        }
    }

    pub(crate) fn take_executed(&self) -> Vec<String> {
        std::mem::take(&mut *self.executed.lock().unwrap())
    }

    pub(crate) fn file(&self, path: &str, text: &str) -> SourceFile {
        let mut next = self.next_file.lock().unwrap();
        let id = FileId::new(*next);
        *next += 1;
        SourceFile::new(self, id, path.into(), text.into())
    }

    pub(crate) fn project(
        &self,
        name: &str,
        kind: ProjectKind,
        files: &[(&str, &str)],
        dependencies: Vec<Dependency>,
    ) -> Project {
        let files = files
            .iter()
            .map(|(path, text)| self.file(path, text))
            .collect();
        Project::new(
            self,
            name.into(),
            files,
            pascal_case(name).unwrap(),
            kind,
            "src/Program.mr".into(),
            dependencies,
        )
    }
}

#[salsa::db]
impl salsa::Database for TestDb {}

#[salsa::db]
impl mora_span::Db for TestDb {}

pub(crate) fn app(db: &TestDb, files: &[(&str, &str)]) -> Project {
    db.project("users-api", ProjectKind::App, files, Vec::new())
}

pub(crate) fn codes(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.code.to_string()).collect()
}

pub(crate) fn path(text: &str) -> Vec<String> {
    text.split('.').map(str::to_owned).collect()
}

pub(crate) fn file_named(db: &TestDb, project: Project, path: &str) -> SourceFile {
    *project
        .files(db)
        .iter()
        .find(|file| file.path(db) == path)
        .unwrap()
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

pub(crate) fn item_named(db: &TestDb, project: Project, file: SourceFile, name: &str) -> ItemId {
    let (idx, _) = item_tree(db, file)
        .items()
        .find(|(_, item)| item.name == name)
        .unwrap();
    Scope::file(project, file).item(idx)
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

pub(crate) fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.message.clone()).collect()
}
