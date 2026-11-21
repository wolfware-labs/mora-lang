//! Small path-only repositories laid out as this one, which `cargo metadata`
//! resolves offline, for [`check`] and [`rebuild`] end to end.

use std::path::PathBuf;
use std::process::Command;

use super::resolve::tests::{built, drift_of, registry};
use super::resolve::{FUZZED, Unit, resolve_locked};
use super::*;
use crate::paths::scratch;
use crate::tools;

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
        let output = Command::new(tools::cargo())
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
fn a_missing_or_broken_fuzz_lock_is_rebuilt_from_the_workspace_lock() {
    let fixture = Fixture::new("fuzz-lock-broken");
    let locked = fixture.fuzz_lock();
    fixture.write(FUZZ_LOCK, "<<<<<<< HEAD\nversion = 4\n=======\n");
    assert!(rebuild(&fixture.0).unwrap().0);
    assert_eq!(fixture.fuzz_lock(), locked);
    fs::remove_file(fixture.0.join(FUZZ_LOCK)).unwrap();
    let (changed, checked) = rebuild(&fixture.0).unwrap();
    assert!(changed);
    assert_eq!(checked.problems, []);
    assert_eq!(fixture.fuzz_lock(), locked);
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
    let metadata = resolve_locked(&fixture.0, ROOT_MANIFEST).unwrap();
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
    // Cargo's error ends with a newline, which the message drops.
    assert_eq!(unresolved.error, unresolved.error.trim_end());
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
