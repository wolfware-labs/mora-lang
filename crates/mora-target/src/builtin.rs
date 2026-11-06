//! The built-in descriptions: the files in `targets/` at the repository root.

/// The built-in descriptions, by name, in the order of §19.1.
const BUILTIN: &[(&str, &str)] = &[
    (
        "x86_64-linux",
        include_str!("../../../targets/x86_64-linux.toml"),
    ),
    (
        "aarch64-linux",
        include_str!("../../../targets/aarch64-linux.toml"),
    ),
    (
        "aarch64-macos",
        include_str!("../../../targets/aarch64-macos.toml"),
    ),
    (
        "x86_64-macos",
        include_str!("../../../targets/x86_64-macos.toml"),
    ),
    (
        "x86_64-windows",
        include_str!("../../../targets/x86_64-windows.toml"),
    ),
    (
        "aarch64-windows",
        include_str!("../../../targets/aarch64-windows.toml"),
    ),
    (
        "riscv64gc-linux",
        include_str!("../../../targets/riscv64gc-linux.toml"),
    ),
];

/// The names of the built-in targets, in the order of §19.1.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(name, _)| *name)
}

/// The text of the built-in description called `name`.
pub(crate) fn text(name: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, text)| *text)
}
