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

use super::{Checked, Compatible, Problem};
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

/// What cargo resolves for `manifest` under `root`, with `--locked` when
/// `locked`, or why it cannot.
pub fn resolve(root: &Path, manifest: &'static str, locked: bool) -> Result<Metadata, Unresolved> {
    let mut command = command(root, manifest);
    if locked {
        command.other_options(["--locked".to_owned()]);
    }
    command.exec().map_err(|err| Unresolved {
        manifest,
        error: err.to_string().trim_end().to_owned(),
    })
}

/// [`FUZZED`]'s closure in a resolve of `manifest`.
fn closure(metadata: &Metadata, manifest: &'static str) -> Result<Vec<Unit>, Unresolved> {
    Unit::closure(metadata).ok_or_else(|| Unresolved {
        manifest,
        error: format!("it does not build {FUZZED} from a path"),
    })
}

/// Resolves the workspace and the fuzz crate under `root` with `--locked`
/// and compares how they build [`FUZZED`]'s closure: other features are
/// warnings, any other difference a problem. The fuzz crate failing to
/// resolve is a problem of the fuzz lockfile; the workspace failing is an
/// error.
pub fn check(root: &Path) -> Result<Checked, Unresolved> {
    let workspace = closure(&resolve(root, ROOT_MANIFEST, true)?, ROOT_MANIFEST)?;
    let fuzz = match resolve(root, FUZZ_MANIFEST, true) {
        Ok(fuzz) => closure(&fuzz, FUZZ_MANIFEST)?,
        Err(unresolved) => {
            return Ok(Checked {
                problems: vec![Problem::Unlocked(unresolved.error)],
                warnings: Vec::new(),
                packages: 0,
            });
        }
    };
    let (warnings, apart): (Vec<Drift>, Vec<Drift>) = drift(&workspace, &fuzz)
        .into_iter()
        .partition(|drift| matches!(drift, Drift::Features { .. }));
    Ok(Checked {
        problems: apart.into_iter().map(Problem::Drift).collect(),
        warnings,
        packages: fuzz.len(),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// The source cargo writes for crates.io.
    pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

    /// A vendored smallvec folder.
    pub const VENDOR: &str = "/repo/vendor/smallvec";

    /// A package built from `from`.
    pub fn built(name: &str, version: &str, from: &str) -> Built {
        Built {
            name: name.into(),
            version: Version::parse(version).unwrap(),
            from: from.into(),
        }
    }

    /// A package built from crates.io.
    pub fn registry(name: &str, version: &str) -> Built {
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

    pub fn drift_of(root: Built, fuzz: Built) -> Drift {
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

/// Small path-only repositories that `cargo metadata` resolves offline.
#[cfg(test)]
mod fixture_tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::tests::{built, drift_of, registry};
    use super::*;
    use crate::fuzz_lock::{check_summary, rebuild, resolve_failure};
    use crate::paths::{FUZZ_LOCK, scratch};

    /// A repository laid out as this one: a workspace with `mora-syntax`
    /// and `tool`, and an excluded `fuzz` crate on `mora-syntax`. Crates.io
    /// is replaced by a folder that holds `leaf` 1.0.0, which `mora-syntax`
    /// depends on.
    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = scratch(name);
            let fixture = Self(root);
            fixture.write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"syntax\", \"tool\"]\nexclude = [\"fuzz\"]\n\
                 resolver = \"3\"\n",
            );
            fixture.write(
                ".cargo/config.toml",
                "[source.crates-io]\nreplace-with = \"fixture\"\n\n[source.fixture]\n\
                 directory = \"registry\"\n",
            );
            fixture.package("syntax", FUZZED, "leaf = \"1\"\n");
            fixture.package("tool", "tool", "mora-syntax = { path = \"../syntax\" }\n");
            fixture.package(
                "fuzz",
                "mora-fuzz",
                "mora-syntax = { path = \"../syntax\" }\n",
            );
            fixture.publish("leaf", "1.0.0");
            fixture.lock(".");
            fixture.lock("fuzz");
            fixture
        }

        fn write(&self, file: &str, text: &str) {
            let path = self.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        /// A library package in `dir` with `dependencies`.
        fn package(&self, dir: &str, name: &str, dependencies: &str) {
            self.write(
                &format!("{dir}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
                     [dependencies]\n{dependencies}"
                ),
            );
            self.write(&format!("{dir}/src/lib.rs"), "");
        }

        /// A `leaf` crate with an `extra` feature in `dir`.
        fn leaf(&self, dir: &str, version: &str) {
            self.library(dir, "leaf", version);
        }

        /// A crate `name` with an `extra` feature in `dir`.
        fn library(&self, dir: &str, name: &str, version: &str) {
            self.write(
                &format!("{dir}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n\n\
                     [features]\nextra = []\n"
                ),
            );
            self.write(&format!("{dir}/src/lib.rs"), "");
        }

        /// Puts `name` at `version` in the folder that stands in for
        /// crates.io.
        fn publish(&self, name: &str, version: &str) {
            let dir = format!("registry/{name}-{version}");
            self.library(&dir, name, version);
            self.write(
                &format!("{dir}/.cargo-checksum.json"),
                &format!("{{\"files\":{{}},\"package\":\"{}\"}}", "0".repeat(64)),
            );
        }

        /// Has cargo write the lockfile of the manifest in `dir`.
        fn lock(&self, dir: &str) {
            let output = Command::new(env!("CARGO"))
                .args(["generate-lockfile", "--offline", "--quiet"])
                .current_dir(self.0.join(dir))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stderr}");
        }

        /// Appends `text` to `file`.
        fn append(&self, file: &str, text: &str) {
            let path = self.0.join(file);
            let before = fs::read_to_string(&path).unwrap();
            fs::write(path, before + text).unwrap();
        }

        fn check(&self) -> Result<Checked, Unresolved> {
            check(&self.0)
        }

        fn fuzz_lock(&self) -> Vec<u8> {
            fs::read(self.0.join(FUZZ_LOCK)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The drifts a check found.
    fn drifts(checked: &Checked) -> Vec<&Drift> {
        checked
            .problems
            .iter()
            .filter_map(|problem| match problem {
                Problem::Drift(drift) => Some(drift),
                Problem::Unlocked(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_fixture_built_alike_passes() {
        let fixture = Fixture::new("fuzz-lock-alike");
        let checked = fixture.check().unwrap();
        assert_eq!(checked.problems, []);
        assert_eq!(checked.packages, 2, "mora-syntax and leaf");
        assert_eq!(rebuild(&fixture.0).unwrap().1, checked);
    }

    #[test]
    fn a_stale_fuzz_lock_fails_the_check_and_the_rebuild_fixes_it() {
        let fixture = Fixture::new("fuzz-lock-stale");
        fixture.leaf("syntax/other", "2.0.0");
        fixture.append(
            "syntax/Cargo.toml",
            "other = { path = \"other\", package = \"leaf\" }\n",
        );
        fixture.lock(".");
        let checked = fixture.check().unwrap();
        let [Problem::Unlocked(error)] = checked.problems.as_slice() else {
            panic!("{checked:?}");
        };
        assert!(error.contains("--locked"), "{error}");
        let (changed, checked) = rebuild(&fixture.0).unwrap();
        assert!(changed);
        assert_eq!(checked.problems, []);
        assert_eq!(fixture.check().unwrap().problems, []);
    }

    #[test]
    fn a_patch_in_the_fuzz_cargo_config_counts() {
        let fixture = Fixture::new("fuzz-lock-config");
        fixture.leaf("alt/leaf", "1.0.0");
        fixture.write(
            "fuzz/.cargo/config.toml",
            "[patch.crates-io]\nleaf = { path = \"../alt/leaf\" }\n",
        );
        fixture.lock("fuzz");
        let checked = fixture.check().unwrap();
        let alt = fs::canonicalize(fixture.0.join("alt/leaf")).unwrap();
        assert_eq!(
            drifts(&checked),
            [&drift_of(
                registry("leaf", "1.0.0"),
                built("leaf", "1.0.0", &alt.display().to_string())
            )]
        );
        // The rebuild cannot fix it and leaves the file as it was.
        let before = fixture.fuzz_lock();
        assert_eq!(rebuild(&fixture.0).unwrap().1, checked);
        assert_eq!(fixture.fuzz_lock(), before);
    }

    #[test]
    fn two_vendored_folders_drift() {
        let fixture = Fixture::new("fuzz-lock-folders");
        fixture.leaf("vendor/a", "1.0.0");
        fixture.leaf("vendor/b", "1.0.0");
        fixture.append(
            "Cargo.toml",
            "\n[patch.crates-io]\nleaf = { path = \"vendor/a\" }\n",
        );
        fixture.append(
            "fuzz/Cargo.toml",
            "\n[patch.crates-io]\nleaf = { path = \"../vendor/b\" }\n",
        );
        fixture.lock(".");
        fixture.lock("fuzz");
        let checked = fixture.check().unwrap();
        let folder = |dir: &str| {
            fs::canonicalize(fixture.0.join(dir))
                .unwrap()
                .display()
                .to_string()
        };
        assert_eq!(
            drifts(&checked),
            [&drift_of(
                built("leaf", "1.0.0", &folder("vendor/a")),
                built("leaf", "1.0.0", &folder("vendor/b"))
            )]
        );
    }

    #[test]
    fn a_second_copy_outside_the_closure_does_not_hide_a_drift() {
        // `tool` takes a vendored leaf 1.0.1 beside the registry's 1.0.0
        // that mora-syntax uses; the fuzz crate patches mora-syntax's leaf
        // to that vendored copy.
        let fixture = Fixture::new("fuzz-lock-copies");
        fixture.leaf("vendor/leaf", "1.0.1");
        fixture.append(
            "tool/Cargo.toml",
            "vendored = { path = \"../vendor/leaf\", package = \"leaf\" }\n",
        );
        fixture.append(
            "fuzz/Cargo.toml",
            "\n[patch.crates-io]\nleaf = { path = \"../vendor/leaf\" }\n",
        );
        fixture.lock(".");
        fixture.lock("fuzz");
        let checked = fixture.check().unwrap();
        let vendored = fs::canonicalize(fixture.0.join("vendor/leaf")).unwrap();
        let vendored = built("leaf", "1.0.1", &vendored.display().to_string());
        assert_eq!(
            drifts(&checked),
            [&drift_of(registry("leaf", "1.0.0"), vendored)]
        );
    }

    #[test]
    fn a_feature_another_member_turns_on_is_a_warning() {
        let fixture = Fixture::new("fuzz-lock-features");
        fixture.append(
            "tool/Cargo.toml",
            "leaf = { version = \"1\", features = [\"extra\"] }\n",
        );
        fixture.lock(".");
        let checked = fixture.check().unwrap();
        assert_eq!(checked.problems, []);
        assert_eq!(check_summary(&checked.problems), None);
        assert_eq!(checked.warnings, [extra_on_leaf()]);
        // The rebuild keeps what it wrote: only a feature differs.
        let (_, rebuilt) = rebuild(&fixture.0).unwrap();
        assert_eq!(rebuilt, checked);
    }

    #[test]
    fn a_feature_and_a_version_apart_fail() {
        let fixture = Fixture::new("fuzz-lock-features-version");
        // The workspace locks twig before 1.0.1 is out, the fuzz crate after.
        fixture.publish("twig", "1.0.0");
        fixture.append("syntax/Cargo.toml", "twig = \"1\"\n");
        fixture.append(
            "tool/Cargo.toml",
            "leaf = { version = \"1\", features = [\"extra\"] }\n",
        );
        fixture.lock(".");
        fixture.publish("twig", "1.0.1");
        fixture.lock("fuzz");
        let checked = fixture.check().unwrap();
        assert_eq!(
            drifts(&checked),
            [&drift_of(
                registry("twig", "1.0.0"),
                registry("twig", "1.0.1")
            )]
        );
        assert_eq!(checked.warnings, [extra_on_leaf()]);
        assert!(check_summary(&checked.problems).is_some());
    }

    /// The `extra` feature only the workspace turns on for `leaf`.
    fn extra_on_leaf() -> Drift {
        Drift::Features {
            package: registry("leaf", "1.0.0"),
            root: vec!["extra".into()],
            fuzz: Vec::new(),
        }
    }

    #[test]
    fn the_closure_follows_normal_dependencies_only() {
        // mora-syntax's build and dev dependencies are not code the fuzzer
        // runs; a build dependency still shows in its dependencies.
        let fixture = Fixture::new("fuzz-lock-closure");
        fixture.package("syntax/builder", "builder", "");
        fixture.package("syntax/tester", "tester", "");
        fixture.append(
            "syntax/Cargo.toml",
            "\n[build-dependencies]\nbuilder = { path = \"builder\" }\n\n\
             [dev-dependencies]\ntester = { path = \"tester\" }\n",
        );
        fixture.lock(".");
        let metadata = resolve(&fixture.0, ROOT_MANIFEST, true).unwrap();
        let closure = Unit::closure(&metadata).unwrap();
        let names: Vec<_> = closure
            .iter()
            .map(|unit| (unit.built.name.as_str(), unit.built.version.to_string()))
            .collect();
        assert_eq!(names, [("leaf", "1.0.0".into()), (FUZZED, "0.0.0".into())]);
        let syntax = &closure[1];
        let deps: Vec<_> = syntax.deps.iter().map(|dep| dep.name.as_str()).collect();
        assert_eq!(deps, ["builder", "leaf"]);
    }

    #[test]
    fn a_crate_outside_the_closure_is_not_compared() {
        // `tool` and the fuzz crate each use a leaf 3 of their own, from two
        // folders; mora-syntax uses neither.
        let fixture = Fixture::new("fuzz-lock-outside");
        fixture.leaf("tool/own", "3.0.0");
        fixture.append(
            "tool/Cargo.toml",
            "own = { path = \"own\", package = \"leaf\" }\n",
        );
        fixture.leaf("fuzz/own", "3.0.1");
        fixture.append(
            "fuzz/Cargo.toml",
            "own = { path = \"own\", package = \"leaf\" }\n",
        );
        fixture.lock(".");
        fixture.lock("fuzz");
        assert_eq!(fixture.check().unwrap().problems, []);
    }

    #[test]
    fn a_workspace_that_does_not_resolve_is_named() {
        let fixture = Fixture::new("fuzz-lock-workspace");
        fixture.leaf("tool/new", "4.0.0");
        fixture.append(
            "tool/Cargo.toml",
            "new = { path = \"new\", package = \"leaf\" }\n",
        );
        let unresolved = fixture.check().unwrap_err();
        assert_eq!(unresolved.manifest, ROOT_MANIFEST);
        let error = rebuild(&fixture.0).unwrap_err();
        assert!(
            error.starts_with("cargo cannot resolve Cargo.toml: "),
            "{error}"
        );
        assert!(!error.contains("fuzz/Cargo.toml"), "{error}");
    }

    #[test]
    fn an_offline_failure_is_not_taken_for_a_requirement() {
        let fixture = Fixture::new("fuzz-lock-offline");
        fixture.write("fuzz/.cargo/config.toml", "[net]\noffline = true\n");
        fixture.append(
            "fuzz/Cargo.toml",
            "far = { git = \"https://example.invalid/far\" }\n",
        );
        let before = fixture.fuzz_lock();
        let error = rebuild(&fixture.0).unwrap_err();
        assert_eq!(fixture.fuzz_lock(), before);
        assert!(error.contains("offline"), "{error}");
        assert!(!error.contains("fix the requirement"), "{error}");
        assert!(
            error.starts_with("cargo cannot fetch what fuzz/Cargo.toml needs"),
            "{error}"
        );
        let unresolved = Unresolved {
            manifest: FUZZ_MANIFEST,
            error: "error: offline".into(),
        };
        assert!(resolve_failure(&unresolved).ends_with("fuzz/Cargo.lock is left as it was"));
    }
}
