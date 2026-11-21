//! Clippy reads only the nearest clippy.toml, so this crate's must repeat
//! every setting of the workspace's.

use std::path::Path;

#[test]
fn the_crate_clippy_toml_repeats_the_workspace_settings() {
    let crate_dir = std::env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let crate_dir = Path::new(&crate_dir);
    let [workspace, local]: [toml::Table; 2] = [
        crate_dir.join("../../clippy.toml"),
        crate_dir.join("clippy.toml"),
    ]
    .map(|path| std::fs::read_to_string(path).unwrap().parse().unwrap());
    assert!(!workspace.is_empty());
    for (key, value) in &workspace {
        assert_eq!(local.get(key), Some(value), "{key}");
    }
}
