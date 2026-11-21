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
//!
//! A `[patch]` in `Cargo.toml` does not reach the fuzz crate, so a crate it
//! patches is locked from another source in the two files until
//! `fuzz/Cargo.toml` has the same patch. A patch in the repository's cargo
//! config counts for both builds, and cargo takes it over a manifest's
//! patch of the same crate. When both builds patch a crate, the
//! rebuild drops the fuzz lockfile's entry and cargo takes the patch, and
//! the check fails when the two patches point at different crates.

mod patches;

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use cargo_metadata::semver::Version;
use toml::{Table, Value};

use self::patches::Patched;
pub use self::patches::{Patch, Patches, patches_in};
use crate::paths::{FUZZ_LOCK, FUZZ_MANIFEST, ROOT_LOCK, ROOT_MANIFEST, error_at};

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
    /// The crate's name.
    name: &'a str,
    /// The version, as the lockfile writes it.
    version: &'a str,
    /// Where the crate comes from; `None` for a path dependency.
    source: Option<&'a str>,
    /// The versions `version` is compatible with.
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
    /// moves to the root's unless a requirement of the fuzz crate holds it,
    /// only the fuzz crate patches it, or the two builds patch it to
    /// different crates.
    Version {
        name: String,
        fuzz: String,
        root: String,
        patched: Patched,
    },
    /// A compatible version from another source than the one `Cargo.lock`
    /// takes it from.
    Source {
        name: String,
        fuzz: String,
        fuzz_source: String,
        root: String,
        root_source: String,
        patched: Patched,
    },
}

impl Drift {
    /// Whether rebuilding the fuzz lockfile from `Cargo.lock` can remove it.
    pub fn rebuild_fixes(&self) -> bool {
        match self {
            Self::Version { patched, .. } => !matches!(patched, Patched::Apart(_) | Patched::Fuzz),
            // Cargo keeps the version the rebuild takes from `Cargo.lock`
            // over a patch only the fuzz crate has.
            Self::Source { patched, .. } => matches!(patched, Patched::Same | Patched::Fuzz),
        }
    }

    /// Whether it is a version the rebuild moves to the root's unless a
    /// requirement of the fuzz crate holds it apart.
    fn may_be_held(&self) -> bool {
        matches!(self, Self::Version { .. }) && self.rebuild_fixes()
    }
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, patched) = match self {
            Self::Version {
                name,
                fuzz,
                root,
                patched,
            } => {
                write!(f, "{FUZZ_LOCK}: {name} {fuzz}, {ROOT_LOCK} has {root}")?;
                (name, patched)
            }
            Self::Source {
                name,
                fuzz,
                fuzz_source,
                root,
                root_source,
                patched,
            } => {
                write!(
                    f,
                    "{FUZZ_LOCK}: {name} {fuzz} from {fuzz_source}, {ROOT_LOCK} takes {name} \
                     {root} from {root_source}"
                )?;
                (name, patched)
            }
        };
        let source = matches!(self, Self::Source { .. });
        match patched {
            // The problem for the two patches says what they are.
            Patched::Apart(_) => write!(f, "; the two builds patch {name} to different crates"),
            Patched::Fuzz if source => write!(
                f,
                "; {FUZZ_MANIFEST} patches {name} and {ROOT_MANIFEST} does not: remove the patch \
                 from {FUZZ_MANIFEST}, or add it to {ROOT_MANIFEST}; until then `cargo xtask \
                 fuzz-lock` takes the version {ROOT_LOCK} has and leaves the patch unused"
            ),
            Patched::Fuzz => write!(
                f,
                "; {FUZZ_MANIFEST} patches {name} and {ROOT_MANIFEST} does not: remove the patch \
                 from {FUZZ_MANIFEST}, or add it to {ROOT_MANIFEST}"
            ),
            Patched::Same if source => write!(
                f,
                "; both builds patch {name} the same way: run `cargo xtask fuzz-lock`"
            ),
            Patched::Root if source => write!(
                f,
                "; {ROOT_MANIFEST} patches {name}: give {FUZZ_MANIFEST} the same `[patch]`, \
                 then run `cargo xtask fuzz-lock`"
            ),
            Patched::Neither if source => write!(
                f,
                "; neither build patches {name} from the repository: check whether the \
                 workspace has a crate of that name, which is another crate than the fuzz \
                 crate's, or a cargo config outside the repository patches it"
            ),
            Patched::Same | Patched::Root | Patched::Neither => Ok(()),
        }
    }
}

/// The crates `fuzz` locks differently from `root`: at another version of the
/// same crate, or from another source than a compatible version in `root`.
pub fn drift(root: &Lock, fuzz: &Lock, patches: &Patches) -> Vec<Drift> {
    let root: Vec<Locked<'_>> = root.locked().collect();
    fuzz.locked()
        .filter_map(|package| {
            if let Some(same) = root.iter().find(|other| package.same_crate(other)) {
                return (same.version != package.version).then(|| Drift::Version {
                    name: package.name.to_owned(),
                    fuzz: package.version.to_owned(),
                    root: same.version.to_owned(),
                    patched: patches.of(package.name),
                });
            }
            let other = root.iter().find(|other| package.compatible_with(other))?;
            Some(Drift::Source {
                name: package.name.to_owned(),
                fuzz: package.version.to_owned(),
                fuzz_source: shown_source(package.source).to_owned(),
                root: other.version.to_owned(),
                root_source: shown_source(other.source).to_owned(),
                patched: patches.of(package.name),
            })
        })
        .collect()
}

/// `root`, with the entries of `fuzz` that `root` does not replace.
///
/// Cargo then drops what the fuzz crate does not use and adds what it lacks,
/// keeps every shared crate at the version `root` locks, and writes the file
/// in its own layout. `root` replaces an entry of the same crate, and one of
/// a compatible version from another source when both builds patch that
/// crate, so that cargo takes the fuzz crate's patch instead of the version
/// locked before it. An entry `root` does not replace (another major
/// version, another source no patch of both builds explains, or a version
/// that does not parse) is kept as the fuzz lockfile has it.
pub fn merged(root: &Lock, fuzz: &Lock, patches: &Patches) -> String {
    let shared: Vec<Locked<'_>> = root.locked().collect();
    let replaced = |package: Locked<'_>| {
        shared.iter().any(|other| {
            package.same_crate(other)
                || (package.compatible_with(other) && patches.of(package.name).in_both())
        })
    };
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
    /// The fuzz lockfile cannot be read; the reason.
    Unreadable(String),
    /// A crate the fuzz lockfile locks differently from `Cargo.lock`.
    Drift(Drift),
    /// A crate both builds patch, each to another crate: the workspace's
    /// patch and the fuzz crate's.
    PatchesApart(Box<(Patch, Patch)>),
}

impl Problem {
    /// Whether `cargo xtask fuzz-lock` can remove it.
    pub fn rebuild_fixes(&self) -> bool {
        match self {
            Self::Unlocked(error) => !names_requirement(error),
            Self::Unreadable(_) => true,
            Self::Drift(drift) => drift.rebuild_fixes(),
            Self::PatchesApart(_) => false,
        }
    }

    /// Whether it is a shared crate at another version, which a requirement
    /// of the fuzz crate can hold apart from `Cargo.lock`.
    fn may_be_held(&self) -> bool {
        matches!(self, Self::Drift(drift) if drift.may_be_held())
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unlocked(error) if names_requirement(error) => f.write_str(&unsatisfiable(error)),
            Self::Unlocked(error) => {
                write!(f, "{FUZZ_LOCK} does not match {FUZZ_MANIFEST}: {error}")
            }
            Self::Unreadable(reason) => f.write_str(reason),
            Self::Drift(drift) => drift.fmt(f),
            Self::PatchesApart(pair) => {
                let (root, fuzz) = &**pair;
                write!(
                    f,
                    "{} patches {} with `{}` and {} with `{}`, two different crates: point both \
                     `[patch]` entries at the same crate",
                    root.file, root.name, root.written, fuzz.file, fuzz.written
                )
            }
        }
    }
}

/// Each problem with the fuzz lockfile: `locked` is what cargo said when it
/// resolved the fuzz crate with `--locked`, and `fuzz` the fuzz lockfile or
/// why it cannot be read.
pub fn problems(
    locked: Result<(), String>,
    root: &Lock,
    fuzz: Result<Lock, String>,
    patches: &Patches,
) -> Vec<Problem> {
    let mut problems: Vec<Problem> = locked
        .err()
        .map(|error| Problem::Unlocked(error.trim_end().to_owned()))
        .into_iter()
        .collect();
    match fuzz {
        Ok(fuzz) => problems.extend(drift(root, &fuzz, patches).into_iter().map(Problem::Drift)),
        Err(reason) => problems.push(Problem::Unreadable(reason)),
    }
    problems.extend(
        patches
            .apart()
            .into_iter()
            .map(|pair| Problem::PatchesApart(Box::new(pair))),
    );
    problems
}

/// The line `--check` ends with when it finds problems; `None` when there
/// are none and the check passes.
pub fn check_summary(problems: &[Problem]) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let fixable = problems
        .iter()
        .filter(|problem| problem.rebuild_fixes())
        .count();
    let mut summary = format!("fuzz-lock: {} problem(s)", problems.len());
    if fixable > 0 {
        summary += &format!("; run `cargo xtask fuzz-lock` for {fixable} of them");
    }
    // Whether a requirement holds a version apart only shows once the
    // rebuild has run; the rebuild says so then.
    if problems.iter().any(Problem::may_be_held) {
        summary += "; it says what to change when a requirement holds a version apart";
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
    // A version the rebuild leaves apart is held there by a requirement of
    // the fuzz crate itself; running it again changes nothing. The other
    // problems say what to do in their own lines.
    if problems.iter().any(Problem::may_be_held) {
        summary += &format!(
            "; a version apart from {ROOT_LOCK} is held by a requirement in {FUZZ_MANIFEST} or \
             in a crate only it uses: change that requirement, or move {ROOT_LOCK} to a version \
             it accepts"
        );
    }
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

/// What a passing run prints, for a fuzz lockfile of `packages` entries.
pub fn ok_summary(packages: usize) -> String {
    format!("fuzz-lock: {packages} packages, shared crates on the versions of {ROOT_LOCK}, ok")
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
/// `resolve` it, which may rewrite it; returns whether the file changed.
/// When cargo fails, the file is put back byte for byte, or removed when
/// there was none.
pub fn write_resolved(
    file: &Path,
    text: &str,
    resolve: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    let shown = || error_at(Path::new(FUZZ_LOCK));
    let before = match fs::read(file) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(shown()(err)),
    };
    fs::write(file, text).map_err(shown())?;
    if let Err(error) = resolve() {
        match &before {
            Some(bytes) => fs::write(file, bytes),
            None => fs::remove_file(file),
        }
        .map_err(shown())?;
        return Err(resolve_failure(&error));
    }
    let after = fs::read(file).map_err(shown())?;
    Ok(before.as_deref() != Some(after.as_slice()))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::patches::tests::{
        FUZZ_PATCH, FUZZ_PATCH_2, NO_PATCH, ROOT_PATCH, patches_of, resolved,
    };
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

    /// The lockfile [`merged`] writes, parsed.
    fn parse_merged(root: &Lock, fuzz: &Lock, patches: &Patches) -> Lock {
        Lock::parse(&merged(root, fuzz, patches)).unwrap()
    }

    fn version(name: &str, fuzz: &str, root: &str) -> Drift {
        Drift::Version {
            name: name.into(),
            fuzz: fuzz.into(),
            root: root.into(),
            patched: Patched::Neither,
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
        assert_eq!(drift(&root, &fuzz, &Patches::default()), []);
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
        let found = drift(&root, &fuzz, &Patches::default());
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
        assert_eq!(drift(&root, &fuzz, &Patches::default()), []);
        let merged_lock = parse_merged(&root, &fuzz, &Patches::default());
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

    /// The workspace takes smallvec from a path, the fuzz crate from
    /// crates.io.
    fn path_smallvec() -> (Lock, Lock) {
        let root = lock_from(&[
            ("smallvec", "1.16.9", None),
            ("cc", "1.10.0", Some(CRATES_IO)),
        ]);
        let fuzz = lock(&[("smallvec", "1.16.4"), ("cc", "1.10.0")]);
        (root, fuzz)
    }

    /// The two smallvec patches of `Cargo.toml` and `fuzz/Cargo.toml`.
    fn apart() -> Patched {
        let patches = patches_of(ROOT_PATCH, FUZZ_PATCH_2);
        Patched::Apart(Box::new((patches.root[0].clone(), patches.fuzz[0].clone())))
    }

    fn source_drift(patched: Patched) -> Drift {
        Drift::Source {
            name: "smallvec".into(),
            fuzz: "1.16.4".into(),
            fuzz_source: "crates.io".into(),
            root: "1.16.9".into(),
            root_source: "a path".into(),
            patched,
        }
    }

    const APART_ADVICE: &str = "Cargo.toml patches smallvec with `smallvec = { path = \
                                \"vendor/smallvec\" }` and fuzz/Cargo.toml with `smallvec = { \
                                path = \"../vendor/smallvec2\" }`, two different crates: point \
                                both `[patch]` entries at the same crate";

    #[test]
    fn a_crate_only_the_root_patches_drifts_by_source() {
        let (root, fuzz) = path_smallvec();
        let patches = patches_of(ROOT_PATCH, NO_PATCH);
        let found = drift(&root, &fuzz, &patches);
        assert_eq!(found, [source_drift(Patched::Root)]);
        assert!(!found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.4 from crates.io, Cargo.lock takes smallvec 1.16.9 \
             from a path; Cargo.toml patches smallvec: give fuzz/Cargo.toml the same `[patch]`, \
             then run `cargo xtask fuzz-lock`"
        );
        // The rebuild keeps the fuzz crate's own smallvec, not a newer one.
        let merged_lock = parse_merged(&root, &fuzz, &patches);
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
    fn a_crate_both_builds_patch_the_same_way_is_left_to_the_patch() {
        // The rebuild drops the crates.io smallvec, which cargo would
        // otherwise keep, leaving the patch unused.
        let (root, fuzz) = path_smallvec();
        let patches = patches_of(ROOT_PATCH, FUZZ_PATCH);
        let found = drift(&root, &fuzz, &patches);
        assert_eq!(found, [source_drift(Patched::Same)]);
        assert!(found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.4 from crates.io, Cargo.lock takes smallvec 1.16.9 \
             from a path; both builds patch smallvec the same way: run `cargo xtask fuzz-lock`"
        );
        assert_eq!(
            parse_merged(&root, &fuzz, &patches).packages(),
            [("smallvec", "1.16.9"), ("cc", "1.10.0")]
        );
        assert_eq!(problems(Ok(()), &root, Ok(lock(&[])), &patches), []);
        // Another major version is another crate, patch or not.
        let fuzz = lock(&[("smallvec", "2.0.1"), ("cc", "1.10.0")]);
        assert_eq!(
            parse_merged(&root, &fuzz, &patches).packages(),
            [
                ("smallvec", "1.16.9"),
                ("cc", "1.10.0"),
                ("smallvec", "2.0.1")
            ]
        );
    }

    #[test]
    fn a_patch_in_the_cargo_config_counts_for_both_builds() {
        // `.cargo/config.toml` patches the workspace and the fuzz crate.
        let (root, fuzz) = path_smallvec();
        let config = patches_in(".cargo/config.toml", ROOT_PATCH, |path| {
            resolved("/repo", path)
        })
        .unwrap();
        let patches = Patches::new(Vec::new(), Vec::new(), config);
        let found = drift(&root, &fuzz, &patches);
        assert_eq!(found, [source_drift(Patched::Same)]);
        assert!(Problem::Drift(source_drift(Patched::Same)).rebuild_fixes());
        assert_eq!(
            check_summary(&[Problem::Drift(source_drift(Patched::Same))]).unwrap(),
            "fuzz-lock: 1 problem(s); run `cargo xtask fuzz-lock` for 1 of them"
        );
        assert_eq!(
            parse_merged(&root, &fuzz, &patches).packages(),
            [("smallvec", "1.16.9"), ("cc", "1.10.0")]
        );
    }

    #[test]
    fn patches_to_different_crates_at_one_version_are_a_problem() {
        // Both vendored copies are smallvec 1.16.4: once rebuilt, the two
        // lockfiles have the same entry, but the fuzz crate builds the
        // other copy.
        let root = lock_from(&[("smallvec", "1.16.4", None)]);
        let fuzz = lock_from(&[("smallvec", "1.16.4", None)]);
        let patches = patches_of(ROOT_PATCH, FUZZ_PATCH_2);
        assert_eq!(drift(&root, &fuzz, &patches), []);
        let Patched::Apart(pair) = apart() else {
            unreachable!()
        };
        let found = problems(Ok(()), &root, Ok(fuzz), &patches);
        assert_eq!(found, [Problem::PatchesApart(pair)]);
        assert!(!found[0].rebuild_fixes());
        assert_eq!(found[0].to_string(), APART_ADVICE);
        assert_eq!(check_summary(&found).unwrap(), "fuzz-lock: 1 problem(s)");
        assert_eq!(
            rebuild_summary(&found).unwrap(),
            "fuzz-lock: 1 problem(s) the rebuild cannot fix"
        );
        // Before the rebuild the fuzz lockfile still has the crates.io one;
        // the rebuild drops it, so cargo takes the fuzz crate's patch.
        let stale = lock(&[("smallvec", "1.16.4")]);
        assert_eq!(
            parse_merged(&root, &stale, &patches).packages(),
            [("smallvec", "1.16.4")]
        );
    }

    #[test]
    fn patches_to_different_crates_at_two_versions_say_so() {
        // vendor/smallvec2 is smallvec 1.16.9: no requirement holds it.
        let root = lock_from(&[("smallvec", "1.16.4", None)]);
        let fuzz = lock_from(&[("smallvec", "1.16.9", None)]);
        let patches = patches_of(ROOT_PATCH, FUZZ_PATCH_2);
        let found = drift(&root, &fuzz, &patches);
        let expected = Drift::Version {
            name: "smallvec".into(),
            fuzz: "1.16.9".into(),
            root: "1.16.4".into(),
            patched: apart(),
        };
        assert_eq!(found, [expected]);
        assert!(!found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.9, Cargo.lock has 1.16.4; the two builds patch \
             smallvec to different crates"
        );
        let all = problems(Ok(()), &root, Ok(fuzz), &patches);
        assert_eq!(all.len(), 2);
        assert_eq!(
            rebuild_summary(&all).unwrap(),
            "fuzz-lock: 2 problem(s) the rebuild cannot fix"
        );
        assert_eq!(check_summary(&all).unwrap(), "fuzz-lock: 2 problem(s)");
        let source = Drift::Source {
            name: "smallvec".into(),
            fuzz: "1.16.4".into(),
            fuzz_source: "a path".into(),
            root: "1.16.9".into(),
            root_source: "a path".into(),
            patched: apart(),
        };
        assert!(!source.rebuild_fixes());
        assert!(
            source
                .to_string()
                .ends_with("; the two builds patch smallvec to different crates"),
            "{source}"
        );
    }

    #[test]
    fn a_patch_only_the_fuzz_manifest_has_says_to_move_it() {
        // The workspace locks smallvec from crates.io.
        let root = lock(&[("smallvec", "1.16.4"), ("cc", "1.10.0")]);
        let fuzz = lock_from(&[
            ("smallvec", "1.16.9", None),
            ("cc", "1.10.0", Some(CRATES_IO)),
        ]);
        let patches = patches_of(NO_PATCH, FUZZ_PATCH);
        let found = drift(&root, &fuzz, &patches);
        assert_eq!(
            found,
            [Drift::Source {
                name: "smallvec".into(),
                fuzz: "1.16.9".into(),
                fuzz_source: "a path".into(),
                root: "1.16.4".into(),
                root_source: "crates.io".into(),
                patched: Patched::Fuzz,
            }]
        );
        // The rebuild puts Cargo.lock's smallvec in, which cargo keeps over
        // the patch.
        assert!(found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.9 from a path, Cargo.lock takes smallvec 1.16.4 from \
             crates.io; fuzz/Cargo.toml patches smallvec and Cargo.toml does not: remove the \
             patch from fuzz/Cargo.toml, or add it to Cargo.toml; until then `cargo xtask \
             fuzz-lock` takes the version Cargo.lock has and leaves the patch unused"
        );
        assert_eq!(
            parse_merged(&root, &fuzz, &patches).packages(),
            [
                ("smallvec", "1.16.4"),
                ("cc", "1.10.0"),
                ("smallvec", "1.16.9"),
            ]
        );
        let held = Drift::Version {
            name: "smallvec".into(),
            fuzz: "1.16.9".into(),
            root: "1.16.4".into(),
            patched: Patched::Fuzz,
        };
        assert!(!held.rebuild_fixes());
        assert!(
            held.to_string().ends_with("or add it to Cargo.toml"),
            "{held}"
        );
        assert_eq!(
            rebuild_summary(&[Problem::Drift(held)]).unwrap(),
            "fuzz-lock: 1 problem(s) the rebuild cannot fix"
        );
    }

    #[test]
    fn a_source_no_patch_explains_says_what_to_check() {
        // A workspace crate named smallvec, not a patch.
        let (root, fuzz) = path_smallvec();
        let found = drift(&root, &fuzz, &Patches::default());
        assert_eq!(found, [source_drift(Patched::Neither)]);
        assert!(!found[0].rebuild_fixes());
        assert_eq!(
            found[0].to_string(),
            "fuzz/Cargo.lock: smallvec 1.16.4 from crates.io, Cargo.lock takes smallvec 1.16.9 \
             from a path; neither build patches smallvec from the repository: check whether the \
             workspace has a crate of that name, which is another crate than the fuzz crate's, \
             or a cargo config outside the repository patches it"
        );
        assert_eq!(
            parse_merged(&root, &fuzz, &Patches::default()).packages(),
            [
                ("smallvec", "1.16.9"),
                ("cc", "1.10.0"),
                ("smallvec", "1.16.4"),
            ]
        );
    }

    #[test]
    fn a_resolved_rebuild_reports_whether_it_changed_the_file() {
        let dir = crate::paths::scratch("fuzz-lock-resolved");
        let file = dir.join("Cargo.lock");
        assert_eq!(write_resolved(&file, "a", || Ok(())), Ok(true));
        assert_eq!(write_resolved(&file, "a", || Ok(())), Ok(false));
        // Cargo rewriting the file to what it was counts as no change.
        let rewrite = || fs::write(&file, "a").map_err(|err| err.to_string());
        assert_eq!(write_resolved(&file, "b", rewrite), Ok(false));
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
    fn problems_list_cargo_first_then_each_drift() {
        let root = lock(&[("cc", "1.11.3"), ("syn", "2.0.119")]);
        let fuzz = lock(&[("cc", "1.10.0"), ("syn", "2.0.119")]);
        let none = &Patches::default();
        assert_eq!(
            problems(Ok(()), &root, Ok(lock(&[("cc", "1.11.3")])), none),
            []
        );
        assert_eq!(
            problems(Err(format!("{STALE}\n\n")), &root, Ok(fuzz), none),
            [
                Problem::Unlocked(STALE.into()),
                Problem::Drift(version("cc", "1.10.0", "1.11.3")),
            ]
        );
        assert_eq!(
            problems(Ok(()), &root, Err("fuzz/Cargo.lock: bad".into()), none),
            [Problem::Unreadable("fuzz/Cargo.lock: bad".into())]
        );
    }

    #[test]
    fn problems_say_whether_the_rebuild_fixes_them() {
        let stale = Problem::Unlocked(STALE.into());
        assert!(stale.rebuild_fixes());
        assert_eq!(
            stale.to_string(),
            format!("fuzz/Cargo.lock does not match fuzz/Cargo.toml: {STALE}")
        );
        let unsatisfied = Problem::Unlocked(UNSATISFIED.into());
        assert!(!unsatisfied.rebuild_fixes());
        assert_eq!(
            unsatisfied.to_string(),
            format!(
                "cargo cannot resolve fuzz/Cargo.toml: {UNSATISFIED}; fix the requirement it names"
            )
        );
        let missing = Problem::Unlocked("error: no matching package named `nope` found".into());
        assert!(!missing.rebuild_fixes());
        let unreadable = Problem::Unreadable("fuzz/Cargo.lock: bad".into());
        assert!(unreadable.rebuild_fixes());
        assert_eq!(unreadable.to_string(), "fuzz/Cargo.lock: bad");
        let drift = Problem::Drift(source_drift(Patched::Root));
        assert!(!drift.rebuild_fixes());
        assert_eq!(drift.to_string(), source_drift(Patched::Root).to_string());
        assert!(Problem::Drift(source_drift(Patched::Same)).rebuild_fixes());
        assert!(Problem::Drift(version("cc", "1.10.0", "1.11.3")).rebuild_fixes());
    }

    #[test]
    fn the_check_says_how_many_problems_the_rebuild_fixes() {
        assert_eq!(check_summary(&[]), None);
        let held = Problem::Drift(version("memchr", "2.8.0", "2.8.3"));
        let patch = Problem::Drift(source_drift(Patched::Root));
        assert_eq!(check_summary(&[patch]).unwrap(), "fuzz-lock: 1 problem(s)");
        assert_eq!(
            check_summary(&[
                Problem::Unlocked(STALE.into()),
                Problem::Unlocked(UNSATISFIED.into())
            ])
            .unwrap(),
            "fuzz-lock: 2 problem(s); run `cargo xtask fuzz-lock` for 1 of them"
        );
        assert_eq!(
            check_summary(&[held, Problem::Drift(source_drift(Patched::Root))]).unwrap(),
            "fuzz-lock: 2 problem(s); run `cargo xtask fuzz-lock` for 1 of them; it says what to \
             change when a requirement holds a version apart"
        );
    }

    #[test]
    fn the_rebuild_names_a_held_version_only_when_one_is_left() {
        assert_eq!(rebuild_summary(&[]), None);
        assert_eq!(
            rebuild_summary(&[Problem::Drift(source_drift(Patched::Same))]).unwrap(),
            "fuzz-lock: 1 problem(s) the rebuild cannot fix"
        );
        assert_eq!(
            rebuild_summary(&[
                Problem::Drift(source_drift(Patched::Root)),
                Problem::Drift(version("memchr", "2.8.0", "2.8.3")),
            ])
            .unwrap(),
            "fuzz-lock: 2 problem(s) the rebuild cannot fix; a version apart from Cargo.lock is \
             held by a requirement in fuzz/Cargo.toml or in a crate only it uses: change that \
             requirement, or move Cargo.lock to a version it accepts"
        );
    }

    #[test]
    fn the_rebuild_says_whether_it_changed_the_file() {
        assert_eq!(rebuilt(false), "fuzz-lock: fuzz/Cargo.lock is up to date");
        assert_eq!(rebuilt(true), "fuzz-lock: wrote fuzz/Cargo.lock");
        assert_eq!(
            ok_summary(81),
            "fuzz-lock: 81 packages, shared crates on the versions of Cargo.lock, ok"
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
        let merged_lock = parse_merged(&root, &fuzz, &Patches::default());
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
        let alone = parse_merged(&root, &Lock::default(), &Patches::default());
        assert_eq!(alone.packages(), root.packages());
    }

    #[test]
    fn merging_keeps_fuzz_entries_it_cannot_read() {
        let root = lock(&[("cc", "1.11.3")]);
        let text = format!("{HEADER}\n[[package]]\nname = \"odd\"\nversion = \"1.x\"\n");
        let merged_text = merged(&root, &Lock::parse(&text).unwrap(), &Patches::default());
        assert!(merged_text.contains("name = \"odd\""), "{merged_text}");
    }
}
