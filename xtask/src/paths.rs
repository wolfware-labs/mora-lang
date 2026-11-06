//! Where things are in the repository, and paths as messages show them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The spec, relative to the repository root.
pub const SPEC: &str = "docs/design/mora.md";

/// The error pages, relative to the repository root.
pub const PAGES: &str = "docs/errors";

/// The file in [`PAGES`] that holds the page template, not a page.
pub const PAGE_TEMPLATE: &str = "README.md";

/// The repository root: the parent of this crate.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// `file` relative to `root`, with `/` separators, for messages.
pub fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Every file under `dir` with the extension, sorted, skipping `target`;
/// none when `dir` does not exist.
pub fn files_with_extension(dir: &Path, extension: &str) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|ext| ext == extension) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}
