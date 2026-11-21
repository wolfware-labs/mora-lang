//! What each build resolves for `mora-syntax` and the crates it is built on,
//! asked of `cargo metadata`, and how the two builds compare.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

use cargo_metadata::semver::Version;
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Node, Package, PackageId};

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

/// How a package depends on another, of the kinds the closure follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A normal dependency, built for the platform the package is built for.
    Normal,
    /// A build dependency, built for the host to run the build script.
    Build,
}

impl Kind {
    /// The kind of a dependency of `kind`; `None` for a dev dependency,
    /// which is not code the fuzzer runs.
    fn of(kind: DependencyKind) -> Option<Self> {
        match kind {
            DependencyKind::Normal => Some(Self::Normal),
            DependencyKind::Build => Some(Self::Build),
            _ => None,
        }
    }
}

/// A package another is built on, and how.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dep {
    /// The package depended on.
    pub built: Built,
    /// Whether it is a normal or a build dependency.
    pub kind: Kind,
}

impl fmt::Display for Dep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            Kind::Normal => write!(f, "{}", self.built),
            Kind::Build => write!(f, "{} as a build dependency", self.built),
        }
    }
}

/// One package of [`FUZZED`]'s dependency closure as one build makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The package.
    pub built: Built,
    /// What it is built on: its normal and build dependencies, a package it
    /// depends on both ways listed once for each, sorted.
    pub deps: Vec<Dep>,
    /// The features cargo turns on for it, sorted.
    pub features: Vec<String>,
    /// The crates its manifest declares as optional dependencies, by name
    /// and kind, each with the features that turn it on.
    pub optional: BTreeMap<(String, Kind), Vec<String>>,
}

/// The optional dependencies `package` declares, by crate name and kind,
/// each with the features that turn it on: those that list `dep:name` or
/// `name/feature`, and those that list `name` when that is the feature
/// cargo makes for the dependency, `name` being the name the manifest gives
/// it. Cargo makes that feature, listed as `name = ["dep:name"]`, for an
/// optional dependency no feature names with `dep:`; otherwise a feature
/// called `name` is one like any other.
fn optional(package: &Package) -> BTreeMap<(String, Kind), Vec<String>> {
    let mut found: BTreeMap<(String, Kind), Vec<String>> = BTreeMap::new();
    let declared = package
        .dependencies
        .iter()
        .filter(|dep| dep.optional)
        .filter_map(|dep| Some((dep, Kind::of(dep.kind)?)));
    for (dep, kind) in declared {
        let name = dep.rename.as_deref().unwrap_or(&dep.name);
        let listed = format!("dep:{name}");
        let implicit = package
            .features
            .get(name)
            .is_some_and(|values| values.contains(&listed));
        let turns_on = |value: &String| match value.split_once('/') {
            Some((dep, _)) => dep == name,
            None => *value == listed || (implicit && value == name),
        };
        let features = found.entry((dep.name.clone(), kind)).or_default();
        features.extend(
            package
                .features
                .iter()
                .filter(|(_, values)| values.iter().any(turns_on))
                .map(|(feature, _)| feature.clone()),
        );
        features.sort();
        features.dedup();
    }
    found
}

impl Unit {
    /// [`FUZZED`], from a path, and every package it is built on through
    /// normal and build dependencies, recursively and on any platform, as
    /// `metadata` resolves them, sorted; `None` when the resolve has no such
    /// crate. A build dependency and what it is built on are built for the
    /// host, to run the build script.
    pub fn closure(metadata: &Metadata) -> Option<Vec<Unit>> {
        let resolve = metadata.resolve.as_ref()?;
        let packages: HashMap<&PackageId, &Package> = metadata
            .packages
            .iter()
            .map(|package| (&package.id, package))
            .collect();
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
            let on: Vec<(&PackageId, Kind)> = node
                .deps
                .iter()
                .flat_map(|dep| {
                    dep.dep_kinds
                        .iter()
                        .filter_map(|info| Some((&dep.pkg, Kind::of(info.kind)?)))
                })
                .collect();
            next.extend(on.iter().map(|(id, _)| *id));
            let mut deps: Vec<Dep> = on
                .iter()
                .filter_map(|(id, kind)| {
                    Some(Dep {
                        built: built.get(id)?.clone(),
                        kind: *kind,
                    })
                })
                .collect();
            deps.sort();
            deps.dedup();
            let mut features: Vec<String> = node.features.iter().map(ToString::to_string).collect();
            features.sort();
            units.push(Unit {
                built: built.get(id)?.clone(),
                deps,
                features,
                optional: optional(packages.get(id)?),
            });
        }
        units.sort_by(|a, b| a.built.cmp(&b.built));
        Some(units)
    }
}

/// The `cargo update` that moves the workspace from one registry version of
/// a crate to the fuzz crate's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The workspace's package, as `cargo update -p` takes it: `name@version`,
    /// or with its source in front when that names more than one package of
    /// the workspace.
    pub spec: String,
    /// The fuzz crate's version.
    pub precise: Version,
}

impl Update {
    /// The update from `root` to `fuzz`, two versions of one crate, when
    /// both come from the same registry; `workspace` is every package of the
    /// workspace's resolve, which tells whether `name@version` is enough.
    fn of(root: &Built, fuzz: &Built, workspace: &[Built]) -> Option<Self> {
        if root.from != fuzz.from || !root.is_registry() {
            return None;
        }
        let named = workspace
            .iter()
            .filter(|package| package.name == root.name && package.version == root.version)
            .count();
        let short = format!("{}@{}", root.name, root.version);
        let spec = if named > 1 {
            format!("{}#{short}", root.from)
        } else {
            short
        };
        Some(Self {
            spec,
            precise: fuzz.version.clone(),
        })
    }
}

impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`cargo update -p {} --precise {}`",
            self.spec, self.precise
        )
    }
}

/// An optional dependency only one build turns on, through features only
/// that build turns on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Optional {
    /// The dependency.
    pub dep: Dep,
    /// The features that turn it on.
    pub features: Vec<String>,
}

impl fmt::Display for Optional {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (by {})",
            self.dep,
            listed(&self.features, |name| format!("`{name}`"))
        )
    }
}

/// How the fuzz crate builds a package of [`FUZZED`]'s closure differently
/// from the workspace.
#[derive(Debug, PartialEq, Eq)]
pub enum Drift {
    /// At another version, or from another source: the workspace's and the
    /// fuzz crate's, and the update that moves the workspace to the fuzz
    /// crate's version, when one does.
    Built {
        root: Built,
        fuzz: Built,
        update: Option<Update>,
    },
    /// The same package, built on other dependencies: those only the
    /// workspace builds it on, and those only the fuzz crate does. A
    /// dependency of the same kind at another version, or from another
    /// source, shows here only when its own difference is not reported.
    Deps {
        package: Built,
        root: Vec<Dep>,
        fuzz: Vec<Dep>,
    },
    /// The same package with other features: those only the workspace
    /// turns on, and those only the fuzz crate does, with the optional
    /// dependencies those features turn on.
    Features {
        package: Built,
        root: Vec<String>,
        fuzz: Vec<String>,
        root_deps: Vec<Optional>,
        fuzz_deps: Vec<Optional>,
    },
}

/// `items`, joined with commas, or `none`.
fn listed<T>(items: &[T], show: impl Fn(&T) -> String) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items.iter().map(show).collect::<Vec<_>>().join(", ")
}

/// The features a build turns on, and the optional dependencies they turn
/// on, if any.
fn features_and_deps(features: &[String], deps: &[Optional]) -> String {
    let shown = listed(features, |name| format!("`{name}`"));
    if deps.is_empty() {
        return shown;
    }
    format!(
        "{shown}, which turn on the optional dependencies {}",
        listed(deps, ToString::to_string)
    )
}

/// Advice to run `update`, to follow a difference it settles; empty when
/// there is none.
fn advice(update: Option<&Update>) -> String {
    update.map_or_else(String::new, |update| {
        format!("; to move the workspace to the fuzz crate's version instead, run {update}")
    })
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Built { root, fuzz, update } => write!(
                f,
                "{}: {ROOT_MANIFEST} builds {} from {}, {FUZZ_MANIFEST} builds {} from {}{}",
                fuzz.name,
                root.version,
                root.from,
                fuzz.version,
                fuzz.from,
                advice(update.as_ref())
            ),
            Self::Deps {
                package,
                root,
                fuzz,
            } => write!(
                f,
                "{package}: dependencies only {ROOT_MANIFEST} builds it on: {}; only \
                 {FUZZ_MANIFEST}: {}",
                listed(root, ToString::to_string),
                listed(fuzz, ToString::to_string),
            ),
            Self::Features {
                package,
                root,
                fuzz,
                root_deps,
                fuzz_deps,
            } => write!(
                f,
                "{package}: features only {ROOT_MANIFEST} turns on: {}; only {FUZZ_MANIFEST}: {}; \
                 cargo turns a feature on for every crate of a build when one crate asks for it, \
                 so it can come from a crate {FUZZED} does not depend on, such as another \
                 workspace member: turn it on in {FUZZ_MANIFEST}, or off in the workspace",
                features_and_deps(root, root_deps),
                features_and_deps(fuzz, fuzz_deps)
            ),
        }
    }
}

/// The items of `a` that `b` lacks.
fn missing<T: PartialEq + Clone>(a: &[T], b: &[T]) -> Vec<T> {
    a.iter().filter(|item| !b.contains(item)).cloned().collect()
}

/// The package of `candidates` cargo would not let into one resolve with
/// `built`, preferring one from the same source; `None` when there is none.
fn counterpart<'a>(
    candidates: impl Iterator<Item = &'a Built> + Clone,
    built: &Built,
) -> Option<&'a Built> {
    let mut same = candidates.filter(|other| other.same_crate(built));
    same.clone()
        .find(|other| other.from == built.from)
        .or_else(|| same.next())
}

/// The dependencies only one build builds a package on.
struct Apart {
    /// The dependencies only the workspace builds it on.
    root: Vec<Dep>,
    /// The dependencies only the fuzz crate builds it on.
    fuzz: Vec<Dep>,
    /// The optional dependencies only the workspace builds it on, through
    /// features only the workspace turns on.
    root_optional: Vec<Optional>,
    /// The same for the fuzz crate.
    fuzz_optional: Vec<Optional>,
}

/// The optional dependencies of `deps` that `extra`, features only one
/// build turns on for `unit`, turn on; they leave `deps`.
fn take_optional(deps: &mut Vec<Dep>, unit: &Unit, extra: &[String]) -> Vec<Optional> {
    let mut taken = Vec::new();
    deps.retain(|dep| {
        let features: Vec<String> = unit
            .optional
            .get(&(dep.built.name.clone(), dep.kind))
            .into_iter()
            .flatten()
            .filter(|feature| extra.contains(feature))
            .cloned()
            .collect();
        if features.is_empty() {
            return true;
        }
        taken.push(Optional {
            dep: dep.clone(),
            features,
        });
        false
    });
    taken
}

/// How `fuzz` builds a package on other dependencies than `root`, the same
/// package in the workspace. A pair of one crate at two versions or sources,
/// of one kind, is left out when `reported`, the pairs of packages whose own
/// difference is reported, holds it. A dependency only one build has is an
/// optional one that build's own features turn on, or a difference of its
/// own.
fn apart(root: &Unit, fuzz: &Unit, reported: &[(&Built, &Built)]) -> Apart {
    let mut root_alone = missing(&root.deps, &fuzz.deps);
    let mut fuzz_alone = Vec::new();
    let (mut root_paired, mut fuzz_paired) = (Vec::new(), Vec::new());
    for dep in missing(&fuzz.deps, &root.deps) {
        let same_kind = root_alone
            .iter()
            .filter(|other| other.kind == dep.kind)
            .map(|other| &other.built);
        let Some(other) = counterpart(same_kind, &dep.built).cloned() else {
            fuzz_alone.push(dep);
            continue;
        };
        let other = Dep {
            built: other,
            kind: dep.kind,
        };
        root_alone.retain(|item| *item != other);
        if !reported.contains(&(&other.built, &dep.built)) {
            root_paired.push(other);
            fuzz_paired.push(dep);
        }
    }
    let root_optional = take_optional(
        &mut root_alone,
        root,
        &missing(&root.features, &fuzz.features),
    );
    let fuzz_optional = take_optional(
        &mut fuzz_alone,
        fuzz,
        &missing(&fuzz.features, &root.features),
    );
    root_paired.extend(root_alone);
    fuzz_paired.extend(fuzz_alone);
    root_paired.sort();
    fuzz_paired.sort();
    Apart {
        root: root_paired,
        fuzz: fuzz_paired,
        root_optional,
        fuzz_optional,
    }
}

/// How `fuzz` builds [`FUZZED`]'s closure differently from `root`, each the
/// closure of one build; `workspace` is every package of the workspace's
/// resolve. A package both builds make alike is compared on its
/// dependencies and features; one `root` builds at another compatible
/// version or from another source is a difference of its own. A package
/// only one build has shows in the dependencies of the package above it,
/// or, when features only that build turns on turn it on, with those
/// features.
pub fn drift(root: &[Unit], fuzz: &[Unit], workspace: &[Built]) -> Vec<Drift> {
    let roots = root.iter().map(|unit| &unit.built);
    let counterparts: Vec<(&Built, &Built)> = fuzz
        .iter()
        .filter(|unit| !root.iter().any(|other| other.built == unit.built))
        .filter_map(|unit| Some((counterpart(roots.clone(), &unit.built)?, &unit.built)))
        .collect();
    let mut found = Vec::new();
    for unit in fuzz {
        let Some(same) = root.iter().find(|other| other.built == unit.built) else {
            if let Some((other, _)) = counterparts.iter().find(|(_, built)| **built == unit.built) {
                found.push(Drift::Built {
                    root: (*other).clone(),
                    fuzz: unit.built.clone(),
                    update: Update::of(other, &unit.built, workspace),
                });
            }
            continue;
        };
        let deps = apart(same, unit, &counterparts);
        if !deps.root.is_empty() || !deps.fuzz.is_empty() {
            found.push(Drift::Deps {
                package: unit.built.clone(),
                root: deps.root,
                fuzz: deps.fuzz,
            });
        }
        if same.features != unit.features {
            found.push(Drift::Features {
                package: unit.built.clone(),
                root: missing(&same.features, &unit.features),
                fuzz: missing(&unit.features, &same.features),
                root_deps: deps.root_optional,
                fuzz_deps: deps.fuzz_optional,
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
            optional: BTreeMap::new(),
        }
    }

    /// A normal dependency on `built`.
    fn normal(built: Built) -> Dep {
        Dep {
            built,
            kind: Kind::Normal,
        }
    }

    /// `unit` built on `deps`, normal dependencies.
    fn on(built: Built, deps: &[Built]) -> Unit {
        Unit {
            deps: deps.iter().cloned().map(normal).collect(),
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
        Drift::Built {
            update: Update::of(&root, &fuzz, &[]),
            root,
            fuzz,
        }
    }

    /// [`drift`], for a workspace whose packages are those of its closure.
    fn compare(root: &[Unit], fuzz: &[Unit]) -> Vec<Drift> {
        let workspace: Vec<Built> = root.iter().map(|unit| unit.built.clone()).collect();
        drift(root, fuzz, &workspace)
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
        assert_eq!(compare(&root, &fuzz), []);
        assert_eq!(compare(&root, &[]), []);
        assert_eq!(compare(&[], &fuzz), []);
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
        let found = compare(&root, &fuzz);
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
        assert_eq!(compare(&root, &fuzz), []);
    }

    #[test]
    fn a_shared_crate_from_another_source_drifts() {
        let found = compare(
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
        let found = compare(
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
            compare(&root, &fuzz),
            [drift_of(
                registry("unicode-ident", "1.0.27"),
                vendored.clone()
            )]
        );
        // A dependency only one build has shows in the package built on it,
        // and so does one the other build takes as another kind.
        let cc = registry("cc", "1.2.0");
        let for_host = Dep {
            built: cc.clone(),
            kind: Kind::Build,
        };
        let root = [on(macro2.clone(), std::slice::from_ref(&cc))];
        let fuzz = [
            Unit {
                deps: vec![for_host.clone(), normal(vendored.clone())],
                ..unit(macro2.clone())
            },
            unit(vendored.clone()),
        ];
        let found = compare(&root, &fuzz);
        assert_eq!(
            found,
            [Drift::Deps {
                package: macro2,
                root: vec![normal(cc)],
                fuzz: vec![for_host, normal(vendored)],
            }]
        );
        assert_eq!(
            found[0].to_string(),
            format!(
                "proc-macro2 1.0.106 from {CRATES_IO}: dependencies only Cargo.toml builds it on: \
                 cc 1.2.0 from {CRATES_IO}; only fuzz/Cargo.toml: cc 1.2.0 from {CRATES_IO} as a \
                 build dependency, unicode-ident 1.0.28 from /repo/vendor/unicode-ident"
            )
        );
    }

    #[test]
    fn a_package_with_other_features_drifts() {
        let smallvec = registry("smallvec", "1.16.4");
        let found = compare(
            &[with(smallvec.clone(), &["const_new", "union"])],
            &[with(smallvec.clone(), &["const_new"])],
        );
        assert_eq!(
            found,
            [Drift::Features {
                package: smallvec,
                root: vec!["union".into()],
                fuzz: Vec::new(),
                root_deps: Vec::new(),
                fuzz_deps: Vec::new(),
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
    fn a_dependency_on_a_copy_the_workspace_also_builds_drifts() {
        // Another workspace crate takes the vendored unicode-ident, so the
        // workspace builds that copy too; only proc-macro2's dependencies
        // tell the two builds apart.
        let vendored = built("unicode-ident", "1.0.99", "/repo/vendor/unicode-ident");
        let registered = registry("unicode-ident", "1.0.27");
        let macro2 = registry("proc-macro2", "1.0.106");
        let root = [
            on(macro2.clone(), std::slice::from_ref(&registered)),
            unit(registered.clone()),
            unit(vendored.clone()),
        ];
        let fuzz = [
            on(macro2.clone(), std::slice::from_ref(&vendored)),
            unit(vendored.clone()),
        ];
        let found = compare(&root, &fuzz);
        assert_eq!(found.len(), 1, "{found:?}");
        let shown = found[0].to_string();
        assert!(shown.starts_with("proc-macro2 1.0.106"), "{shown}");
        assert!(
            shown.contains("unicode-ident 1.0.99 from /repo/vendor"),
            "{shown}"
        );
    }

    #[test]
    fn the_counterpart_from_the_same_source_is_preferred() {
        let root = [
            unit(built("smallvec", "1.16.5", VENDOR)),
            unit(registry("smallvec", "1.16.6")),
        ];
        let fuzz = [unit(registry("smallvec", "1.16.4"))];
        let found = compare(&root, &fuzz);
        assert_eq!(found.len(), 1, "{found:?}");
        let shown = found[0].to_string();
        assert!(
            shown.contains("cargo update -p smallvec@1.16.6 --precise 1.16.4"),
            "{shown}"
        );
    }

    /// `on(built, deps)` with `features`, declaring indexmap as an optional
    /// normal dependency that `preserve_order` turns on.
    fn optional_on(built: Built, deps: &[Built], features: &[&str]) -> Unit {
        let indexmap = ("indexmap".to_owned(), Kind::Normal);
        Unit {
            optional: [(indexmap, vec!["preserve_order".to_owned()])].into(),
            features: with(built.clone(), features).features,
            ..on(built, deps)
        }
    }

    #[test]
    fn an_optional_dependency_only_one_build_turns_on_is_a_feature_difference() {
        let json = registry("serde_json", "1.0.154");
        let indexmap = registry("indexmap", "2.12.0");
        let memchr = registry("memchr", "2.8.0");
        let both = [memchr.clone()];
        let more = [memchr.clone(), indexmap.clone()];
        let found = compare(
            &[optional_on(json.clone(), &more, &["preserve_order", "std"])],
            &[optional_on(json.clone(), &both, &["std"])],
        );
        let turned_on = Optional {
            dep: normal(indexmap.clone()),
            features: vec!["preserve_order".into()],
        };
        assert_eq!(
            found,
            [Drift::Features {
                package: json.clone(),
                root: vec!["preserve_order".into()],
                fuzz: Vec::new(),
                root_deps: vec![turned_on],
                fuzz_deps: Vec::new(),
            }]
        );
        let shown = found[0].to_string();
        assert!(
            shown.starts_with(&format!(
                "serde_json 1.0.154 from {CRATES_IO}: features only Cargo.toml turns on: \
                 `preserve_order`, which turn on the optional dependencies indexmap 2.12.0 from \
                 {CRATES_IO} (by `preserve_order`); only fuzz/Cargo.toml: none; "
            )),
            "{shown}"
        );
        // The same the other way round.
        let found = compare(
            &[optional_on(json.clone(), &both, &["std"])],
            &[optional_on(json.clone(), &more, &["preserve_order", "std"])],
        );
        assert!(
            matches!(&found[..], [Drift::Features { fuzz_deps, .. }] if fuzz_deps.len() == 1),
            "{found:?}"
        );
        // A feature both builds turn on does not account for it: the
        // dependency is a difference of its own.
        let found = compare(
            &[optional_on(json.clone(), &more, &["preserve_order", "std"])],
            &[optional_on(json.clone(), &both, &["preserve_order"])],
        );
        assert!(
            matches!(&found[..], [Drift::Deps { root, .. }, Drift::Features { root_deps, .. }]
                if *root == [normal(indexmap.clone())] && root_deps.is_empty()),
            "{found:?}"
        );
        // Nor does a feature that does not turn it on.
        let found = compare(
            &[optional_on(
                json.clone(),
                &[memchr.clone(), registry("ryu", "1.0.20")],
                &["std"],
            )],
            &[optional_on(json.clone(), &both, &[])],
        );
        assert!(
            matches!(&found[..], [Drift::Deps { .. }, Drift::Features { .. }]),
            "{found:?}"
        );
    }

    /// A package as `cargo metadata` reports it, with `dependencies`, each
    /// `(name, rename, kind, optional)` and `kind` as cargo writes it, and
    /// `features` as cargo lists them, the implicit ones included.
    fn declaring(
        dependencies: &[(&str, Option<&str>, Option<&str>, bool)],
        features: serde_json::Value,
    ) -> Package {
        let dependencies: Vec<serde_json::Value> = dependencies
            .iter()
            .map(|(name, rename, kind, optional)| {
                serde_json::json!({
                    "name": name, "source": CRATES_IO, "req": "^1", "kind": kind,
                    "optional": optional, "uses_default_features": true, "features": [],
                    "target": null, "rename": rename, "registry": null, "path": null,
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "name": "leaf", "version": "1.0.0", "id": "leaf 1.0.0", "source": null,
            "dependencies": dependencies, "targets": [], "features": features,
            "manifest_path": "/repo/leaf/Cargo.toml",
        }))
        .unwrap()
    }

    #[test]
    fn an_optional_dependency_is_turned_on_by_the_features_that_list_it() {
        let twig = |rename| [("twig", rename, None, true)];
        let found = |deps: &[_], features| optional(&declaring(deps, features));
        let turned_on = |features: &[&str]| {
            BTreeMap::from([(
                ("twig".to_owned(), Kind::Normal),
                features.iter().map(|name| (*name).to_owned()).collect(),
            )])
        };
        // Cargo lists the feature named after an optional dependency that no
        // feature names with `dep:`.
        let implicit = serde_json::json!({ "twig": ["dep:twig"] });
        assert_eq!(found(&twig(None), implicit), turned_on(&["twig"]));
        // A renamed dependency goes by the name the manifest gives it.
        let renamed = serde_json::json!({ "f": ["dep:t"] });
        assert_eq!(found(&twig(Some("t")), renamed), turned_on(&["f"]));
        let renamed = serde_json::json!({ "t": ["dep:t"] });
        assert_eq!(found(&twig(Some("t")), renamed), turned_on(&["t"]));
        // `twig/extra` turns twig on; `twig?/extra` only asks for `extra` when
        // something else does.
        let feature = serde_json::json!({ "f": ["twig/extra"], "twig": ["dep:twig"] });
        assert_eq!(found(&twig(None), feature), turned_on(&["f", "twig"]));
        let weak = serde_json::json!({ "g": ["dep:twig"], "w": ["twig?/extra"] });
        assert_eq!(found(&twig(None), weak), turned_on(&["g"]));
        // Once `dep:twig` names it, a feature called `twig` is one like any
        // other.
        let named = serde_json::json!({ "g": ["dep:twig"], "other": [], "twig": ["other"] });
        assert_eq!(found(&twig(None), named), turned_on(&["g"]));
        // A bare `twig` is the feature called `twig`: it turns twig on when
        // that is the feature cargo makes for it, and not otherwise.
        let bare = serde_json::json!({ "f": ["twig"], "twig": ["dep:twig"] });
        assert_eq!(found(&twig(None), bare), turned_on(&["f", "twig"]));
        let bare = serde_json::json!({ "g": ["dep:twig"], "h": ["twig"], "twig": [] });
        assert_eq!(found(&twig(None), bare), turned_on(&["g"]));
        // An optional build dependency is told from a normal one.
        let build = [("twig", None, Some("build"), true)];
        let feature = serde_json::json!({ "g": ["dep:twig"] });
        assert_eq!(
            found(&build, feature),
            BTreeMap::from([(("twig".to_owned(), Kind::Build), vec!["g".to_owned()])])
        );
    }

    #[test]
    fn the_update_names_the_source_when_the_version_names_two_packages() {
        let root = [
            unit(registry("smallvec", "1.16.6")),
            unit(built("smallvec", "1.16.6", VENDOR)),
        ];
        let found = compare(&root, &[unit(registry("smallvec", "1.16.7"))]);
        let shown = found[0].to_string();
        assert!(
            shown.ends_with(&format!(
                "run `cargo update -p {CRATES_IO}#smallvec@1.16.6 --precise 1.16.7`"
            )),
            "{shown}"
        );
    }

    #[test]
    fn a_workspace_crate_from_another_folder_drifts() {
        let found = compare(
            &[unit(syntax())],
            &[unit(built(FUZZED, "0.0.0", "/elsewhere/mora-syntax"))],
        );
        assert_eq!(found.len(), 1);
    }
}
