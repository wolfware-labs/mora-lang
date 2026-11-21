//! `cargo xtask fuzz-lock`: keeps the fuzz crate on the code the workspace
//! builds (spec §20.1).
//!
//! The fuzz crate sits outside the workspace and has a lockfile of its own.
//! The fuzz targets test `mora-syntax`, so both builds must build it and
//! every crate it reaches through normal dependencies, on any platform, the
//! same way: the same version from the same source, on the same
//! dependencies. `--check` asks `cargo metadata` what each build resolves
//! and fails when one of those crates differs; crates outside that set, such
//! as those only the fuzzing runtime or the workspace's tools use, are not
//! compared. The command itself rebuilds `fuzz/Cargo.lock` from
//! `Cargo.lock`.
//!
//! Features are compared too, but a difference is a warning, not a failure:
//! cargo turns a feature on for every crate of a workspace build when one
//! member asks for it, so tools and benchmarks turn on features of
//! `mora-syntax`'s dependencies that the fuzz build has no need for.
//!
//! Two packages are the same crate when cargo would let only one of them in
//! a resolve: the same name and versions that are semver compatible.

mod resolve;

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use cargo_metadata::semver::Version;
use toml::{Table, Value};

pub use self::resolve::{Drift, Unresolved, check};
use crate::paths::{FUZZ_LOCK, FUZZ_MANIFEST, ROOT_LOCK, ROOT_MANIFEST, error_at};

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

    /// Reads and parses the lockfile `file` under `root`; errors name
    /// `file` as given.
    fn read(root: &Path, file: &str) -> Result<Self, String> {
        let text = fs::read_to_string(root.join(file)).map_err(error_at(Path::new(file)))?;
        Self::parse(&text).map_err(error_at(Path::new(file)))
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

/// What a cargo error is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// Cargo could not fetch something: it runs offline, or the network or
    /// a source is out of reach. Running again once it can may work.
    Unreachable,
    /// A requirement no version satisfies, which neither lockfile can
    /// change.
    Requirement,
    /// Anything else, such as a lockfile `--locked` keeps cargo from
    /// updating.
    Other,
}

impl Cause {
    /// Reads cargo's `error`. A requirement cargo cannot meet offline is
    /// unreachable, not a bad requirement.
    fn of(error: &str) -> Self {
        const UNREACHABLE: [&str; 9] = [
            "offline mode",
            "--offline was specified",
            "failed to download",
            "failed to query replaced source",
            "failed to update registry",
            "Unable to update registry",
            "spurious network error",
            "Couldn't resolve host",
            "failed to get successful HTTP response",
        ];
        if UNREACHABLE.iter().any(|marker| error.contains(marker)) {
            Self::Unreachable
        } else if error.contains("failed to select a version for")
            || error.contains("no matching package")
        {
            Self::Requirement
        } else {
            Self::Other
        }
    }
}

/// Why cargo could not resolve `manifest`, from its `error`.
fn unresolved(manifest: &str, error: &str) -> String {
    match Cause::of(error) {
        Cause::Unreachable => format!(
            "cargo cannot fetch what {manifest} needs: {error}; it runs offline or cannot reach \
             a source: check the network and cargo's `net.offline` setting, then run again"
        ),
        Cause::Requirement => {
            format!("cargo cannot resolve {manifest}: {error}; fix the requirement it names")
        }
        Cause::Other => format!("cargo cannot resolve {manifest}: {error}"),
    }
}

/// A problem with `fuzz/Cargo.lock`.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// Cargo cannot resolve the fuzz crate with `--locked`: the fuzz
    /// lockfile needs a change, a requirement no version satisfies, or
    /// cargo could not fetch something; its error.
    Unlocked(String),
    /// A crate the fuzz crate builds at another version, from another
    /// source or on other dependencies than the workspace does.
    Drift(Drift),
}

impl Problem {
    /// Whether `cargo xtask fuzz-lock` may remove it. A crate built
    /// differently is fixed by the rebuild when only the fuzz lockfile holds
    /// it apart, which shows once the rebuild has run.
    fn rebuild_may_fix(&self) -> bool {
        match self {
            Self::Unlocked(error) => Cause::of(error) != Cause::Requirement,
            Self::Drift(_) => true,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unlocked(error) if Cause::of(error) == Cause::Other => {
                write!(f, "{FUZZ_LOCK} does not match {FUZZ_MANIFEST}: {error}")
            }
            Self::Unlocked(error) => f.write_str(&unresolved(FUZZ_MANIFEST, error)),
            Self::Drift(drift) => drift.fmt(f),
        }
    }
}

/// What cargo resolves for the fuzz crate, checked against the workspace.
#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    /// Each problem with the fuzz lockfile.
    pub problems: Vec<Problem>,
    /// Each crate built with other features, which the check reports
    /// without failing: cargo turns a feature on for a whole workspace when
    /// one member asks for it, so tools and benchmarks turn on features the
    /// fuzz build does not need.
    pub warnings: Vec<Drift>,
    /// The number of packages the comparison covers: `mora-syntax` and the
    /// crates it depends on, in the fuzz crate's build.
    pub packages: usize,
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

/// How a crate built with other features is printed.
pub fn warning(features: &Drift) -> String {
    format!("fuzz-lock: warning: {features}")
}

/// What a passing run prints, for a comparison of `packages` packages that
/// printed `warnings` feature warnings.
pub fn ok_summary(packages: usize, warnings: usize) -> String {
    format!(
        "fuzz-lock: mora-syntax and its normal dependencies, {packages} packages, built alike by \
         both builds (version, source and dependencies; {warnings} feature warning(s)), ok"
    )
}

impl fmt::Display for Unresolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&unresolved(self.manifest, self.error.trim_end()))
    }
}

/// Why the rebuild failed, from the build cargo could not resolve; the
/// rebuild has put the file back as it was.
pub fn resolve_failure(failed: &Unresolved) -> String {
    format!("{failed}; {FUZZ_LOCK} is left as it was")
}

/// Writes `text`, the rebuilt fuzz lockfile, to `file` and has cargo
/// `resolve` and check it, which may rewrite it; returns whether the file
/// changed, and the check. When cargo fails, or the check finds problems,
/// the file is put back byte for byte, or removed when there was none, so
/// that a rebuild that cannot settle every crate moves none.
pub fn write_resolved(
    file: &Path,
    text: &str,
    resolve: impl FnOnce() -> Result<Checked, Unresolved>,
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
        Err(failed) => {
            restore()?;
            return Err(resolve_failure(&failed));
        }
    };
    if !checked.problems.is_empty() {
        restore()?;
        return Ok((false, checked));
    }
    let after = fs::read(file).map_err(shown())?;
    Ok((before.as_deref() != Some(after.as_slice()), checked))
}

/// Rebuilds the fuzz lockfile of the repository at `root` from its
/// `Cargo.lock`, keeping the entries of the crates only the fuzz crate
/// uses, has cargo resolve it and checks it; returns whether the file
/// changed, and the check. A missing or broken fuzz lockfile, such as one
/// left with merge conflicts, is rebuilt from `Cargo.lock` alone. When
/// cargo cannot resolve either build, or a crate is still built
/// differently, the fuzz lockfile is left as it was.
pub fn rebuild(root: &Path) -> Result<(bool, Checked), String> {
    let fuzz = Lock::read(root, FUZZ_LOCK).unwrap_or_else(|err| {
        eprintln!("fuzz-lock: rebuilding {FUZZ_LOCK} from {ROOT_LOCK} alone: {err}");
        Lock::default()
    });
    let text = merged(&Lock::read(root, ROOT_LOCK)?, &fuzz);
    write_resolved(&root.join(FUZZ_LOCK), &text, || {
        resolve::resolve(root, FUZZ_MANIFEST, false)?;
        check(root)
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    use super::resolve::tests::{CRATES_IO, VENDOR, built, drift_of, registry};
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

    /// Cargo's `error` about the fuzz manifest.
    fn fuzz_failed(error: &str) -> Unresolved {
        Unresolved {
            manifest: FUZZ_MANIFEST,
            error: error.into(),
        }
    }

    #[test]
    fn a_lockfile_that_is_not_toml_is_an_error() {
        assert!(Lock::parse("[[package]\nname = \"cc\"\n").is_err());
        assert!(Lock::parse("<<<<<<< HEAD\nversion = 4\n=======\n").is_err());
    }

    #[test]
    fn a_lockfile_is_read_under_the_root() {
        let dir = crate::paths::scratch("fuzz-lock-read");
        fs::write(
            dir.join("Cargo.lock"),
            format!("{HEADER}\n[[package]]\nname = \"cc\"\nversion = \"1.0.0\"\n"),
        )
        .unwrap();
        assert_eq!(
            packages(&Lock::read(&dir, "Cargo.lock").unwrap()),
            [("cc", "1.0.0")]
        );
        let err = Lock::read(&dir, "fuzz/Cargo.lock").err().unwrap();
        assert!(err.starts_with("fuzz/Cargo.lock: "), "{err}");
        fs::write(dir.join("Cargo.lock"), "[[package]\n").unwrap();
        let err = Lock::read(&dir, "Cargo.lock").err().unwrap();
        assert!(err.starts_with("Cargo.lock: "), "{err}");
        let _ = fs::remove_dir_all(dir);
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
            warnings: Vec::new(),
            packages: 71,
        }
    }

    /// A check that still finds smallvec built from two sources.
    fn apart() -> Checked {
        Checked {
            problems: vec![Problem::Drift(drift_of(
                built("smallvec", "1.16.4", VENDOR),
                registry("smallvec", "1.16.6"),
            ))],
            warnings: Vec::new(),
            packages: 71,
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
            fs::write(&file, "a").unwrap();
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
                Err(fuzz_failed("error: the lock file is odd\n"))
            });
            assert_eq!(
                failed,
                Err(
                    "cargo cannot resolve fuzz/Cargo.toml: error: the lock file is odd; \
                     fuzz/Cargo.lock is left as it was"
                        .into()
                )
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
            fs::write(&file, "rebuilt by cargo").unwrap();
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
        let failed = write_resolved(&file, "rebuilt", || Err(fuzz_failed(UNSATISFIED)));
        assert_eq!(failed, Err(resolve_failure(&fuzz_failed(UNSATISFIED))));
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
        let failed = write_resolved(&file, "rebuilt", || Err(fuzz_failed("error: odd")));
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let err = failed.unwrap_err();
        assert!(err.starts_with("fuzz/Cargo.lock: "), "{err}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "version = 4\n");
        let _ = fs::remove_dir_all(dir);
    }

    const STALE: &str = "error: cannot update the lock file fuzz/Cargo.lock because --locked \
                         was passed to prevent this\nhelp: to generate the lock file without \
                         accessing the network, remove the --locked flag and use --offline \
                         instead.";
    const UNSATISFIED: &str = "error: failed to select a version for the requirement \
                               `memchr = \"^9\"`\ncandidate versions found which didn't match";
    const OFFLINE: &str = "error: failed to select a version for the requirement `memchr = \
                           \"^9\"`\nAs a reminder, you're using offline mode (--offline) which \
                           can sometimes cause surprising resolution failures";

    #[test]
    fn cargo_errors_are_told_apart() {
        assert_eq!(Cause::of(STALE), Cause::Other);
        assert_eq!(Cause::of(UNSATISFIED), Cause::Requirement);
        assert_eq!(
            Cause::of("error: no matching package named `nope` found"),
            Cause::Requirement
        );
        assert_eq!(Cause::of(OFFLINE), Cause::Unreachable);
        for error in [
            "error: failed to download from `https://index.crates.io/`",
            "error: attempting to make an HTTP request, but --offline was specified",
            "error: failed to query replaced source registry `crates-io`",
            "Caused by: failed to get successful HTTP response from `https://x`",
            "[6] Couldn't resolve host name (Could not resolve host: index.crates.io)",
            "warning: spurious network error (3 tries remaining)",
            "error: Unable to update registry `crates-io`",
            "error: failed to update registry `crates-io`",
            "error: can't checkout from 'https://x': you are in the offline mode (--offline)",
        ] {
            assert_eq!(Cause::of(error), Cause::Unreachable, "{error}");
        }
    }

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
        let offline = Problem::Unlocked(OFFLINE.into());
        assert!(offline.rebuild_may_fix());
        assert_eq!(
            offline.to_string(),
            format!(
                "cargo cannot fetch what fuzz/Cargo.toml needs: {OFFLINE}; it runs offline or \
                 cannot reach a source: check the network and cargo's `net.offline` setting, \
                 then run again"
            )
        );
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
            ok_summary(71, 6),
            "fuzz-lock: mora-syntax and its normal dependencies, 71 packages, built alike by both \
             builds (version, source and dependencies; 6 feature warning(s)), ok"
        );
        let features = Drift::Features {
            package: registry("smallvec", "1.16.4"),
            root: vec!["union".into()],
            fuzz: Vec::new(),
        };
        assert_eq!(
            warning(&features),
            format!("fuzz-lock: warning: {features}")
        );
    }

    #[test]
    fn a_failed_rebuild_names_the_build_and_the_cause() {
        assert_eq!(
            resolve_failure(&fuzz_failed(UNSATISFIED)),
            format!(
                "cargo cannot resolve fuzz/Cargo.toml: {UNSATISFIED}; fix the requirement it \
                 names; fuzz/Cargo.lock is left as it was"
            )
        );
        let workspace = Unresolved {
            manifest: ROOT_MANIFEST,
            error: format!("{UNSATISFIED}\n"),
        };
        assert_eq!(
            resolve_failure(&workspace),
            format!(
                "cargo cannot resolve Cargo.toml: {UNSATISFIED}; fix the requirement it names; \
                 fuzz/Cargo.lock is left as it was"
            )
        );
        let offline = resolve_failure(&fuzz_failed(OFFLINE));
        assert!(
            offline.starts_with("cargo cannot fetch what fuzz/Cargo.toml needs: "),
            "{offline}"
        );
        assert!(
            !offline.contains("fix the requirement it names"),
            "{offline}"
        );
    }
}
