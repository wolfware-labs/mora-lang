//! A database and helpers for tests, shared with the crates above this one
//! through the `test-support` feature.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use mora_diag::Diagnostic;
use mora_span::{Dependency, FileId, LineIndex, Project, ProjectKind, SourceFile};

use crate::pascal_case;

/// A database that records every query execution.
#[salsa::db]
#[derive(Clone)]
pub struct TestDb {
    storage: salsa::Storage<Self>,
    executed: Arc<Mutex<Vec<String>>>,
    next_file: Arc<Mutex<u32>>,
}

impl Default for TestDb {
    fn default() -> Self {
        Self::new()
    }
}

impl TestDb {
    /// An empty database.
    pub fn new() -> Self {
        let executed = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&executed);
        let storage = salsa::Storage::new(Some(Box::new(move |event: salsa::Event| {
            if let salsa::EventKind::WillExecute { database_key } = event.kind {
                log.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(format!("{database_key:?}"));
            }
        })));
        Self {
            storage,
            executed,
            next_file: Arc::default(),
        }
    }

    /// The queries executed since the last call, as `name(id)`.
    pub fn take_executed(&self) -> Vec<String> {
        std::mem::take(&mut *self.executed.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// A new file at `path` holding `text`.
    pub fn file(&self, path: &str, text: &str) -> SourceFile {
        let mut next = self
            .next_file
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = FileId::new(*next);
        *next += 1;
        SourceFile::new(self, id, path.into(), text.into())
    }

    /// A project named `name` of the files `files`, as path and text.
    ///
    /// # Panics
    ///
    /// When `name` is not a valid project name.
    pub fn project(
        &self,
        name: &str,
        kind: ProjectKind,
        files: &[(&str, &str)],
        dependencies: Vec<Dependency>,
    ) -> Project {
        let Some(namespace) = pascal_case(name) else {
            panic!("not a project name: {name}");
        };
        let files = files
            .iter()
            .map(|(path, text)| self.file(path, text))
            .collect();
        Project::new(
            self,
            name.into(),
            files,
            namespace,
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

/// An app of the files `files`, with no dependencies.
pub fn app(db: &TestDb, files: &[(&str, &str)]) -> Project {
    db.project("users-api", ProjectKind::App, files, Vec::new())
}

/// The file of `project` at `path`.
///
/// # Panics
///
/// When the project has no such file.
pub fn file_named(db: &TestDb, project: Project, path: &str) -> SourceFile {
    match project.files(db).iter().find(|file| file.path(db) == path) {
        Some(file) => *file,
        None => panic!("no file at {path}"),
    }
}

/// The codes of `diagnostics`.
pub fn codes(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.code.to_string()).collect()
}

/// The messages of `diagnostics`.
pub fn messages(diagnostics: &[Diagnostic]) -> Vec<String> {
    diagnostics.iter().map(|d| d.message.clone()).collect()
}

/// A text with the edits of every fix its diagnostics offer applied.
pub struct Fixed {
    /// The text after the edits.
    pub text: String,
    /// How many edits were applied.
    pub edits: usize,
    /// The lines a fix changed that still report a diagnostic, which must
    /// be none.
    pub remaining: Vec<String>,
}

/// `text` with every fix of the diagnostics `diagnostics_of` reports for it
/// applied. A fix adds no line, so a line has one number in both texts.
pub fn apply_fixes(text: &str, diagnostics_of: impl Fn(&str) -> Vec<Diagnostic>) -> Fixed {
    let mut edits: Vec<_> = diagnostics_of(text)
        .iter()
        .flat_map(|d| d.fixes.iter().flat_map(|fix| fix.edits.clone()))
        .collect();
    // Edits apply from the end of the text, so earlier offsets hold.
    edits.sort_by_key(|edit| std::cmp::Reverse(edit.span.range.start()));
    let mut fixed = text.to_owned();
    for edit in &edits {
        fixed.replace_range(
            std::ops::Range::<usize>::from(edit.span.range),
            &edit.replacement,
        );
    }
    let line_of = |index: &LineIndex, offset| index.line_col(offset).map(|at| at.line);
    let before = LineIndex::new(text);
    let fixed_lines: BTreeSet<_> = edits
        .iter()
        .map(|edit| line_of(&before, edit.span.range.start()))
        .collect();
    let after = LineIndex::new(&fixed);
    let lines: Vec<_> = fixed.lines().collect();
    let remaining = diagnostics_of(&fixed)
        .iter()
        .map(|d| line_of(&after, d.primary.span.range.start()))
        .filter(|line| fixed_lines.contains(line))
        .map(|line| line.map_or("?", |line| lines[line as usize]).to_owned())
        .collect();
    Fixed {
        edits: edits.len(),
        text: fixed,
        remaining,
    }
}
