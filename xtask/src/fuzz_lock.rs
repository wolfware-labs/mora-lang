//! `cargo xtask fuzz-lock`: keeps the fuzz crate on the code the workspace
//! builds (spec §20.1).
//!
//! The fuzz crate sits outside the workspace and has a lockfile of its own.
//! The workspace and the fuzz crate must build the same version from the
//! same source of every crate they share, `mora-syntax` and what that
//! depends on, so that fuzzing runs the code the tests run. `--check` asks
//! cargo what each build resolves and fails when a shared crate differs; the
//! command itself rebuilds `fuzz/Cargo.lock` from `Cargo.lock`.
//!
//! Two packages are the same crate when cargo would let only one of them in
//! a resolve: the same name and versions that are semver compatible. A crate
//! only the fuzz crate uses can then take another major version of a crate
//! the workspace builds without counting as a difference.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use cargo_metadata::semver::Version;
use toml::{Table, Value};

use crate::paths::{FUZZ_LOCK, FUZZ_MANIFEST, ROOT_MANIFEST, error_at};

/// The key of a lockfile's array of package entries.
const PACKAGE: &str = "package";

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
    /// The crate's name.
    name: &'a str,
    /// The versions its version is compatible with.
    compatible: Compatible,
}

impl<'a> Locked<'a> {
    fn read(entry: &'a Value) -> Option<Self> {
        let version = entry.get("version")?.as_str()?;
        Some(Self {
            name: entry.get("name")?.as_str()?,
            compatible: Compatible::of(&Version::parse(version).ok()?),
        })
    }

    /// The same name and semver-compatible versions, from any source.
    fn same_crate(&self, other: &Locked<'_>) -> bool {
        self.name == other.name && self.compatible == other.compatible
    }
}

/// A package one build resolves, as `cargo metadata` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Whether cargo would keep only one of the two in a resolve.
    fn same_crate(&self, other: &Built) -> bool {
        self.name == other.name && Compatible::of(&self.version) == Compatible::of(&other.version)
    }
}

/// A crate the fuzz crate builds differently from the workspace: what the
/// workspace builds and what the fuzz crate builds.
#[derive(Debug, PartialEq, Eq)]
pub struct Drift {
    pub root: Built,
    pub fuzz: Built,
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { root, fuzz } = self;
        write!(
            f,
            "{}: {ROOT_MANIFEST} builds {} from {}, {FUZZ_MANIFEST} builds {} from {}",
            fuzz.name, root.version, root.from, fuzz.version, fuzz.from
        )
    }
}

/// The crates `fuzz` builds differently from `root`, each the resolve of
/// one build: a package of `fuzz` whose crate `root` builds at another
/// version or from another source. Crates only one build resolves are left
/// out.
pub fn drift(root: &[Built], fuzz: &[Built]) -> Vec<Drift> {
    fuzz.iter()
        .filter(|package| !root.contains(package))
        .filter_map(|package| {
            let other = root.iter().find(|other| package.same_crate(other))?;
            Some(Drift {
                root: other.clone(),
                fuzz: package.clone(),
            })
        })
        .collect()
}

/// `root`, with the entries of `fuzz` that `root` does not replace.
///
/// Cargo then drops what the fuzz crate does not use and adds what it lacks,
/// keeps every shared crate at the version `root` locks, and writes the file
/// in its own layout. `root` replaces each entry of the same crate, whatever
/// its source, so that cargo takes a crate the way the fuzz crate's
/// manifest and config resolve it, as for the workspace. An entry `root`
/// does not replace (another major version, or a version that does not
/// parse) is kept as the fuzz lockfile has it.
pub fn merged(root: &Lock, fuzz: &Lock) -> String {
    let shared: Vec<Locked<'_>> = root.locked().collect();
    let replaced = |package: Locked<'_>| shared.iter().any(|other| package.same_crate(other));
    let kept = fuzz
        .entries()
        .iter()
        .filter(|entry| Locked::read(entry).is_none_or(|package| !replaced(package)));
    let mut packages = root.entries().to_vec();
    packages.extend(kept.cloned());
    let mut table = root.0.clone();
    table.insert(PACKAGE.to_owned(), Value::Array(packages));
    table.to_string()
}

/// Whether a cargo error is about a requirement no version satisfies,
/// which neither lockfile can change.
fn names_requirement(error: &str) -> bool {
    error.contains("failed to select a version for") || error.contains("no matching package")
}

/// The message for a cargo `error` about a requirement no version
/// satisfies, which [`names_requirement`] finds.
fn unsatisfiable(error: &str) -> String {
    format!("cargo cannot resolve {FUZZ_MANIFEST}: {error}; fix the requirement it names")
}

/// A problem with `fuzz/Cargo.lock`.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// Cargo cannot resolve the fuzz crate with `--locked`: the fuzz
    /// lockfile needs a change, or a requirement no version satisfies; its
    /// error.
    Unlocked(String),
    /// A crate the fuzz crate builds differently from the workspace.
    Drift(Drift),
}

impl Problem {
    /// Whether `cargo xtask fuzz-lock` may remove it. A crate built
    /// differently is fixed by the rebuild when only the fuzz lockfile holds
    /// it apart, which shows once the rebuild has run.
    fn rebuild_may_fix(&self) -> bool {
        match self {
            Self::Unlocked(error) => !names_requirement(error),
            Self::Drift(_) => true,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unlocked(error) if names_requirement(error) => f.write_str(&unsatisfiable(error)),
            Self::Unlocked(error) => {
                write!(f, "{FUZZ_LOCK} does not match {FUZZ_MANIFEST}: {error}")
            }
            Self::Drift(drift) => drift.fmt(f),
        }
    }
}

/// What cargo resolves for the fuzz crate, checked against the workspace.
#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    /// Each problem with the fuzz lockfile.
    pub problems: Vec<Problem>,
    /// The number of packages the fuzz crate resolves.
    pub packages: usize,
}

/// Each problem with the fuzz lockfile: `root` is what the workspace
/// resolves, and `fuzz` what the fuzz crate resolves with `--locked`, or
/// cargo's error when it cannot.
pub fn problems(root: &[Built], fuzz: Result<&[Built], String>) -> Vec<Problem> {
    match fuzz {
        Ok(fuzz) => drift(root, fuzz).into_iter().map(Problem::Drift).collect(),
        Err(error) => vec![Problem::Unlocked(error.trim_end().to_owned())],
    }
}

/// The line `--check` ends with when it finds problems; `None` when there
/// are none and the check passes.
pub fn check_summary(problems: &[Problem]) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let mut summary = format!("fuzz-lock: {} problem(s)", problems.len());
    if problems.iter().any(Problem::rebuild_may_fix) {
        summary += "; run `cargo xtask fuzz-lock`, which says what it cannot fix";
    }
    Some(summary)
}

/// The line the rebuild ends with when problems remain after it; `None`
/// when there are none.
pub fn rebuild_summary(problems: &[Problem]) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let mut summary = format!(
        "fuzz-lock: {} problem(s) the rebuild cannot fix",
        problems.len()
    );
    // Cargo resolves the crate this way from the rebuilt lockfile, so what
    // holds it apart is in what each build reads besides it.
    if problems
        .iter()
        .any(|problem| matches!(problem, Problem::Drift(_)))
    {
        summary += &format!(
            "; {ROOT_MANIFEST} and {FUZZ_MANIFEST}, with the cargo config each build reads \
             (`.cargo/config.toml` above the folder it runs in), resolve each crate named above \
             differently: make them resolve it the same way"
        );
    }
    summary += &format!("; {FUZZ_LOCK} is left as it was");
    Some(summary)
}

/// What the rebuild says it did: whether it `changed` the fuzz lockfile.
pub fn rebuilt(changed: bool) -> String {
    if changed {
        format!("fuzz-lock: wrote {FUZZ_LOCK}")
    } else {
        format!("fuzz-lock: {FUZZ_LOCK} is up to date")
    }
}

/// What a passing run prints, for a fuzz crate that resolves `packages`
/// packages.
pub fn ok_summary(packages: usize) -> String {
    format!("fuzz-lock: {packages} packages, shared crates built as the workspace builds them, ok")
}

/// Why the rebuild failed, from cargo's error when it resolved the rebuilt
/// fuzz lockfile; the rebuild has put the file back as it was.
pub fn resolve_failure(error: &str) -> String {
    let error = error.trim_end();
    let reason = if names_requirement(error) {
        unsatisfiable(error)
    } else {
        error.to_owned()
    };
    format!("{reason}; {FUZZ_LOCK} is left as it was")
}

/// Writes `text`, the rebuilt fuzz lockfile, to `file` and has cargo
/// `resolve` and check it, which may rewrite it; returns whether the file
/// changed, and the check. When cargo fails, or the check finds problems,
/// the file is put back byte for byte, or removed when there was none, so
/// that a rebuild that cannot settle every crate moves none.
pub fn write_resolved(
    file: &Path,
    text: &str,
    resolve: impl FnOnce() -> Result<Checked, String>,
) -> Result<(bool, Checked), String> {
    let shown = || error_at(Path::new(FUZZ_LOCK));
    let before = match fs::read(file) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(shown()(err)),
    };
    let restore = || {
        match &before {
            Some(bytes) => fs::write(file, bytes),
            None => fs::remove_file(file),
        }
        .map_err(shown())
    };
    fs::write(file, text).map_err(shown())?;
    let checked = match resolve() {
        Ok(checked) => checked,
        Err(error) => {
            restore()?;
            return Err(resolve_failure(&error));
        }
    };
    if !checked.problems.is_empty() {
        restore()?;
        return Ok((false, checked));
    }
    let after = fs::read(file).map_err(shown())?;
    Ok((before.as_deref() != Some(after.as_slice()), checked))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::*;

    const HEADER: &str = "# This file is automatically @generated by Cargo.\nversion = 4\n";

    /// The source cargo writes for crates.io.
    const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

    /// A vendored smallvec folder.
    const VENDOR: &str = "/repo/vendor/smallvec";

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

    /// The name and version of each entry of a lockfile, in its order.
    fn packages(lock: &Lock) -> Vec<(&str, &str)> {
        lock.entries()
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.get("name")?.as_str()?,
                    entry.get("version")?.as_str()?,
                ))
            })
            .collect()
    }

    /// The lockfile [`merged`] writes, parsed.
    fn parse_merged(root: &Lock, fuzz: &Lock) -> Lock {
        Lock::parse(&merged(root, fuzz)).unwrap()
    }

    /// A package built from `from`.
    fn built(name: &str, version: &str, from: &str) -> Built {
        Built {
            name: name.into(),
            version: Version::parse(version).unwrap(),
            from: from.into(),
        }
    }

    /// A package built from crates.io.
    fn registry(name: &str, version: &str) -> Built {
        built(name, version, CRATES_IO)
    }

    fn drift_of(root: Built, fuzz: Built) -> Drift {
        Drift { root, fuzz }
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
    fn builds_of_the_same_packages_do_not_drift() {
        let syntax = built("mora-syntax", "0.0.0", "/repo/crates/mora-syntax");
        let root = [
            registry("cc", "1.10.0"),
            syntax.clone(),
            registry("syn", "2.0.119"),
            registry("syn", "3.0.9"),
            built("xtask", "0.0.0", "/repo/xtask"),
        ];
        let fuzz = [
            registry("arbitrary", "1.5.0"),
            registry("cc", "1.10.0"),
            built("mora-fuzz", "0.0.0", "/repo/fuzz"),
            syntax,
            registry("syn", "3.0.9"),
        ];
        assert_eq!(drift(&root, &fuzz), []);
        assert_eq!(drift(&root, &[]), []);
        assert_eq!(drift(&[], &fuzz), []);
    }

    #[test]
    fn a_shared_crate_at_another_version_drifts() {
        let root = [
            registry("smallvec", "1.16.6"),
            registry("syn", "2.0.119"),
            registry("syn", "3.0.10"),
            registry("zmij", "0.0.4"),
        ];
        let fuzz = [
            registry("arbitrary", "1.5.0"),
            registry("smallvec", "1.16.4"),
            registry("syn", "3.0.9"),
            registry("zmij", "0.0.4"),
        ];
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [
                drift_of(
                    registry("smallvec", "1.16.6"),
                    registry("smallvec", "1.16.4")
                ),
                drift_of(registry("syn", "3.0.10"), registry("syn", "3.0.9")),
            ]
        );
        assert_eq!(
            found[1].to_string(),
            format!(
                "syn: Cargo.toml builds 3.0.10 from {CRATES_IO}, fuzz/Cargo.toml builds 3.0.9 \
                 from {CRATES_IO}"
            )
        );
    }

    #[test]
    fn another_major_version_of_a_shared_crate_is_not_drift() {
        // A crate only the fuzz crate uses can need getrandom 0.3 while the
        // workspace builds 0.4: cargo keeps both.
        let root = [registry("getrandom", "0.4.3"), registry("cc", "1.10.0")];
        let fuzz = [
            registry("cc", "1.10.0"),
            registry("getrandom", "0.3.4"),
            registry("getrandom", "0.4.3"),
            registry("jobserver", "0.1.34"),
        ];
        assert_eq!(drift(&root, &fuzz), []);
    }

    #[test]
    fn a_shared_crate_from_another_source_drifts() {
        // The workspace patches smallvec to a folder, the fuzz crate takes
        // it from crates.io.
        let root = [
            built("smallvec", "1.16.9", VENDOR),
            registry("cc", "1.10.0"),
        ];
        let fuzz = [registry("cc", "1.10.0"), registry("smallvec", "1.16.4")];
        let found = drift(&root, &fuzz);
        assert_eq!(
            found,
            [drift_of(
                built("smallvec", "1.16.9", VENDOR),
                registry("smallvec", "1.16.4")
            )]
        );
        assert_eq!(
            found[0].to_string(),
            format!(
                "smallvec: Cargo.toml builds 1.16.9 from {VENDOR}, fuzz/Cargo.toml builds 1.16.4 \
                 from {CRATES_IO}"
            )
        );
        // The other way round, and a git source.
        let git = "git+https://example.com/smallvec?branch=main#0123abc";
        let found = drift(
            &[registry("smallvec", "1.16.4")],
            &[built("smallvec", "1.16.4", git)],
        );
        assert_eq!(
            found,
            [drift_of(
                registry("smallvec", "1.16.4"),
                built("smallvec", "1.16.4", git)
            )]
        );
    }

    #[test]
    fn the_same_folder_on_both_sides_does_not_drift() {
        let root = [built("smallvec", "1.16.9", VENDOR)];
        assert_eq!(drift(&root, &[built("smallvec", "1.16.9", VENDOR)]), []);
    }

    #[test]
    fn two_folders_at_one_version_drift() {
        // Both vendored copies are smallvec 1.16.4, but they are two crates.
        let other = "/repo/vendor/smallvec2";
        let found = drift(
            &[built("smallvec", "1.16.4", VENDOR)],
            &[built("smallvec", "1.16.4", other)],
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
    fn a_workspace_crate_from_another_folder_drifts() {
        let found = drift(
            &[built("mora-syntax", "0.0.0", "/repo/crates/mora-syntax")],
            &[built("mora-syntax", "0.0.0", "/elsewhere/mora-syntax")],
        );
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn problems_are_cargo_s_error_or_each_drift() {
        let root = [registry("cc", "1.11.3"), registry("syn", "2.0.119")];
        let fuzz = [registry("cc", "1.10.0"), registry("syn", "2.0.119")];
        assert_eq!(problems(&root, Ok(&[registry("cc", "1.11.3")])), []);
        assert_eq!(
            problems(&root, Ok(&fuzz)),
            [Problem::Drift(drift_of(
                registry("cc", "1.11.3"),
                registry("cc", "1.10.0")
            ))]
        );
        assert_eq!(
            problems(&root, Err(format!("{STALE}\n\n"))),
            [Problem::Unlocked(STALE.into())]
        );
    }

    #[test]
    fn merging_replaces_every_entry_of_a_crate_the_root_locks() {
        // The fuzz crate takes smallvec from crates.io and the workspace from
        // a path, or the other way round: the root's entry goes in, and
        // cargo takes smallvec the way the fuzz manifest resolves it.
        let root = lock_from(&[
            ("smallvec", "1.16.9", None),
            ("cc", "1.10.0", Some(CRATES_IO)),
        ]);
        let fuzz = lock(&[("smallvec", "1.16.4"), ("cc", "1.10.0")]);
        assert_eq!(
            packages(&parse_merged(&root, &fuzz)),
            [("smallvec", "1.16.9"), ("cc", "1.10.0")]
        );
        assert_eq!(
            packages(&parse_merged(&fuzz, &root)),
            [("smallvec", "1.16.4"), ("cc", "1.10.0")]
        );
    }

    #[test]
    fn merging_keeps_another_major_version() {
        let root = lock(&[("getrandom", "0.4.3"), ("cc", "1.10.0")]);
        let fuzz = lock(&[
            ("getrandom", "0.3.4"),
            ("getrandom", "0.4.2"),
            ("jobserver", "0.1.34"),
            ("cc", "1.10.0"),
        ]);
        assert_eq!(
            packages(&parse_merged(&root, &fuzz)),
            [
                ("getrandom", "0.4.3"),
                ("cc", "1.10.0"),
                ("getrandom", "0.3.4"),
                ("jobserver", "0.1.34"),
            ]
        );
    }

    #[test]
    fn merging_keeps_the_root_and_adds_crates_only_the_fuzz_lock_has() {
        let root = lock(&[("cc", "1.11.3"), ("xtask", "0.0.0")]);
        let fuzz = lock(&[
            ("arbitrary", "1.5.0"),
            ("cc", "1.10.0"),
            ("mora-fuzz", "0.0.0"),
        ]);
        let merged_lock = parse_merged(&root, &fuzz);
        assert_eq!(
            packages(&merged_lock),
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
        let alone = parse_merged(&root, &Lock::default());
        assert_eq!(packages(&alone), packages(&root));
    }

    #[test]
    fn merging_keeps_fuzz_entries_it_cannot_read() {
        let root = lock(&[("cc", "1.11.3")]);
        let text = format!(
            "{HEADER}\n[[package]]\nname = \"cc\"\n\n[[package]]\nname = \"odd\"\n\
             version = \"1.x\"\n\n[[package]]\nversion = \"1.0.0\"\n"
        );
        let merged_lock = parse_merged(&root, &Lock::parse(&text).unwrap());
        assert_eq!(merged_lock.entries().len(), 4);
        assert_eq!(packages(&merged_lock), [("cc", "1.11.3"), ("odd", "1.x")]);
    }

    /// A check that finds no problem.
    fn clean() -> Checked {
        Checked {
            problems: Vec::new(),
            packages: 81,
        }
    }

    /// A check that still finds smallvec built from two sources.
    fn apart() -> Checked {
        Checked {
            problems: vec![Problem::Drift(drift_of(
                built("smallvec", "1.16.4", VENDOR),
                registry("smallvec", "1.16.6"),
            ))],
            packages: 81,
        }
    }

    #[test]
    fn a_resolved_rebuild_reports_whether_it_changed_the_file() {
        let dir = crate::paths::scratch("fuzz-lock-resolved");
        let file = dir.join("Cargo.lock");
        assert_eq!(
            write_resolved(&file, "a", || Ok(clean())),
            Ok((true, clean()))
        );
        assert_eq!(
            write_resolved(&file, "a", || Ok(clean())),
            Ok((false, clean()))
        );
        // Cargo rewriting the file to what it was counts as no change.
        let rewrite = || {
            fs::write(&file, "a").map_err(|err| err.to_string())?;
            Ok(clean())
        };
        assert_eq!(write_resolved(&file, "b", rewrite), Ok((false, clean())));
        assert_eq!(fs::read_to_string(&file).unwrap(), "a");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_rebuild_restores_the_previous_bytes() {
        let dir = crate::paths::scratch("fuzz-lock-restore");
        let file = dir.join("Cargo.lock");
        for before in [b"version = 4\n".as_slice(), b"\xff\xfeversion = 4\n"] {
            fs::write(&file, before).unwrap();
            let failed = write_resolved(&file, "rebuilt", || {
                assert_eq!(fs::read_to_string(&file).unwrap(), "rebuilt");
                Err("error: failed to download\n".into())
            });
            assert_eq!(
                failed,
                Err("error: failed to download; fuzz/Cargo.lock is left as it was".into())
            );
            assert_eq!(fs::read(&file).unwrap(), before);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_rebuild_that_leaves_a_crate_apart_restores_the_previous_bytes() {
        let dir = crate::paths::scratch("fuzz-lock-apart");
        let file = dir.join("Cargo.lock");
        let before = b"version = 4\n# as it was\n";
        fs::write(&file, before).unwrap();
        // Cargo resolves the rebuilt file and moves a version, but the
        // check still finds smallvec built from two sources.
        let resolve = || {
            fs::write(&file, "rebuilt by cargo").map_err(|err| err.to_string())?;
            Ok(apart())
        };
        assert_eq!(
            write_resolved(&file, "rebuilt", resolve),
            Ok((false, apart()))
        );
        assert_eq!(fs::read(&file).unwrap(), before);
        // With no file before, none is left.
        fs::remove_file(&file).unwrap();
        assert_eq!(
            write_resolved(&file, "rebuilt", || Ok(apart())),
            Ok((false, apart()))
        );
        assert!(!file.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_rebuild_removes_a_file_there_was_not() {
        let dir = crate::paths::scratch("fuzz-lock-remove");
        let file = dir.join("Cargo.lock");
        let failed = write_resolved(&file, "rebuilt", || Err(UNSATISFIED.into()));
        assert_eq!(failed, Err(resolve_failure(UNSATISFIED)));
        assert!(!file.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_lockfile_that_cannot_be_read_is_not_taken_for_none() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::paths::scratch("fuzz-lock-unreadable");
        let file = dir.join("Cargo.lock");
        fs::write(&file, "version = 4\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o200)).unwrap();
        let failed = write_resolved(&file, "rebuilt", || Err("error: offline".into()));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let err = failed.unwrap_err();
        assert!(err.starts_with("fuzz/Cargo.lock: "), "{err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "version = 4\n");
        let _ = fs::remove_dir_all(dir);
    }

    const STALE: &str = "error: cannot update the lock file fuzz/Cargo.lock because --locked \
                         was passed to prevent this";
    const UNSATISFIED: &str = "error: failed to select a version for the requirement \
                               `memchr = \"^9\"`\ncandidate versions found which didn't match";

    #[test]
    fn problems_say_what_cargo_said() {
        let stale = Problem::Unlocked(STALE.into());
        assert!(stale.rebuild_may_fix());
        assert_eq!(
            stale.to_string(),
            format!("fuzz/Cargo.lock does not match fuzz/Cargo.toml: {STALE}")
        );
        let unsatisfied = Problem::Unlocked(UNSATISFIED.into());
        assert!(!unsatisfied.rebuild_may_fix());
        assert_eq!(
            unsatisfied.to_string(),
            format!(
                "cargo cannot resolve fuzz/Cargo.toml: {UNSATISFIED}; fix the requirement it names"
            )
        );
        let missing = Problem::Unlocked("error: no matching package named `nope` found".into());
        assert!(!missing.rebuild_may_fix());
        let found = drift_of(registry("cc", "1.11.3"), registry("cc", "1.10.0"));
        let shown = found.to_string();
        let drift = Problem::Drift(found);
        assert!(drift.rebuild_may_fix());
        assert_eq!(drift.to_string(), shown);
    }

    #[test]
    fn the_check_says_when_the_rebuild_may_help() {
        assert_eq!(check_summary(&[]), None);
        let unsatisfied = Problem::Unlocked(UNSATISFIED.into());
        assert_eq!(
            check_summary(&[unsatisfied]).unwrap(),
            "fuzz-lock: 1 problem(s)"
        );
        let drift = Problem::Drift(drift_of(registry("cc", "1.11.3"), registry("cc", "1.10.0")));
        assert_eq!(
            check_summary(&[Problem::Unlocked(UNSATISFIED.into()), drift]).unwrap(),
            "fuzz-lock: 2 problem(s); run `cargo xtask fuzz-lock`, which says what it cannot fix"
        );
        assert_eq!(
            check_summary(&[Problem::Unlocked(STALE.into())]).unwrap(),
            "fuzz-lock: 1 problem(s); run `cargo xtask fuzz-lock`, which says what it cannot fix"
        );
    }

    #[test]
    fn the_rebuild_points_at_the_manifests_for_a_crate_left_apart() {
        assert_eq!(rebuild_summary(&[]), None);
        assert_eq!(
            rebuild_summary(&[Problem::Unlocked(STALE.into())]).unwrap(),
            "fuzz-lock: 1 problem(s) the rebuild cannot fix; fuzz/Cargo.lock is left as it was"
        );
        let drift = Problem::Drift(drift_of(
            built("smallvec", "1.16.9", VENDOR),
            registry("smallvec", "1.16.4"),
        ));
        assert_eq!(
            rebuild_summary(&[drift]).unwrap(),
            "fuzz-lock: 1 problem(s) the rebuild cannot fix; Cargo.toml and fuzz/Cargo.toml, \
             with the cargo config each build reads (`.cargo/config.toml` above the folder it \
             runs in), resolve each crate named above differently: make them resolve it the same \
             way; fuzz/Cargo.lock is left as it was"
        );
    }

    #[test]
    fn the_rebuild_says_whether_it_changed_the_file() {
        assert_eq!(rebuilt(false), "fuzz-lock: fuzz/Cargo.lock is up to date");
        assert_eq!(rebuilt(true), "fuzz-lock: wrote fuzz/Cargo.lock");
        assert_eq!(
            ok_summary(81),
            "fuzz-lock: 81 packages, shared crates built as the workspace builds them, ok"
        );
    }

    #[test]
    fn a_failed_rebuild_names_a_requirement_only_when_cargo_does() {
        assert_eq!(
            resolve_failure(UNSATISFIED),
            format!(
                "cargo cannot resolve fuzz/Cargo.toml: {UNSATISFIED}; fix the requirement it \
                 names; fuzz/Cargo.lock is left as it was"
            )
        );
        let offline = "error: failed to download from `https://index.crates.io/`";
        // Cargo's error ends with a newline, which the message leaves out.
        assert_eq!(
            resolve_failure(&format!("{offline}\n")),
            format!("{offline}; fuzz/Cargo.lock is left as it was")
        );
    }
}
