//! `cargo xtask fuzz-lock`: keeps `fuzz/Cargo.lock` on the versions
//! `Cargo.lock` locks (spec §20.1).
//!
//! The fuzz crate sits outside the workspace and has a lockfile of its own.
//! Every crate it shares with the workspace, `mora-syntax` and what that
//! depends on, must be locked at the same version in both files, so that
//! fuzzing runs the code the tests run. `--check` fails when a shared crate
//! drifts; the command itself rebuilds the fuzz lockfile from `Cargo.lock`.
//!
//! Two entries are the same crate when cargo would let only one of them in a
//! resolve: the same name, the same source, and versions that are semver
//! compatible. A crate only the fuzz crate uses can then take another major
//! version of a crate the workspace locks without counting as drift.

use std::fmt;

use cargo_metadata::semver::Version;
use toml::{Table, Value};

use crate::paths::{FUZZ_LOCK, FUZZ_MANIFEST, ROOT_LOCK};

/// The key of a lockfile's array of package entries.
const PACKAGE: &str = "package";

/// The source cargo writes for crates.io.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A parsed lockfile; the default one has no packages.
#[derive(Default)]
pub struct Lock(Table);

impl Lock {
    /// Parses the text of a lockfile.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        text.parse().map(Self)
    }

    /// The package entries, in the order of the file.
    fn entries(&self) -> &[Value] {
        self.0
            .get(PACKAGE)
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// The name and version of each package the lockfile locks.
    pub fn packages(&self) -> Vec<(&str, &str)> {
        self.locked()
            .map(|package| (package.name, package.version))
            .collect()
    }

    /// The entries with a name and a valid version.
    fn locked(&self) -> impl Iterator<Item = Locked<'_>> {
        self.entries().iter().filter_map(Locked::read)
    }
}

/// The versions cargo treats as one: those with the same major version, the
/// same minor version below 1.0, or the same patch version below 0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compatible {
    Major(u64),
    Minor(u64),
    Patch(u64),
}

impl Compatible {
    fn of(version: &Version) -> Self {
        match (version.major, version.minor) {
            (0, 0) => Self::Patch(version.patch),
            (0, minor) => Self::Minor(minor),
            (major, _) => Self::Major(major),
        }
    }
}

/// One package entry of a lockfile.
#[derive(Debug, Clone, Copy)]
struct Locked<'a> {
    name: &'a str,
    version: &'a str,
    /// `None` for a path dependency.
    source: Option<&'a str>,
    compatible: Compatible,
}

impl<'a> Locked<'a> {
    fn read(entry: &'a Value) -> Option<Self> {
        let version = entry.get("version")?.as_str()?;
        Some(Self {
            name: entry.get("name")?.as_str()?,
            version,
            source: entry.get("source").and_then(Value::as_str),
            compatible: Compatible::of(&Version::parse(version).ok()?),
        })
    }

    /// Whether cargo would keep only one of the two in a resolve.
    fn same_crate(&self, other: &Locked<'_>) -> bool {
        self.compatible_with(other) && self.source == other.source
    }

    /// The same name and semver-compatible versions, from any source.
    fn compatible_with(&self, other: &Locked<'_>) -> bool {
        self.name == other.name && self.compatible == other.compatible
    }
}

/// Where a package comes from, as messages show it.
fn shown_source(source: Option<&str>) -> &str {
    match source {
        None => "a path",
        Some(CRATES_IO) => "crates.io",
        Some(other) => other,
    }
}

/// A crate `fuzz/Cargo.lock` locks differently from `Cargo.lock`.
#[derive(Debug, PartialEq, Eq)]
pub enum Drift {
    /// The same crate at another version, which rebuilding the fuzz lockfile
    /// moves to the root's.
    Version {
        name: String,
        fuzz: String,
        root: String,
    },
    /// A compatible version from another source, such as a `[patch]` in
    /// `Cargo.toml`, which does not reach the fuzz crate.
    Source {
        name: String,
        fuzz: String,
        fuzz_source: String,
        root: String,
        root_source: String,
    },
}

impl Drift {
    /// Whether rebuilding the fuzz lockfile from `Cargo.lock` can remove it.
    pub fn rebuild_fixes(&self) -> bool {
        matches!(self, Self::Version { .. })
    }
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version { name, fuzz, root } => {
                write!(f, "{FUZZ_LOCK}: {name} {fuzz}, {ROOT_LOCK} has {root}")
            }
            Self::Source {
                name,
                fuzz,
                fuzz_source,
                root,
                root_source,
            } => write!(
                f,
                "{FUZZ_LOCK}: {name} {fuzz} from {fuzz_source}, {ROOT_LOCK} takes {name} {root} \
                 from {root_source}; give {FUZZ_MANIFEST} the `[patch]` that {ROOT_LOCK} follows"
            ),
        }
    }
}

/// The crates `fuzz` locks differently from `root`: at another version of the
/// same crate, or from another source than a compatible version in `root`.
pub fn drift(root: &Lock, fuzz: &Lock) -> Vec<Drift> {
    let root: Vec<Locked<'_>> = root.locked().collect();
    fuzz.locked()
        .filter_map(|package| {
            if let Some(same) = root.iter().find(|other| package.same_crate(other)) {
                return (same.version != package.version).then(|| Drift::Version {
                    name: package.name.to_owned(),
                    fuzz: package.version.to_owned(),
                    root: same.version.to_owned(),
                });
            }
            let other = root.iter().find(|other| package.compatible_with(other))?;
            Some(Drift::Source {
                name: package.name.to_owned(),
                fuzz: package.version.to_owned(),
                fuzz_source: shown_source(package.source).to_owned(),
                root: other.version.to_owned(),
                root_source: shown_source(other.source).to_owned(),
            })
        })
        .collect()
}

/// `root`, with the entries of `fuzz` that are not the same crate as one in
/// `root`.
///
/// Cargo then drops what the fuzz crate does not use and adds what it lacks,
/// keeps every shared crate at the version `root` locks, and writes the file
/// in its own layout. An entry `root` has no counterpart for (another major
/// version, another source, or a version that does not parse) is kept as
/// the fuzz lockfile has it.
pub fn merged(root: &Lock, fuzz: &Lock) -> String {
    let shared: Vec<Locked<'_>> = root.locked().collect();
    let kept = fuzz.entries().iter().filter(|entry| {
        Locked::read(entry)
            .is_none_or(|package| !shared.iter().any(|other| package.same_crate(other)))
    });
    let mut packages = root.entries().to_vec();
    packages.extend(kept.cloned());
    let mut table = root.0.clone();
    table.insert(PACKAGE.to_owned(), Value::Array(packages));
    table.to_string()
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    const HEADER: &str = "# This file is automatically @generated by Cargo.\nversion = 4\n";

    /// A lockfile with one crates.io entry per `(name, version)`.
    fn lock(packages: &[(&str, &str)]) -> Lock {
        let sourced: Vec<_> = packages
            .iter()
            .map(|(name, version)| (*name, *version, Some(CRATES_IO)))
            .collect();
        lock_from(&sourced)
    }

    /// A lockfile with one entry per `(name, version, source)`.
    fn lock_from(packages: &[(&str, &str, Option<&str>)]) -> Lock {
        let mut text = HEADER.to_owned();
        for (name, version, source) in packages {
            // Writing to a String cannot fail.
            let _ = write!(
                text,
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
            );
            if let Some(source) = source {
                let _ = writeln!(text, "source = \"{source}\"");
            }
        }
        Lock::parse(&text).unwrap()
    }

    fn version(name: &str, fuzz: &str, root: &str) -> Drift {
        Drift::Version {
            name: name.into(),
            fuzz: fuzz.into(),
            root: root.into(),
        }
    }

    #[test]
    fn packages_read_each_entry() {
        let lock = lock(&[("cc", "1.10.0"), ("syn", "2.0.119"), ("syn", "3.0.9")]);
        assert_eq!(
            lock.packages(),
            [("cc", "1.10.0"), ("syn", "2.0.119"), ("syn", "3.0.9")]
        );
        assert_eq!(Lock::parse(HEADER).unwrap().packages(), []);
        assert_eq!(Lock::default().packages(), []);
    }

    #[test]
    fn entries_without_a_name_and_a_valid_version_are_skipped() {
        let text = format!(
            "{HEADER}\n[[package]]\nname = \"cc\"\n\n[[package]]\nname = \"syn\"\nversion = 2\n\n\
             [[package]]\nversion = \"1.0.0\"\n\n[[package]]\nname = \"odd\"\nversion = \"1.x\"\n\n\
             [[package]]\nname = \"zip\"\nversion = \"7.2.0\"\n"
        );
        assert_eq!(Lock::parse(&text).unwrap().packages(), [("zip", "7.2.0")]);
    }

    #[test]
    fn a_lockfile_that_is_not_toml_is_an_error() {
        assert!(Lock::parse("[[package]\nname = \"cc\"\n").is_err());
        assert!(Lock::parse("<<<<<<< HEAD\nversion = 4\n=======\n").is_err());
    }

    #[test]
    fn compatible_versions_follow_cargo() {
        let of = |text: &str| Compatible::of(&Version::parse(text).unwrap());
        assert_eq!(of("1.16.4"), of("1.17.0"));
        assert_ne!(of("1.16.4"), of("2.0.0"));
        assert_eq!(of("0.3.1"), of("0.3.4"));
        assert_ne!(of("0.3.1"), of("0.4.0"));
        assert_ne!(of("0.3.1"), of("1.3.1"));
        assert_eq!(of("0.0.3"), of("0.0.3-alpha"));
        assert_ne!(of("0.0.3"), of("0.0.4"));
        assert_ne!(of("0.0.3"), of("0.3.0"));
        assert_eq!(of("0.18.0-alpha.2"), of("0.18.1"));
    }

    #[test]
    fn locks_on_the_same_versions_do_not_drift() {
        let root = lock(&[("cc", "1.10.0"), ("syn", "2.0.119"), ("syn", "3.0.9")]);
        let fuzz = lock(&[("arbitrary", "1.5.0"), ("cc", "1.10.0"), ("syn", "3.0.9")]);
        assert_eq!(drift(&root, &fuzz), []);
    }

    #[test]
    fn a_shared_crate_on_another_version_drifts() {
        let root = lock(&[
            ("smallvec", "1.16.6"),
            ("syn", "2.0.119"),
            ("syn", "3.0.10"),
            ("zmij", "0.0.4"),
        ]);
        let fuzz = lock(&[
            ("arbitrary", "1.5.0"),
            ("smallvec", "1.16.4"),
            ("syn", "3.0.9"),
            ("zmij", "0.0.4"),
        ]);
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [
                version("smallvec", "1.16.4", "1.16.6"),
                version("syn", "3.0.9", "3.0.10"),
            ]
        );
        assert!(found.iter().all(Drift::rebuild_fixes));
        assert_eq!(
            found[1].to_string(),
            "fuzz/Cargo.lock: syn 3.0.9, Cargo.lock has 3.0.10"
        );
    }

    #[test]
    fn another_major_version_of_a_shared_crate_is_not_drift() {
        // A crate only the fuzz crate uses can need getrandom 0.3 while the
        // workspace locks 0.4: cargo keeps both.
        let root = lock(&[("getrandom", "0.4.3"), ("cc", "1.10.0")]);
        let fuzz = lock(&[
            ("getrandom", "0.3.4"),
            ("getrandom", "0.4.3"),
            ("jobserver", "0.1.34"),
            ("cc", "1.10.0"),
        ]);
        assert_eq!(drift(&root, &fuzz), []);
        let merged_lock = Lock::parse(&merged(&root, &fuzz)).unwrap();
        assert_eq!(
            merged_lock.packages(),
            [
                ("getrandom", "0.4.3"),
                ("cc", "1.10.0"),
                ("getrandom", "0.3.4"),
                ("jobserver", "0.1.34"),
            ]
        );
    }

    #[test]
    fn a_crate_the_root_patches_drifts_by_source() {
        // `[patch.crates-io] smallvec = { path = ... }` in Cargo.toml: the
        // workspace takes smallvec from a path, the fuzz crate from crates.io.
        let root = lock_from(&[
            ("smallvec", "1.16.9", None),
            ("cc", "1.10.0", Some(CRATES_IO)),
        ]);
        let fuzz = lock(&[("smallvec", "1.16.4"), ("cc", "1.10.0")]);
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [Drift::Source {
                name: "smallvec".into(),
                fuzz: "1.16.4".into(),
                fuzz_source: "crates.io".into(),
                root: "1.16.9".into(),
                root_source: "a path".into(),
            }]
        );
        assert!(!found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.4 from crates.io, Cargo.lock takes smallvec 1.16.9 \
             from a path; give fuzz/Cargo.toml the `[patch]` that Cargo.lock follows"
        );
        // The rebuild keeps the fuzz crate's own smallvec, not a newer one.
        let merged_lock = Lock::parse(&merged(&root, &fuzz)).unwrap();
        assert_eq!(
            merged_lock.packages(),
            [
                ("smallvec", "1.16.9"),
                ("cc", "1.10.0"),
                ("smallvec", "1.16.4"),
            ]
        );
    }

    #[test]
    fn sources_show_crates_io_paths_and_others() {
        assert_eq!(shown_source(None), "a path");
        assert_eq!(shown_source(Some(CRATES_IO)), "crates.io");
        let git = "git+https://example.com/smallvec#0123abc";
        assert_eq!(shown_source(Some(git)), git);
    }

    #[test]
    fn merging_keeps_the_root_and_adds_crates_only_the_fuzz_lock_has() {
        let root = lock(&[("cc", "1.11.3"), ("xtask", "0.0.0")]);
        let fuzz = lock(&[
            ("arbitrary", "1.5.0"),
            ("cc", "1.10.0"),
            ("mora-fuzz", "0.0.0"),
        ]);
        let merged_lock = Lock::parse(&merged(&root, &fuzz)).unwrap();
        assert_eq!(
            merged_lock.packages(),
            [
                ("cc", "1.11.3"),
                ("xtask", "0.0.0"),
                ("arbitrary", "1.5.0"),
                ("mora-fuzz", "0.0.0"),
            ]
        );
        assert_eq!(merged_lock.0.get("version"), Some(&Value::Integer(4)));
        let sources = merged_lock
            .entries()
            .iter()
            .filter(|entry| entry.get("source").and_then(Value::as_str) == Some(CRATES_IO));
        assert_eq!(sources.count(), 4, "every entry keeps its source");
        let alone = Lock::parse(&merged(&root, &Lock::default())).unwrap();
        assert_eq!(alone.packages(), root.packages());
    }

    #[test]
    fn merging_keeps_fuzz_entries_it_cannot_read() {
        let root = lock(&[("cc", "1.11.3")]);
        let text = format!("{HEADER}\n[[package]]\nname = \"odd\"\nversion = \"1.x\"\n");
        let merged_text = merged(&root, &Lock::parse(&text).unwrap());
        assert!(merged_text.contains("name = \"odd\""), "{merged_text}");
    }
}
