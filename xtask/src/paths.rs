//! Where things are in the repository, and paths as messages show them.

use std::env;
use std::fmt;
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

/// The cargo target directory: `$CARGO_TARGET_DIR`, which cargo resolves
/// against `root` when it is relative, or `root/target`.
pub fn target_dir(root: &Path) -> PathBuf {
    root.join(env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
}

/// `file` relative to `root`, with `/` separators, for messages.
pub fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// Turns an error about `path` into a message that starts with it, for
/// `map_err`.
pub fn error_at<E: fmt::Display>(path: &Path) -> impl FnOnce(E) -> String + '_ {
    move |err| format!("{}: {err}", path.display())
}

/// Every file under `dir` with the extension, sorted, skipping cargo's build
/// directories; none when `dir` does not exist.
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
                if !is_build_dir(&path) {
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

/// Whether `dir` is a cargo target directory, which cargo tags with a
/// `CACHEDIR.TAG`. Telling it by the tag, not by the name `target`, keeps a
/// source module named `target` in.
fn is_build_dir(dir: &Path) -> bool {
    dir.join("CACHEDIR.TAG").is_file()
}

/// An empty directory for one test, named for it and this process, under
/// the system's temporary directory.
#[cfg(test)]
pub fn scratch(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("mora-xtask-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_drop_the_root_and_use_slashes() {
        let root = root();
        let file = root.join("crates").join("mora-syntax").join("lib.rs");
        assert_eq!(relative(&root, &file), "crates/mora-syntax/lib.rs");
        assert_eq!(
            relative(&root, Path::new("elsewhere/x.rs")),
            "elsewhere/x.rs"
        );
    }

    #[test]
    fn the_target_directory_follows_cargo_target_dir() {
        let root = Path::new("repo");
        let dir = env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into());
        assert_eq!(target_dir(root), root.join(dir));
    }

    #[test]
    fn files_skip_build_directories_but_not_modules_named_target() {
        let dir = scratch("files-with-extension");
        for file in ["a.rs", "src/target/mod.rs", "target/debug/b.rs", "c.txt"] {
            let path = dir.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        fs::write(dir.join("target/CACHEDIR.TAG"), "").unwrap();
        let files = files_with_extension(&dir, "rs").unwrap();
        let shown: Vec<_> = files.iter().map(|file| relative(&dir, file)).collect();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(shown, ["a.rs", "src/target/mod.rs"]);
        assert_eq!(
            files_with_extension(&dir.join("missing"), "rs").unwrap(),
            Vec::<PathBuf>::new()
        );
    }
}
