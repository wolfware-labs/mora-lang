//! `cargo xtask bench-compare`: runs the workspace's criterion benchmarks on
//! a base revision and on the working tree, and reports the change of each
//! (§20.6). A benchmark that is slower by more than the threshold, with the
//! whole confidence interval of the change above zero, fails the command.
//!
//! The base is checked out as a detached worktree under the target
//! directory, built in a target directory of its own so that later runs
//! reuse its build, and benchmarked with criterion's `--save-baseline`. The
//! working tree then runs with `--baseline-lenient`, which compares against
//! those results and accepts benchmarks the base does not have.
//!
//! This module reads arguments, finds the bench targets and reads and judges
//! criterion's results; [`crate::bench_run`] runs git and cargo.

use std::fs;
use std::path::{Path, PathBuf};

use cargo_metadata::Metadata;
use serde_json::Value;

/// The name of the criterion baseline the base revision saves.
pub const BASELINE: &str = "base";

/// What the command was asked to do.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The revision to compare with.
    pub base: String,
    /// The slowdown that fails the command, as a fraction (0.10 is 10%).
    pub threshold: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base: "origin/main".to_owned(),
            threshold: 0.10,
        }
    }
}

/// Reads `[--base <rev>] [--threshold <percent>]`.
pub fn parse_args(args: &[&str]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        let mut value = || {
            rest.next()
                .copied()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--base" => options.base = value()?.to_owned(),
            "--threshold" => {
                let text = value()?;
                let percent: f64 = text
                    .parse()
                    .map_err(|_| format!("--threshold {text} is not a number"))?;
                if !percent.is_finite() || percent < 0.0 {
                    return Err(format!("--threshold {text} must be zero or more"));
                }
                options.threshold = percent / 100.0;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// A criterion benchmark target of the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The package that has it.
    pub package: String,
    /// The bench target's name.
    pub name: String,
    /// Its source file, relative to the repository root.
    pub source: PathBuf,
}

/// The bench targets of the workspace, which are criterion benchmarks: a
/// bench target with the default harness would reject criterion's flags.
pub fn targets(metadata: &Metadata) -> Vec<Target> {
    // Cargo writes the workspace root and the source paths the same way, so
    // one is a prefix of the other on every platform.
    let root = metadata.workspace_root.as_std_path();
    let mut found = Vec::new();
    for package in metadata.workspace_packages() {
        for target in &package.targets {
            if !target.is_bench() {
                continue;
            }
            let source = target.src_path.as_std_path();
            let source = source.strip_prefix(root).unwrap_or(source);
            found.push(Target {
                package: package.name.to_string(),
                name: target.name.clone(),
                source: source.to_path_buf(),
            });
        }
    }
    found.sort_by(|a, b| (&a.package, &a.name).cmp(&(&b.package, &b.name)));
    found
}

/// How one benchmark compares with the base.
#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    /// The benchmark's id: package, bench target, then criterion's group
    /// and function, such as `mora-hir/front_end/cold/parse`.
    pub id: String,
    /// The mean time on the base, in nanoseconds, when the base has it.
    pub base: Option<f64>,
    /// The mean time on the working tree, in nanoseconds, when the working
    /// tree has it.
    pub new: Option<f64>,
    /// The relative change of the mean, when the base has the benchmark.
    pub change: Option<Change>,
}

/// The relative change of a mean and its confidence interval, as
/// fractions: 0.05 is 5% slower.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    pub mean: f64,
    pub lower: f64,
    pub upper: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Only the working tree has the benchmark.
    New,
    /// Only the base has the benchmark.
    Removed,
    /// Slower by more than the threshold, and the slowdown is significant.
    Regressed,
    /// Faster by more than the threshold, and the speedup is significant.
    Improved,
    /// Neither.
    Unchanged,
}

impl Comparison {
    pub fn verdict(&self, threshold: f64) -> Verdict {
        if self.new.is_none() {
            return Verdict::Removed;
        }
        match self.change {
            None => Verdict::New,
            Some(change) if change.mean > threshold && change.lower > 0.0 => Verdict::Regressed,
            Some(change) if change.mean < -threshold && change.upper < 0.0 => Verdict::Improved,
            Some(_) => Verdict::Unchanged,
        }
    }
}

/// The targets the base also has, by package and name, and those it lacks.
/// A source file left behind without its `[[bench]]` entry is not a target.
pub fn split_by_base<'a>(
    base: &[Target],
    targets: &'a [Target],
) -> (Vec<&'a Target>, Vec<&'a Target>) {
    targets.iter().partition(|target| {
        base.iter()
            .any(|other| other.package == target.package && other.name == target.name)
    })
}

/// Deletes the `new` results under `home`. The base's run leaves its own
/// there, which would otherwise read as the working tree's results of a
/// benchmark the working tree no longer has.
pub fn clear_new(home: &Path) -> Result<(), String> {
    let mut pending = vec![home.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|err| format!("{}: {err}", dir.display()))?
                .path();
            if !path.is_dir() {
                continue;
            }
            // A benchmark group may be named `new` too; only a directory
            // with results is one to remove.
            if path.file_name().is_some_and(|name| name == "new")
                && path.join("estimates.json").is_file()
            {
                fs::remove_dir_all(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            } else {
                pending.push(path);
            }
        }
    }
    Ok(())
}

/// Reads the results criterion wrote under `home`: each directory with a
/// `new/estimates.json` or a baseline's `estimates.json` is a benchmark,
/// named by its path below `home`.
pub fn collect(home: &Path) -> Result<Vec<Comparison>, String> {
    let mut found = Vec::new();
    let mut pending = vec![home.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = fs::read_dir(&dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|err| format!("{}: {err}", dir.display()))?
                .path();
            if path.is_dir() {
                pending.push(path);
            }
        }
        let new = dir.join("new/estimates.json");
        let base = dir.join(BASELINE).join("estimates.json");
        if !new.is_file() && !base.is_file() {
            continue;
        }
        let id = dir
            .strip_prefix(home)
            .unwrap_or(&dir)
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let change = dir.join("change/estimates.json");
        found.push(Comparison {
            id,
            new: if new.is_file() {
                Some(mean(&read(&new)?, &new)?)
            } else {
                None
            },
            base: if base.is_file() {
                Some(mean(&read(&base)?, &base)?)
            } else {
                None
            },
            change: if change.is_file() {
                Some(relative_change(&read(&change)?, &change)?)
            } else {
                None
            },
        });
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(found)
}

fn read(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
}

fn number(value: &Value, pointer: &str, path: &Path) -> Result<f64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{}: no number at {pointer}", path.display()))
}

/// The mean of an `estimates.json` of measurements, in nanoseconds.
fn mean(value: &Value, path: &Path) -> Result<f64, String> {
    number(value, "/mean/point_estimate", path)
}

/// The relative change of the mean from a `change/estimates.json`.
fn relative_change(value: &Value, path: &Path) -> Result<Change, String> {
    Ok(Change {
        mean: number(value, "/mean/point_estimate", path)?,
        lower: number(value, "/mean/confidence_interval/lower_bound", path)?,
        upper: number(value, "/mean/confidence_interval/upper_bound", path)?,
    })
}

/// A time in nanoseconds with a unit that keeps three significant digits.
pub fn duration(nanos: f64) -> String {
    let (value, unit) = if nanos >= 1e9 {
        (nanos / 1e9, "s")
    } else if nanos >= 1e6 {
        (nanos / 1e6, "ms")
    } else if nanos >= 1e3 {
        (nanos / 1e3, "µs")
    } else {
        (nanos, "ns")
    };
    format!("{value:.3} {unit}")
}

/// The report, one line per benchmark, and how many regressed.
pub fn report(comparisons: &[Comparison], options: &Options) -> (String, usize) {
    let width = comparisons.iter().map(|c| c.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    let mut regressions = 0;
    for comparison in comparisons {
        let verdict = comparison.verdict(options.threshold);
        let base = comparison.base.map_or_else(|| "-".to_owned(), duration);
        let new = comparison.new.map_or_else(|| "-".to_owned(), duration);
        let change = comparison.change.map_or_else(
            || "-".to_owned(),
            |change| {
                format!(
                    "{:+.1}% [{:+.1}%, {:+.1}%]",
                    change.mean * 100.0,
                    change.lower * 100.0,
                    change.upper * 100.0
                )
            },
        );
        let label = match verdict {
            Verdict::New => "new",
            Verdict::Removed => "removed",
            Verdict::Regressed => {
                regressions += 1;
                "REGRESSED"
            }
            Verdict::Improved => "improved",
            Verdict::Unchanged => "unchanged",
        };
        out.push_str(&format!(
            "{:<width$}  {:>12}  {:>12}  {:<28}  {label}\n",
            comparison.id, base, new, change,
        ));
    }
    (out, regressions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison(change: Option<(f64, f64, f64)>) -> Comparison {
        Comparison {
            id: "cold/parse".to_owned(),
            base: change.map(|_| 100.0),
            new: Some(110.0),
            change: change.map(|(mean, lower, upper)| Change { mean, lower, upper }),
        }
    }

    #[test]
    fn bench_targets_come_from_the_workspace() {
        let metadata = crate::metadata_command().exec().unwrap();
        let found = targets(&metadata);
        let front_end = Target {
            package: "mora-hir".to_owned(),
            name: "front_end".to_owned(),
            source: PathBuf::from("crates/mora-hir/benches/front_end.rs"),
        };
        assert!(found.contains(&front_end), "{found:?}");
        // Libraries, binaries and tests are not bench targets.
        assert!(found.iter().all(|t| t.source.is_relative()), "{found:?}");
        assert!(
            found.iter().all(|t| t
                .source
                .components()
                .any(|part| part.as_os_str() == "benches")),
            "{found:?}"
        );
    }

    #[test]
    fn targets_the_base_lacks_are_new() {
        let target = |package: &str, name: &str| Target {
            package: package.to_owned(),
            name: name.to_owned(),
            source: PathBuf::from(format!("{package}/benches/{name}.rs")),
        };
        let base = vec![target("a", "old"), target("b", "same")];
        let all = vec![target("a", "old"), target("a", "same"), target("a", "new")];
        let (present, missing) = split_by_base(&base, &all);
        assert_eq!(present, vec![&all[0]]);
        assert_eq!(missing, vec![&all[1], &all[2]]);
    }

    #[test]
    fn arguments_default_to_main_and_ten_percent() {
        assert_eq!(parse_args(&[]).unwrap(), Options::default());
        let options = parse_args(&["--base", "HEAD~1", "--threshold", "5"]).unwrap();
        assert_eq!(options.base, "HEAD~1");
        assert!((options.threshold - 0.05).abs() < 1e-12);
    }

    #[test]
    fn bad_arguments_are_errors() {
        assert!(parse_args(&["--base"]).is_err());
        assert!(parse_args(&["--threshold", "fast"]).is_err());
        assert!(parse_args(&["--threshold", "-1"]).is_err());
        assert!(parse_args(&["--threshold", "inf"]).is_err());
        assert!(parse_args(&["--quick"]).is_err());
        assert!(parse_args(&["--threshold", "0"]).is_ok());
    }

    #[test]
    fn a_regression_needs_the_threshold_and_significance() {
        let t = 0.10;
        assert_eq!(comparison(None).verdict(t), Verdict::New);
        let removed = Comparison {
            new: None,
            ..comparison(Some((0.5, 0.4, 0.6)))
        };
        assert_eq!(removed.verdict(t), Verdict::Removed);
        assert_eq!(
            comparison(Some((0.15, 0.01, 0.3))).verdict(t),
            Verdict::Regressed
        );
        // Over the threshold but the interval reaches zero: noise.
        assert_eq!(
            comparison(Some((0.15, -0.01, 0.3))).verdict(t),
            Verdict::Unchanged
        );
        assert_eq!(
            comparison(Some((0.15, 0.0, 0.3))).verdict(t),
            Verdict::Unchanged
        );
        // Significant but under the threshold.
        assert_eq!(
            comparison(Some((0.05, 0.02, 0.08))).verdict(t),
            Verdict::Unchanged
        );
        assert_eq!(
            comparison(Some((0.10, 0.05, 0.15))).verdict(t),
            Verdict::Unchanged
        );
        assert_eq!(
            comparison(Some((-0.15, -0.3, -0.01))).verdict(t),
            Verdict::Improved
        );
        assert_eq!(
            comparison(Some((-0.15, -0.3, 0.0))).verdict(t),
            Verdict::Unchanged
        );
        assert_eq!(
            comparison(Some((-0.10, -0.15, -0.05))).verdict(t),
            Verdict::Unchanged
        );
    }

    fn write_estimates(dir: &Path, mean: f64, lower: f64, upper: f64) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{"mean":{{"confidence_interval":{{"confidence_level":0.95,"lower_bound":{lower},"upper_bound":{upper}}},"point_estimate":{mean},"standard_error":1.0}}}}"#
        );
        fs::write(dir.join("estimates.json"), json).unwrap();
    }

    #[test]
    fn results_are_read_from_the_criterion_directory() {
        let home = std::env::temp_dir().join(format!("bench-compare-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let parse = home.join("cold/parse");
        write_estimates(&parse.join("new"), 2000.0, 1900.0, 2100.0);
        write_estimates(&parse.join("base"), 1000.0, 950.0, 1050.0);
        write_estimates(&parse.join("change"), 1.0, 0.9, 1.1);
        let body = home.join("edit/body");
        write_estimates(&body.join("new"), 500.0, 490.0, 510.0);
        // Only the base ran it: the working tree removed it.
        let old = home.join("edit/old");
        write_estimates(&old.join("base"), 300.0, 290.0, 310.0);
        // A directory without results is not a benchmark.
        fs::create_dir_all(home.join("report")).unwrap();
        let found = collect(&home).unwrap();
        fs::remove_dir_all(&home).unwrap();
        assert_eq!(
            found,
            vec![
                Comparison {
                    id: "cold/parse".to_owned(),
                    base: Some(1000.0),
                    new: Some(2000.0),
                    change: Some(Change {
                        mean: 1.0,
                        lower: 0.9,
                        upper: 1.1
                    }),
                },
                Comparison {
                    id: "edit/body".to_owned(),
                    base: None,
                    new: Some(500.0),
                    change: None,
                },
                Comparison {
                    id: "edit/old".to_owned(),
                    base: Some(300.0),
                    new: None,
                    change: None,
                },
            ]
        );
    }

    #[test]
    fn clearing_new_results_keeps_the_baseline() {
        let home = std::env::temp_dir().join(format!("bench-compare-clear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let parse = home.join("cold/parse");
        write_estimates(&parse.join("new"), 2000.0, 1900.0, 2100.0);
        write_estimates(&parse.join("base"), 1000.0, 950.0, 1050.0);
        // A file named `new` is not a result directory, and neither is a
        // group named `new`.
        fs::write(home.join("cold/new"), "").unwrap();
        write_estimates(&home.join("new/case/new"), 7.0, 6.0, 8.0);
        write_estimates(&home.join("new/case/base"), 7.0, 6.0, 8.0);
        clear_new(&home).unwrap();
        let found = collect(&home).unwrap();
        let file_kept = home.join("cold/new").is_file();
        fs::remove_dir_all(&home).unwrap();
        assert!(file_kept);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "cold/parse");
        assert_eq!(found[0].new, None);
        assert_eq!(found[0].base, Some(1000.0));
        assert_eq!(found[1].id, "new/case");
        assert_eq!(found[1].new, None);
        assert!(clear_new(&home).is_err());
    }

    #[test]
    fn a_malformed_estimate_is_an_error() {
        let home = std::env::temp_dir().join(format!("bench-compare-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let new = home.join("cold/parse/new");
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("estimates.json"), r#"{"mean":{}}"#).unwrap();
        let error = collect(&home).unwrap_err();
        fs::remove_dir_all(&home).unwrap();
        assert!(error.contains("/mean/point_estimate"), "{error}");
    }

    #[test]
    fn durations_pick_a_unit() {
        assert_eq!(duration(12.0), "12.000 ns");
        assert_eq!(duration(999.0), "999.000 ns");
        assert_eq!(duration(1_000.0), "1.000 µs");
        assert_eq!(duration(2_500_000.0), "2.500 ms");
        assert_eq!(duration(3_000_000_000.0), "3.000 s");
    }

    #[test]
    fn the_report_counts_regressions() {
        let comparisons = vec![
            comparison(Some((0.15, 0.01, 0.3))),
            comparison(Some((0.01, -0.02, 0.03))),
            comparison(None),
            Comparison {
                new: None,
                ..comparison(Some((0.5, 0.4, 0.6)))
            },
        ];
        let (text, regressions) = report(&comparisons, &Options::default());
        assert_eq!(regressions, 1);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].ends_with("REGRESSED"), "{text}");
        assert!(lines[0].contains("+15.0% [+1.0%, +30.0%]"), "{text}");
        assert!(lines[0].contains("100.000 ns"), "{text}");
        assert!(lines[1].ends_with("unchanged"), "{text}");
        assert!(lines[2].ends_with("new"), "{text}");
        assert!(lines[2].contains("  -  "), "{text}");
        assert!(lines[3].ends_with("removed"), "{text}");
        assert!(lines[3].contains("100.000 ns             -"), "{text}");
    }
}
