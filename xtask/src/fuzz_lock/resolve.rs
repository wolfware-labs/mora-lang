//! What each build resolves for `mora-syntax` and the crates it depends on,
//! asked of `cargo metadata`, and how the two builds compare.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

use cargo_metadata::semver::Version;
use cargo_metadata::{
    DependencyKind, Metadata, MetadataCommand, Node, NodeDep, Package, PackageId,
};

use super::Compatible;
use crate::paths::{FUZZ_MANIFEST, ROOT_MANIFEST};

/// The crate the fuzz targets test.
pub const FUZZED: &str = "mora-syntax";

/// A package one build resolves, as `cargo metadata` reports it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Built {
    /// The crate's name.
    pub name: String,
    /// The version cargo builds.
    pub version: Version,
    /// Where cargo builds it from: the source cargo reports for a registry
    /// or git package, or the folder of the manifest of a path package.
    pub from: String,
}

impl Built {
    /// A package of a resolve and where cargo builds it from: its source,
    /// or for a path package the folder of its manifest, with symbolic links
    /// resolved, so that one folder reached two ways reads the same.
    pub fn of(package: &Package) -> Self {
        let from = package.source.as_ref().map_or_else(
            || {
                let dir = package
                    .manifest_path
                    .parent()
                    .unwrap_or(&package.manifest_path);
                fs::canonicalize(dir)
                    .map_or_else(|_| dir.to_string(), |dir| dir.display().to_string())
            },
            |source| source.repr.clone(),
        );
        Self {
            name: package.name.to_string(),
            version: package.version.clone(),
            from,
        }
    }

    /// Whether cargo would keep only one of the two in a resolve: the same
    /// name and semver-compatible versions, from any source.
    fn same_crate(&self, other: &Built) -> bool {
        self.name == other.name && Compatible::of(&self.version) == Compatible::of(&other.version)
    }

    /// Whether it comes from a registry, where cargo can lock another version.
    fn is_registry(&self) -> bool {
        self.from.starts_with("registry+") || self.from.starts_with("sparse+")
    }
}

impl fmt::Display for Built {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} from {}", self.name, self.version, self.from)
    }
}

/// One package of [`FUZZED`]'s dependency closure as one build makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The package.
    pub built: Built,
    /// What it is built on: its normal and build dependencies, sorted.
    pub deps: Vec<Built>,
    /// The features cargo turns on for it, sorted.
    pub features: Vec<String>,
}

/// Whether a resolved dependency is of one of `kinds`, on any platform.
fn of_kind(dep: &NodeDep, kinds: &[DependencyKind]) -> bool {
    dep.dep_kinds.iter().any(|info| kinds.contains(&info.kind))
}

impl Unit {
    /// [`FUZZED`], from a path, and every package it reaches through normal
    /// dependencies on any platform, as `metadata` resolves them, sorted;
    /// `None` when the resolve has no such crate.
    pub fn closure(metadata: &Metadata) -> Option<Vec<Unit>> {
        let resolve = metadata.resolve.as_ref()?;
        let built: HashMap<&PackageId, Built> = metadata
            .packages
            .iter()
            .map(|package| (&package.id, Built::of(package)))
            .collect();
        let nodes: HashMap<&PackageId, &Node> =
            resolve.nodes.iter().map(|node| (&node.id, node)).collect();
        let start = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == FUZZED && package.source.is_none())?;
        let mut seen = HashSet::new();
        let mut next = vec![&start.id];
        let mut units = Vec::new();
        while let Some(id) = next.pop() {
            if !seen.insert(id) {
                continue;
            }
            let node = nodes.get(id)?;
            next.extend(
                node.deps
                    .iter()
                    .filter(|dep| of_kind(dep, &[DependencyKind::Normal]))
                    .map(|dep| &dep.pkg),
            );
            let mut deps: Vec<Built> = node
                .deps
                .iter()
                .filter(|dep| of_kind(dep, &[DependencyKind::Normal, DependencyKind::Build]))
                .filter_map(|dep| built.get(&dep.pkg).cloned())
                .collect();
            deps.sort();
            deps.dedup();
            let mut features: Vec<String> = node.features.iter().map(ToString::to_string).collect();
            features.sort();
            units.push(Unit {
                built: built.get(id)?.clone(),
                deps,
                features,
            });
        }
        units.sort_by(|a, b| a.built.cmp(&b.built));
        Some(units)
    }
}

/// How the fuzz crate builds a package of [`FUZZED`]'s closure differently
/// from the workspace.
#[derive(Debug, PartialEq, Eq)]
pub enum Drift {
    /// At another version, or from another source: the workspace's and the
    /// fuzz crate's.
    Built { root: Built, fuzz: Built },
    /// The same package, built on other dependencies: those only the
    /// workspace builds it on, and those only the fuzz crate does.
    Deps {
        package: Built,
        root: Vec<Built>,
        fuzz: Vec<Built>,
    },
    /// The same package with other features: those only the workspace
    /// turns on, and those only the fuzz crate does.
    Features {
        package: Built,
        root: Vec<String>,
        fuzz: Vec<String>,
    },
}

/// `items`, joined with commas, or `none`.
fn listed<T>(items: &[T], show: impl Fn(&T) -> String) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items.iter().map(show).collect::<Vec<_>>().join(", ")
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Built { root, fuzz } => {
                write!(
                    f,
                    "{}: {ROOT_MANIFEST} builds {} from {}, {FUZZ_MANIFEST} builds {} from {}",
                    fuzz.name, root.version, root.from, fuzz.version, fuzz.from
                )?;
                if root.from == fuzz.from && root.is_registry() {
                    write!(
                        f,
                        "; to move the workspace to the fuzz crate's version instead, run \
                         `cargo update -p {}@{} --precise {}`",
                        root.name, root.version, fuzz.version
                    )?;
                }
                Ok(())
            }
            Self::Deps {
                package,
                root,
                fuzz,
            } => write!(
                f,
                "{package}: dependencies only {ROOT_MANIFEST} builds it on: {}; only \
                 {FUZZ_MANIFEST}: {}",
                listed(root, ToString::to_string),
                listed(fuzz, ToString::to_string)
            ),
            Self::Features {
                package,
                root,
                fuzz,
            } => write!(
                f,
                "{package}: features only {ROOT_MANIFEST} turns on: {}; only {FUZZ_MANIFEST}: {}; \
                 cargo turns a feature on for every crate of a build when one crate asks for it, \
                 so it can come from a crate {FUZZED} does not depend on, such as another \
                 workspace member: turn it on in {FUZZ_MANIFEST}, or off in the workspace",
                listed(root, |name| format!("`{name}`")),
                listed(fuzz, |name| format!("`{name}`"))
            ),
        }
    }
}

/// The items of `a` that `b` lacks.
fn missing<T: PartialEq + Clone>(a: &[T], b: &[T]) -> Vec<T> {
    a.iter().filter(|item| !b.contains(item)).cloned().collect()
}

/// The dependencies only `root` and only `fuzz` build a package on, without
/// the pairs of one crate at two versions or sources whose fuzz side is a
/// package of `closure`: that package's own difference reports them.
fn apart_deps(root: &[Built], fuzz: &[Built], closure: &[Unit]) -> (Vec<Built>, Vec<Built>) {
    let (mut root, mut fuzz) = (missing(root, fuzz), missing(fuzz, root));
    fuzz.retain(|dep| {
        let reported = closure.iter().any(|unit| &unit.built == dep);
        let pair = root.iter().position(|other| other.same_crate(dep));
        match pair {
            Some(index) if reported => {
                root.remove(index);
                false
            }
            _ => true,
        }
    });
    (root, fuzz)
}

/// How `fuzz` builds [`FUZZED`]'s closure differently from `root`, each the
/// closure of one build. A package both build alike is compared on its
/// dependencies and features; one `root` builds at another compatible
/// version or from another source is a difference of its own. A package
/// only one build has shows in the dependencies of the package above it.
pub fn drift(root: &[Unit], fuzz: &[Unit]) -> Vec<Drift> {
    let mut found = Vec::new();
    for unit in fuzz {
        let Some(same) = root.iter().find(|other| other.built == unit.built) else {
            if let Some(other) = root
                .iter()
                .find(|other| other.built.same_crate(&unit.built))
            {
                found.push(Drift::Built {
                    root: other.built.clone(),
                    fuzz: unit.built.clone(),
                });
            }
            continue;
        };
        let (root_deps, fuzz_deps) = apart_deps(&same.deps, &unit.deps, fuzz);
        if !root_deps.is_empty() || !fuzz_deps.is_empty() {
            found.push(Drift::Deps {
                package: unit.built.clone(),
                root: root_deps,
                fuzz: fuzz_deps,
            });
        }
        if same.features != unit.features {
            found.push(Drift::Features {
                package: unit.built.clone(),
                root: missing(&same.features, &unit.features),
                fuzz: missing(&unit.features, &same.features),
            });
        }
    }
    found
}

/// A build cargo could not resolve: its manifest, relative to the
/// repository root, and cargo's error.
#[derive(Debug, PartialEq, Eq)]
pub struct Unresolved {
    pub manifest: &'static str,
    pub error: String,
}

/// `cargo metadata` for `manifest` under `root`, run from the manifest's
/// folder, so that cargo reads the config a build there reads.
fn command(root: &Path, manifest: &str) -> MetadataCommand {
    let manifest = root.join(manifest);
    let mut command = MetadataCommand::new();
    if let Some(dir) = manifest.parent() {
        command.current_dir(dir);
    }
    command.manifest_path(manifest);
    command
}

/// Runs `command` for `manifest`; cargo's error, without its trailing
/// newline, when it fails.
fn exec(command: &MetadataCommand, manifest: &'static str) -> Result<Metadata, Unresolved> {
    command.exec().map_err(|err| Unresolved {
        manifest,
        error: err.to_string().trim_end().to_owned(),
    })
}

/// What cargo resolves for `manifest` under `root`, which may update its
/// lockfile, or why it cannot.
pub fn resolve(root: &Path, manifest: &'static str) -> Result<Metadata, Unresolved> {
    exec(&command(root, manifest), manifest)
}

/// What cargo resolves for `manifest` under `root` with `--locked`, or why
/// it cannot.
pub fn resolve_locked(root: &Path, manifest: &'static str) -> Result<Metadata, Unresolved> {
    let mut command = command(root, manifest);
    command.other_options(["--locked".to_owned()]);
    exec(&command, manifest)
}

/// [`FUZZED`]'s closure in a resolve of `manifest`.
pub fn closure(metadata: &Metadata, manifest: &'static str) -> Result<Vec<Unit>, Unresolved> {
    Unit::closure(metadata).ok_or_else(|| Unresolved {
        manifest,
        error: format!("it does not build {FUZZED} from a path"),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The source cargo writes for crates.io.
    pub(in crate::fuzz_lock) const CRATES_IO: &str =
        "registry+https://github.com/rust-lang/crates.io-index";

    /// A vendored smallvec folder.
    pub(in crate::fuzz_lock) const VENDOR: &str = "/repo/vendor/smallvec";

    /// A package built from `from`.
    pub(in crate::fuzz_lock) fn built(name: &str, version: &str, from: &str) -> Built {
        Built {
            name: name.into(),
            version: Version::parse(version).unwrap(),
            from: from.into(),
        }
    }

    /// A package built from crates.io.
    pub(in crate::fuzz_lock) fn registry(name: &str, version: &str) -> Built {
        built(name, version, CRATES_IO)
    }

    /// A package with no dependencies and no features.
    fn unit(built: Built) -> Unit {
        Unit {
            built,
            deps: Vec::new(),
            features: Vec::new(),
        }
    }

    /// `unit` built on `deps`.
    fn on(built: Built, deps: &[Built]) -> Unit {
        Unit {
            deps: deps.to_vec(),
            ..unit(built)
        }
    }

    /// `unit` with `features`.
    fn with(built: Built, features: &[&str]) -> Unit {
        Unit {
            features: features.iter().map(|name| (*name).to_owned()).collect(),
            ..unit(built)
        }
    }

    /// The fuzz crate building `fuzz` where the workspace builds `root`.
    pub(in crate::fuzz_lock) fn drift_of(root: Built, fuzz: Built) -> Drift {
        Drift::Built { root, fuzz }
    }

    fn syntax() -> Built {
        built(FUZZED, "0.0.0", "/repo/crates/mora-syntax")
    }

    #[test]
    fn a_package_is_built_from_its_source_or_its_folder() {
        let root = crate::paths::root();
        let metadata = crate::metadata_command(&root).exec().unwrap();
        let mut syntax = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == FUZZED)
            .unwrap()
            .clone();
        // `root` is `xtask/..`: the folder is read with the link resolved.
        let folder = fs::canonicalize(root.join("crates/mora-syntax")).unwrap();
        let path = Built::of(&syntax);
        assert_eq!(path.from, folder.display().to_string());
        assert_eq!(
            (path.name.as_str(), &path.version),
            (FUZZED, &syntax.version)
        );
        // A folder that is not there is kept as cargo reports it.
        syntax.manifest_path = "/no/such/mora-syntax/Cargo.toml".into();
        assert_eq!(Built::of(&syntax).from, "/no/such/mora-syntax");
        syntax.source = Some(cargo_metadata::Source {
            repr: CRATES_IO.into(),
        });
        assert_eq!(Built::of(&syntax).from, CRATES_IO);
    }

    #[test]
    fn builds_of_the_same_packages_do_not_drift() {
        let root = [
            on(syntax(), &[registry("syn", "2.0.119")]),
            with(registry("syn", "2.0.119"), &["full"]),
            unit(registry("syn", "3.0.9")),
        ];
        let fuzz = [
            on(syntax(), &[registry("syn", "2.0.119")]),
            with(registry("syn", "2.0.119"), &["full"]),
        ];
        assert_eq!(drift(&root, &fuzz), []);
        assert_eq!(drift(&root, &[]), []);
        assert_eq!(drift(&[], &fuzz), []);
    }

    #[test]
    fn a_shared_crate_at_another_version_drifts() {
        let root = [
            unit(registry("smallvec", "1.16.6")),
            unit(registry("syn", "2.0.119")),
            unit(registry("zmij", "0.0.4")),
        ];
        let fuzz = [
            unit(registry("smallvec", "1.16.4")),
            unit(registry("syn", "2.0.119")),
            unit(registry("zmij", "0.0.4")),
        ];
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [drift_of(
                registry("smallvec", "1.16.6"),
                registry("smallvec", "1.16.4")
            )]
        );
        assert_eq!(
            found[0].to_string(),
            format!(
                "smallvec: Cargo.toml builds 1.16.6 from {CRATES_IO}, fuzz/Cargo.toml builds \
                 1.16.4 from {CRATES_IO}; to move the workspace to the fuzz crate's version \
                 instead, run `cargo update -p smallvec@1.16.6 --precise 1.16.4`"
            )
        );
    }

    #[test]
    fn another_major_version_is_another_crate() {
        let root = [unit(registry("getrandom", "0.4.3"))];
        let fuzz = [unit(registry("getrandom", "0.3.4"))];
        assert_eq!(drift(&root, &fuzz), []);
    }

    #[test]
    fn a_shared_crate_from_another_source_drifts() {
        let found = drift(
            &[unit(built("smallvec", "1.16.9", VENDOR))],
            &[unit(registry("smallvec", "1.16.4"))],
        );
        assert_eq!(
            found,
            [drift_of(
                built("smallvec", "1.16.9", VENDOR),
                registry("smallvec", "1.16.4")
            )]
        );
        // No `cargo update` advice when the sources differ.
        assert_eq!(
            found[0].to_string(),
            format!(
                "smallvec: Cargo.toml builds 1.16.9 from {VENDOR}, fuzz/Cargo.toml builds 1.16.4 \
                 from {CRATES_IO}"
            )
        );
        let git = "git+https://example.com/smallvec?branch=main#0123abc";
        let moved = drift_of(
            built("smallvec", "1.16.4", git),
            built("smallvec", "1.16.5", git),
        );
        assert!(!moved.to_string().contains("cargo update"), "{moved}");
        let sparse = "sparse+https://index.crates.io/";
        let moved = drift_of(
            built("smallvec", "1.16.4", sparse),
            built("smallvec", "1.16.5", sparse),
        );
        assert!(moved.to_string().contains("cargo update"), "{moved}");
    }

    #[test]
    fn two_folders_at_one_version_drift() {
        let other = "/repo/vendor/smallvec2";
        let found = drift(
            &[unit(built("smallvec", "1.16.4", VENDOR))],
            &[unit(built("smallvec", "1.16.4", other))],
        );
        assert_eq!(
            found,
            [drift_of(
                built("smallvec", "1.16.4", VENDOR),
                built("smallvec", "1.16.4", other)
            )]
        );
    }

    #[test]
    fn a_package_built_on_other_dependencies_drifts() {
        // The workspace has two copies of unicode-ident; the fuzz crate builds
        // proc-macro2 on the other one, which its own difference reports.
        let vendored = built("unicode-ident", "1.0.28", "/repo/vendor/unicode-ident");
        let macro2 = registry("proc-macro2", "1.0.106");
        let root = [
            on(macro2.clone(), &[registry("unicode-ident", "1.0.27")]),
            unit(registry("unicode-ident", "1.0.27")),
        ];
        let fuzz = [
            on(macro2.clone(), std::slice::from_ref(&vendored)),
            unit(vendored.clone()),
        ];
        assert_eq!(
            drift(&root, &fuzz),
            [drift_of(
                registry("unicode-ident", "1.0.27"),
                vendored.clone()
            )]
        );
        // A build dependency at another version is not in the closure, so
        // the package built on it reports it; so does a dependency only one
        // build has.
        let cc = |version| registry("cc", version);
        let root = [on(macro2.clone(), &[cc("1.2.0")])];
        let fuzz = [
            on(macro2.clone(), &[cc("1.2.1"), vendored.clone()]),
            unit(vendored.clone()),
        ];
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [Drift::Deps {
                package: macro2,
                root: vec![cc("1.2.0")],
                fuzz: vec![cc("1.2.1"), vendored],
            }]
        );
        assert_eq!(
            found[0].to_string(),
            format!(
                "proc-macro2 1.0.106 from {CRATES_IO}: dependencies only Cargo.toml builds it on: \
                 cc 1.2.0 from {CRATES_IO}; only fuzz/Cargo.toml: cc 1.2.1 from {CRATES_IO}, \
                 unicode-ident 1.0.28 from /repo/vendor/unicode-ident"
            )
        );
    }

    #[test]
    fn a_package_with_other_features_drifts() {
        let smallvec = registry("smallvec", "1.16.4");
        let found = drift(
            &[with(smallvec.clone(), &["const_new", "union"])],
            &[with(smallvec.clone(), &["const_new"])],
        );
        assert_eq!(
            found,
            [Drift::Features {
                package: smallvec,
                root: vec!["union".into()],
                fuzz: Vec::new(),
            }]
        );
        assert_eq!(
            found[0].to_string(),
            format!(
                "smallvec 1.16.4 from {CRATES_IO}: features only Cargo.toml turns on: `union`; \
                 only fuzz/Cargo.toml: none; cargo turns a feature on for every crate of a build \
                 when one crate asks for it, so it can come from a crate mora-syntax does not \
                 depend on, such as another workspace member: turn it on in fuzz/Cargo.toml, or \
                 off in the workspace"
            )
        );
    }

    #[test]
    fn a_workspace_crate_from_another_folder_drifts() {
        let found = drift(
            &[unit(syntax())],
            &[unit(built(FUZZED, "0.0.0", "/elsewhere/mora-syntax"))],
        );
        assert_eq!(found.len(), 1);
    }
}
