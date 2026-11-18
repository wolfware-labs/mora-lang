//! Helpers shared by the tests of this crate.

use mora_diag::Severity;
use mora_hir::test_support::{TestDb, app};
use mora_span::Project;

/// `text` as `src/Program.mr`, alone in an app. The front end and MIR lowering
/// must report no error for it.
///
/// # Panics
///
/// When the text has an error.
pub(crate) fn app_of(db: &TestDb, text: &str) -> Project {
    let project = app(db, &[("src/Program.mr", text)]);
    let errors: Vec<String> = mora_hir::diagnostics(db, project)
        .iter()
        .chain(mora_mir::diagnostics(db, project))
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert_eq!(errors, Vec::<String>::new(), "{text}");
    project
}
