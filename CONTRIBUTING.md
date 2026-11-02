# Contributing to Mora

Everyone taking part in the project is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security problems privately, as described in [SECURITY.md](SECURITY.md), never in a public issue.

## Setup

The Cargo workspace, `rust-toolchain.toml` and `deny.toml` arrive with M0.2 ([#17](https://github.com/wolfware-labs/mora-lang/issues/17)). Until it is merged, the repository holds only documents, and the steps below apply from then on.

1. Install [rustup](https://rustup.rs).
2. Clone the repository. The toolchain version is pinned in `rust-toolchain.toml`, and `rustup` installs it with `rustfmt` and `clippy` the first time you run `cargo` in the repo.
3. Install `cargo-deny`:

   ```sh
   cargo install cargo-deny --locked
   ```

The design is in the [specification](docs/design/mora.md). Section 14.2 lists the crates and which crate may depend on which; `cargo xtask layers` enforces it.

## Checks

These are the checks CI runs on every pull request (§20.5 of the spec). They are added during milestone M0, so a check whose command does not exist yet is not required yet. Run them locally before pushing:

| Check | Command |
| --- | --- |
| Formatting | `cargo fmt --all --check` |
| Lints | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Tests | `cargo test --workspace --locked` |
| Docs | `cargo doc --workspace --no-deps --locked` with `RUSTDOCFLAGS=-D warnings` |
| Crate layers and target names (§14.2, §19.4) | `cargo xtask layers` |
| Dependency policy | `cargo deny check` |
| Spec and error-page examples | `cargo xtask spec-examples` |
| Generated syntax code (§14.4): `SyntaxKind` and the AST views, from `crates/mora-syntax/mora.ungram` and §3.4 | `cargo xtask codegen`; CI runs `cargo xtask codegen --check` |
| Test coverage (§20.5) | `cargo llvm-cov --workspace --open` after `cargo install cargo-llvm-cov` and `rustup component add llvm-tools`; CI uploads it to Codecov |
| Mutation testing (§20.5) | `git diff origin/main... > pr.diff && cargo mutants --in-diff pr.diff`, after `cargo install cargo-mutants`; CI runs it on every pull request, and a surviving mutant fails it |
| Parser fuzzing (§20.1) | `cargo +nightly fuzz run parse` in `fuzz/`, after `cargo install cargo-fuzz`; the nightly workflow runs it for 10 minutes from the parser test files |
| Error-code registry | `cargo xtask error-codes` |
| Runtime libraries (§13.2) | `cargo xtask runtime --test` for the host; CI builds and tests every hosted target. Set `CC` to a C compiler for the target, such as `musl-gcc` for `x86_64-unknown-linux-musl`, and `CFLAGS` to any flags it needs, such as `-arch x86_64` for `x86_64-apple-darwin` on an arm64 Mac |
| Linker and sysroots (§14.14) | `cargo xtask sysroot` installs the pinned `lld`, musl and MinGW-w64 pieces into `target/sysroot`; CI installs them on every host |
| Portability guards (§19.4) | `rustup target add thumbv7em-none-eabihf`, then `cargo build -p mora-rt-core --target thumbv7em-none-eabihf`. The run tests on `riscv64gc-linux` under QEMU join with M1.18 |

CI sets `RUSTFLAGS=-D warnings`, so a warning fails the build. Set the same flags locally: `export RUSTFLAGS="-D warnings" RUSTDOCFLAGS="-D warnings"` in a Unix shell, or `$env:RUSTFLAGS="-D warnings"` and `$env:RUSTDOCFLAGS="-D warnings"` in PowerShell.

## Benchmarks

The criterion benchmarks in `crates/*/benches/` time the compiler on generated projects: each query layer from a cold database, and the work after an edit (§14.3). Before opening a pull request that changes Rust code, compare it with `main`:

```sh
git fetch origin
cargo xtask bench-compare
```

It checks `origin/main` out under the target directory, runs every benchmark there and then on your working tree, and prints the change of each mean with its 95% confidence interval. A benchmark that is more than 10% slower, with the whole interval above zero, fails the command (§20.6). `--base <rev>` compares with another revision and `--threshold <percent>` changes the limit. Close other programs while it runs; the base build is kept in `target/bench-compare/base-target`, so later runs only rebuild what changed. A pull request that accepts a slowdown says why in its description. Run one suite directly with `cargo bench -p mora-hir --bench front_end`.

A pull request that changes only Markdown files skips the Rust build, except for `docs/design/mora.md`, `docs/errors/` and `docs/guide/`: they contain executable examples, so the example checks always run for them.

## Spec first

The specification, its examples and the golden files under `tests/` define how Mora behaves. A pull request that changes or settles behaviour updates `docs/design/mora.md` in the same pull request, before or with the code. If you find yourself deciding behaviour in code, write it into the spec first.

## Code rules

- `unsafe` Rust is allowed only in `mora-rt-core`, `mora-rt`, `mora-codegen` (calling JIT-compiled code) and `mora-macro` (running macros). Every `unsafe` block carries a `// SAFETY:` comment.
- `unwrap()` and `expect()` are denied in product code. Tests, benchmarks, `mora-testkit` and `xtask` may use them. An `expect` in product code that encodes an invariant needs `#[allow(clippy::expect_used)]` and a comment stating the invariant.
- No code outside `mora-target`, `mora-link` and the backends may branch on a target name, and no code outside `mora-target` may assume a pointer width or byte order (§19.4).
- Every compiler diagnostic has a registered `MR` code and a page in `docs/errors/` (§17.6).

## Dependencies

`deny.toml` defines the dependency policy. Third-party dependencies must use one of the allowed licenses, come from crates.io, and have no open security advisories. Workspace crates depend on each other through `path` dependencies. A new dependency that fails `cargo deny check` needs a discussion in its pull request before the policy changes.

## Issues, milestones and labels

The [roadmap](ROADMAP.md) splits the work into milestones M0 to M13; M0 to M7 are the MVP. Each MVP deliverable has an ID such as `M1.4` and one issue, and each milestone has a tracking issue that lists its deliverables. An issue's "blocked by" field lists what has to land first.

Every issue has these labels:

| Prefix | Meaning | Example |
| --- | --- | --- |
| `kind/` | Kind of work: `feature`, `bug`, `test`, `docs`, `infra`, `spec`, `perf`, `security`, `tracking` | `kind/feature` |
| `area/` | Part of the code the work touches | `area/mir` |
| `platform/` | Only when the work is specific to an OS | `platform/windows` |

Reports from the issue forms also get `needs-triage` until a maintainer reviews them.

## Pull requests

- Keep one issue per pull request, and put `Closes #<number>` in the description.
- Write the title as a conventional commit with the roadmap ID at the end (see below). Pull requests are squash merged, and the title becomes the commit on `main`.
- Name the branch after the deliverable, for example `m1.4/parser-expressions`.
- Keep unrelated changes out, including drive-by formatting.
- Add or update tests for the behaviour you change.
- Every commit must be signed.
- Make sure every check above passes.

## Commit messages

Pull request titles and commits follow [Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<scope>): <summary> (<roadmap ID>)
```

- `type` is one of `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `style` or `chore`.
- `scope` is the crate without the `mora-` prefix (`syntax`, `mir`, `rt`), or `std`, `spec` or `ci`, and can be left out for repository-wide changes.
- The summary is in the imperative mood ("add", "fix", "remove"), lowercase, at most 72 characters, with no trailing period.
- Add a body after a blank line when the change needs explaining: what changed and why, wrapped at 72 characters.

```
feat(syntax): lex interpolated strings with nested holes (M1.1)

Holes can contain string literals that contain holes, so the lexer
keeps a mode stack inside strings instead of using one logos pattern.
```

## Contributor Licence Agreement

Every contributor signs the [Contributor Licence Agreement](CLA.md) once. You keep the copyright to your contribution, and Wolfware LLC can keep distributing the project, including under other licences.

When you open your first pull request, a bot comments with a link to the agreement. Reply with the sentence it asks for, and the `CLA` check passes. A pull request is merged only after every author has signed.

## License

By contributing, you agree that your contributions are dual licensed under the MIT and Apache 2.0 licenses, as described in the [README](README.md#license), and covered by the [Contributor Licence Agreement](CLA.md).
