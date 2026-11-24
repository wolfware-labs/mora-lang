//! The `.mr` files of the repository that the unit tests read.

use std::path::PathBuf;

/// Every `.mr` file under `dirs`, at any depth, with its text.
///
/// # Panics
///
/// When a folder or a file cannot be read.
pub(crate) fn mr_files(mut dirs: Vec<PathBuf>) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "mr") {
                let text = std::fs::read_to_string(&path).unwrap();
                files.push((path, text));
            }
        }
    }
    files
}
