//! The `[patch]` entries each of the two builds follows, and how they
//! compare for one crate.

use std::path::PathBuf;

use toml::{Table, Value};

/// What a `[patch]` entry puts in place of a crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replacement {
    /// A crate in a folder, resolved against the folder cargo resolves the
    /// file's paths from.
    Path(PathBuf),
    /// A git repository, at a branch, tag or rev when the patch names one.
    Git {
        url: String,
        reference: Option<String>,
    },
    /// A version from a registry; `None` for crates.io.
    Registry {
        registry: Option<String>,
        version: Option<String>,
    },
}

impl Replacement {
    /// Reads a patch entry; `resolve` turns its `path` into the folder it
    /// names.
    fn read(patch: &Value, resolve: &impl Fn(&str) -> PathBuf) -> Self {
        let text = |key: &str| patch.get(key).and_then(Value::as_str);
        if let Some(path) = text("path") {
            return Self::Path(resolve(path));
        }
        if let Some(url) = text("git") {
            let reference = ["branch", "tag", "rev"]
                .into_iter()
                .find_map(|key| text(key).map(|value| format!("{key} {value}")));
            return Self::Git {
                url: url.trim_end_matches('/').to_owned(),
                reference,
            };
        }
        Self::Registry {
            registry: text("registry").map(str::to_owned),
            version: text("version").map(str::to_owned),
        }
    }
}

/// One `[patch]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The crate it replaces: the `package` it names, or else its key.
    pub name: String,
    /// The file that has it, relative to the repository root.
    pub file: String,
    /// The entry, keys in sorted order, for messages.
    pub written: String,
    /// What it puts in place of the crate.
    pub replacement: Replacement,
}

/// The `[patch]` entries of `file`, a manifest or a cargo config whose text
/// is `text`; `resolve` turns a patch's `path` into the folder it names.
pub fn patches_in(
    file: &str,
    text: &str,
    resolve: impl Fn(&str) -> PathBuf,
) -> Result<Vec<Patch>, toml::de::Error> {
    let table: Table = text.parse()?;
    let sources = table.get("patch").and_then(Value::as_table);
    Ok(sources
        .into_iter()
        .flat_map(Table::values)
        .filter_map(Value::as_table)
        .flat_map(Table::iter)
        .map(|(key, patch)| Patch {
            name: patch
                .get("package")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_owned(),
            file: file.to_owned(),
            written: format!("{key} = {patch}"),
            replacement: Replacement::read(patch, &resolve),
        })
        .collect())
}

/// The `[patch]` entries the workspace build and the fuzz build follow,
/// each list in the order cargo prefers them.
#[derive(Debug, Default)]
pub struct Patches {
    /// The workspace's: those of the repository's cargo config, then those
    /// of `Cargo.toml`.
    pub root: Vec<Patch>,
    /// The fuzz crate's: those of the repository's cargo config, then those
    /// of `fuzz/Cargo.toml`.
    pub fuzz: Vec<Patch>,
}

impl Patches {
    /// The patches of the two manifests, `root` and `fuzz`, and of the
    /// repository's cargo `config`, which both builds read and which cargo
    /// prefers over a manifest's patch of the same crate.
    pub fn new(root: Vec<Patch>, fuzz: Vec<Patch>, config: Vec<Patch>) -> Self {
        let with_config = |manifest: Vec<Patch>| {
            let mut patches = config.clone();
            patches.extend(manifest);
            patches
        };
        Self {
            root: with_config(root),
            fuzz: with_config(fuzz),
        }
    }

    /// How the two builds patch `name`.
    pub(super) fn of(&self, name: &str) -> Patched {
        let find = |patches: &[Patch]| patches.iter().find(|patch| patch.name == name).cloned();
        match (find(&self.root), find(&self.fuzz)) {
            (Some(root), Some(fuzz)) if root.replacement == fuzz.replacement => Patched::Same,
            (Some(root), Some(fuzz)) => Patched::Apart(Box::new((root, fuzz))),
            (Some(_), None) => Patched::Root,
            (None, Some(_)) => Patched::Fuzz,
            (None, None) => Patched::Neither,
        }
    }

    /// The crates both builds patch, each to another crate: the
    /// workspace's patch and the fuzz crate's.
    pub(super) fn apart(&self) -> Vec<(Patch, Patch)> {
        let mut seen: Vec<&str> = Vec::new();
        self.root
            .iter()
            .filter(|patch| {
                let first = !seen.contains(&patch.name.as_str());
                seen.push(&patch.name);
                first
            })
            .filter_map(|patch| match self.of(&patch.name) {
                Patched::Apart(pair) => Some(*pair),
                _ => None,
            })
            .collect()
    }
}

/// How the two builds patch a crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Patched {
    /// Both, with the same replacement; the rebuild drops the fuzz
    /// lockfile's entry, so cargo takes the patch.
    Same,
    /// Both, each with another replacement: the workspace's patch and the
    /// fuzz crate's.
    Apart(Box<(Patch, Patch)>),
    /// The workspace only; the fuzz crate does not see the patch.
    Root,
    /// The fuzz crate only.
    Fuzz,
    /// Neither: the workspace has a crate of that name of its own, or a
    /// cargo config outside the repository patches it.
    Neither,
}

impl Patched {
    /// Whether both builds patch the crate.
    pub(super) fn in_both(&self) -> bool {
        matches!(self, Self::Same | Self::Apart(_))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// `path` under `base`, with `..` and `.` taken out, as canonicalizing
    /// an existing folder does.
    pub(in crate::fuzz_lock) fn resolved(base: &str, path: &str) -> PathBuf {
        let mut parts: Vec<&str> = base.split('/').collect();
        for part in path.split('/') {
            match part {
                ".." => drop(parts.pop()),
                "." | "" => {}
                part => parts.push(part),
            }
        }
        PathBuf::from(parts.join("/"))
    }

    /// The patches of `text`, the text of `Cargo.toml`, with paths left as
    /// written.
    fn unresolved(text: &str) -> Result<Vec<Patch>, toml::de::Error> {
        patches_in("Cargo.toml", text, |path: &str| PathBuf::from(path))
    }

    /// The patches of `root`, the text of `Cargo.toml` in `/repo`, and of
    /// `fuzz`, the text of `fuzz/Cargo.toml`.
    pub(in crate::fuzz_lock) fn patches_of(root: &str, fuzz: &str) -> Patches {
        Patches {
            root: patches_in("Cargo.toml", root, |path| resolved("/repo", path)).unwrap(),
            fuzz: patches_in("fuzz/Cargo.toml", fuzz, |path| resolved("/repo/fuzz", path)).unwrap(),
        }
    }

    pub(in crate::fuzz_lock) const ROOT_PATCH: &str =
        "[patch.crates-io]\nsmallvec = { path = \"vendor/smallvec\" }\n";
    pub(in crate::fuzz_lock) const FUZZ_PATCH: &str =
        "[patch.crates-io]\nsmallvec = { path = \"../vendor/smallvec\" }\n";
    pub(in crate::fuzz_lock) const FUZZ_PATCH_2: &str =
        "[patch.crates-io]\nsmallvec = { path = \"../vendor/smallvec2\" }\n";
    pub(in crate::fuzz_lock) const NO_PATCH: &str = "[package]\nname = \"mora-fuzz\"\n";

    #[test]
    fn patches_come_from_every_patch_table_with_their_replacements() {
        let manifest = "[package]\nname = \"mora-fuzz\"\n\n[dependencies]\ncc = \"1\"\n\n\
                        [patch.crates-io]\nsmallvec = { path = \"../vendor/smallvec\" }\n\
                        cc2 = { git = \"https://example.com/cc/\", branch = \"next\", package = \
                        \"cc\" }\n\n[patch.\"https://example.com/index\"]\n\
                        zmij = { version = \"0.0.4\", registry = \"mirror\" }\n";
        let found = patches_in("fuzz/Cargo.toml", manifest, |path| {
            resolved("/repo/fuzz", path)
        })
        .unwrap();
        let mut found: Vec<(&str, &str, &Replacement)> = found
            .iter()
            .map(|patch| {
                assert_eq!(patch.file, "fuzz/Cargo.toml");
                (
                    patch.name.as_str(),
                    patch.written.as_str(),
                    &patch.replacement,
                )
            })
            .collect();
        found.sort_by_key(|(name, ..)| *name);
        assert_eq!(
            found,
            [
                (
                    "cc",
                    "cc2 = { branch = \"next\", git = \"https://example.com/cc/\", package = \
                     \"cc\" }",
                    &Replacement::Git {
                        url: "https://example.com/cc".into(),
                        reference: Some("branch next".into()),
                    }
                ),
                (
                    "smallvec",
                    "smallvec = { path = \"../vendor/smallvec\" }",
                    &Replacement::Path("/repo/vendor/smallvec".into())
                ),
                (
                    "zmij",
                    "zmij = { registry = \"mirror\", version = \"0.0.4\" }",
                    &Replacement::Registry {
                        registry: Some("mirror".into()),
                        version: Some("0.0.4".into()),
                    }
                ),
            ]
        );
        let git = |extra: &str| {
            let text =
                format!("[patch.crates-io]\ncc = {{ git = \"https://example.com/cc\"{extra} }}\n");
            unresolved(&text).unwrap()[0].replacement.clone()
        };
        assert_eq!(
            git(", tag = \"v1\""),
            Replacement::Git {
                url: "https://example.com/cc".into(),
                reference: Some("tag v1".into()),
            }
        );
        assert_ne!(git(", rev = \"abc\""), git(""));
        assert_eq!(unresolved(NO_PATCH).unwrap(), []);
        assert_eq!(unresolved("patch = 1\n").unwrap(), []);
        assert!(unresolved("[patch.crates-io\n").is_err());
    }

    #[test]
    fn patches_say_how_the_two_builds_patch_a_crate() {
        let root = "[patch.crates-io]\na = { path = \"a\" }\nb = { path = \"b\" }\n\
                    d = { path = \"d\" }\n";
        let fuzz = "[patch.crates-io]\nb = { path = \"../b\" }\nc = { path = \"c\" }\n\
                    d = { path = \"d\" }\n";
        let patches = patches_of(root, fuzz);
        assert_eq!(patches.of("a"), Patched::Root);
        assert_eq!(patches.of("b"), Patched::Same);
        assert_eq!(patches.of("c"), Patched::Fuzz);
        assert!(matches!(patches.of("d"), Patched::Apart(_)));
        assert_eq!(patches.of("e"), Patched::Neither);
        let apart = patches.apart();
        assert_eq!(apart.len(), 1);
        assert_eq!(apart[0].0.replacement, Replacement::Path("/repo/d".into()));
        assert_eq!(
            apart[0].1.replacement,
            Replacement::Path("/repo/fuzz/d".into())
        );
        assert!(Patched::Same.in_both() && patches.of("d").in_both());
        assert!(!Patched::Root.in_both() && !Patched::Fuzz.in_both());
        assert!(!Patched::Neither.in_both());
    }

    #[test]
    fn a_crate_patched_twice_is_reported_once() {
        let root =
            "[patch.crates-io]\nd = { path = \"d\" }\n\n[patch.mirror]\nd = { path = \"d\" }\n";
        let fuzz = "[patch.crates-io]\nd = { path = \"d\" }\n";
        assert_eq!(patches_of(root, fuzz).apart().len(), 1);
    }

    #[test]
    fn the_cargo_config_patch_comes_before_a_manifest_patch() {
        // Cargo takes the config's patch of smallvec over `Cargo.toml`'s,
        // so both builds follow the config one.
        let text = "[patch.crates-io]\nsmallvec = { path = \"vendor/smallvec2\" }\n";
        let config =
            patches_in(".cargo/config.toml", text, |path| resolved("/repo", path)).unwrap();
        let manifests = patches_of(ROOT_PATCH, NO_PATCH);
        let patches = Patches::new(manifests.root, manifests.fuzz, config.clone());
        assert_eq!(patches.of("smallvec"), Patched::Same);
        assert_eq!(patches.root[0], config[0]);
        assert_eq!(patches.root.len(), 2);
        assert_eq!(patches.fuzz, config);
        assert_eq!(patches.apart(), []);
    }
}
