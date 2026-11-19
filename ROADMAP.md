# Mora roadmap

Companion to [`docs/design/mora.md`](docs/design/mora.md). The spec defines what Mora is; this roadmap orders the work, states what each milestone delivers and how each deliverable is proven.

## How to use this document

- Work milestones in order. M0 builds the foundations, M1 is a walking skeleton on all three operating systems, and each later milestone deepens one area.
- Every deliverable has an ID (`M2.4`) used in its issue title, its branch name and the subject of its commit. Spec references look like §10.4 and link to the spec.
- One deliverable is one issue and one pull request, with its code and its tests.
- **Blocked by** lists direct prerequisites only, and never a later deliverable.
- A milestone is done only when its exit criteria pass in CI.
- Spec first: a behaviour change updates `docs/design/mora.md` in the same pull request, before or with the code.
- `[verify]` marks third-party behaviour that a test or a manual check must confirm before code relies on it.

## Release lines

| Line | Milestones | Meaning |
|---|---|---|
| **MVP (v0.1)** | M0 to M7 | The REST API of §2.2 builds into a native executable on Linux, macOS and Windows, with editor support and installers |
| **v0.2** | M8, M9, M16 | Libraries and packages from URL and git sources, with capability grants, semantic diff and semver checks (contracts join in v0.3), and the rest of the agent toolchain |
| **v0.3** | M14, M15, M17 | Contracts and property tests, the world simulator, record and replay, hot reload, and standard library growth |
| **v0.4** | M10, M11, M18 | Threads, language growth and the optimizer passes left after the MVP |
| **v0.5** | M12, M13 | The package registry with registry dependencies and signing, the LLVM backend and more targets |

Post-MVP milestones are broken into deliverables when each one starts, after its spec sections are updated.

## M0: Foundations

**Goal.** Repository, CI on Linux, macOS and Windows, workspace crates and layering check, spans and diagnostics, error-code registry, test harnesses, and a runtime skeleton built for every Tier 1 target.

**Exit criteria.** CI is green on all three OSes; the UI, run and spec-example harnesses run an empty suite; `cargo xtask layers` enforces §14.2.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M0.1](#m01-repository-governance-and-templates) | Repository governance and templates | `ci` | none |
| [M0.2](#m02-cargo-workspace-and-lint-configuration) | Cargo workspace and lint configuration | `ci` | M0.1 |
| [M0.3](#m03-ci-workflow-on-linux-macos-and-windows) | CI workflow on Linux, macOS and Windows | `ci` | M0.2 |
| [M0.4](#m04-crate-layering-check) | Crate layering check | `ci` | M0.2 |
| [M0.5](#m05-spans-files-and-the-analysis-database) | Spans, files and the analysis database | `driver` | M0.2 |
| [M0.6](#m06-diagnostics-and-rendering) | Diagnostics and rendering | `driver` | M0.5 |
| [M0.7](#m07-error-code-registry) | Error-code registry | `docs` | M0.6 |
| [M0.8](#m08-ui-and-run-test-harnesses) | UI and run test harnesses | `ci` | M0.6 |
| [M0.9](#m09-executable-spec-examples) | Executable spec examples | `docs` | M0.8 |
| [M0.10](#m010-runtime-skeleton-portable-core-and-hosted-layer) | Runtime skeleton: portable core and hosted layer | `rt` | M0.3 |
| [M0.11](#m011-bundled-linkers-and-sysroots-for-development) | Bundled linkers and sysroots for development | `release` | M0.10 |
| [M0.12](#m012-cli-skeleton) | CLI skeleton | `cli` | M0.6 |
| [M0.13](#m013-target-descriptions) | Target descriptions | `driver` | M0.2 |

### M0.1 Repository governance and templates

Spec: [§21](docs/design/mora.md#21-project-conventions). Crates: repository. Blocked by: none.

Public-repository files: README with status, MIT and Apache-2.0 licenses, NOTICE, CODE_OF_CONDUCT, SECURITY, CONTRIBUTING (checks, commit format with roadmap IDs, signed commits, squash merge, spec-first rule), CLA and the CLA workflow, issue forms, pull request template, CODEOWNERS, Dependabot for Cargo and Actions, and the label set of §21.

Done when:

- The CLA check runs on a test pull request from a second account and passes after signing.
- Issue forms add `needs-triage`; the pull request template asks for the roadmap ID and `Closes #`.
- Every label of §21 exists in the repository.

### M0.2 Cargo workspace and lint configuration

Spec: [§14.2](docs/design/mora.md#142-crates), [§21](docs/design/mora.md#21-project-conventions). Crates: `all`. Blocked by: M0.1.

Workspace `Cargo.toml` with every crate of §14.2 as an empty library (`mora-cli` as a binary, `mora-rt` as `staticlib` plus `rlib`), workspace dependency versions pinned as §14.2 lists them, `rust-toolchain.toml` at 1.99.1, `clippy.toml`, `rustfmt.toml`, `deny.toml` (licenses, sources, advisories) and `.cargo/config.toml` with the `xtask` alias.

Done when:

- `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo deny check` pass.
- Each crate's `lib.rs` has a crate doc comment stating its responsibility from §14.2.

### M0.3 CI workflow on Linux, macOS and Windows

Spec: [§20.5](docs/design/mora.md#205-continuous-integration), [§19.1](docs/design/mora.md#191-targets). Crates: repository. Blocked by: M0.2.

`ci.yml` running fmt, clippy, tests, docs and `cargo deny` on `ubuntu-latest`, `ubuntu-24.04-arm`, `macos-latest`, `windows-2025` and `windows-11-arm`, with a change filter that skips the Rust build for Markdown-only changes but never for `docs/design/mora.md`, `docs/errors/` or `docs/guide/`, whose executable examples must run, `!cancelled()` so one run reports every failing check, `RUSTFLAGS=-D warnings`, and every third-party action pinned by commit SHA.

Done when:

- A pull request shows one required check per job and they pass.
- A change to `README.md` alone completes without building; a change to `docs/design/mora.md` alone is not skipped.
- A repository ruleset on `main` requires the `ci-ok` job, which passes only when every other job passed or was skipped by the change filter.

### M0.4 Crate layering check

Spec: [§14.2](docs/design/mora.md#142-crates). Crates: `xtask`. Blocked by: M0.2.

`cargo xtask layers` reads `cargo metadata` and fails when a workspace crate depends on a crate in the same or a higher layer of §14.2, or when `mora-rt` depends on any workspace crate.

Done when:

- A test fixture with an upward dependency makes the check fail with the offending edge in the message.
- The check runs in CI.

### M0.5 Spans, files and the analysis database

Spec: [§14.3](docs/design/mora.md#143-the-analysis-database). Crates: `mora-span`. Blocked by: M0.2.

`FileId`, `Span`, `TextRange`, a `LineIndex` that converts offsets to line and column in UTF-8 bytes and in UTF-16 code units (for the LSP), and the base `salsa` database with file-text and project inputs.

Done when:

- Round-trip tests between offsets and both column encodings on ASCII, multi-byte and astral-plane text, including CRLF lines.
- A `salsa` input change invalidates exactly the dependent test query.

### M0.6 Diagnostics and rendering

Spec: [§17.6](docs/design/mora.md#176-diagnostics). Crates: `mora-diag`. Blocked by: M0.5.

The one `Diagnostic` type of §17.6: code, severity, primary and secondary labeled spans, message, note, fixes as text edits. Terminal rendering with `annotate-snippets` in the layout §17.6 shows, and `--message-format json` output with one object per line.

Done when:

- A golden test renders the `MR0310` example of §17.6 byte for byte (colors off).
- JSON output round-trips through `serde_json` with every field of §17.6.

### M0.7 Error-code registry

Spec: [§17.6](docs/design/mora.md#176-diagnostics), [§20.4](docs/design/mora.md#204-executable-examples). Crates: `mora-diag`, `xtask`. Blocked by: M0.6.

A single registry of every `MR` code with its range from §17.6, a `docs/errors/MRnnnn.md` page template (explanation, failing example, fixed example), and `cargo xtask error-codes`, which fails when a code is used without a page, a page has no code, or a code sits outside its range.

Done when:

- The check fails on a fixture with an unregistered code.
- `docs/errors/README.md` explains how to add a code.
- The `linux` job in `ci.yml` runs `cargo xtask error-codes`, so it also runs for changes to `docs/errors/` (§20.5).

### M0.8 UI and run test harnesses

Spec: [§20.1](docs/design/mora.md#201-test-layers). Crates: `mora-testkit`. Blocked by: M0.6.

Harnesses for `tests/ui` (`//~ ERROR MRnnnn` annotations on lines, rendered-output snapshots, `--bless` to update) and `tests/run` (`// expect:` stdout, `// exit:` code, run through JIT and AOT once those exist, outputs compared). Each harness discovers files, runs them in parallel and reports failures with a diff.

Done when:

- Self-tests with a fake compiler cover a missing annotation, an unexpected error, a stdout mismatch and an exit-code mismatch.
- `cargo test` runs both harnesses.

### M0.9 Executable spec examples

Spec: [§1.6](docs/design/mora.md#16-how-to-read-this-document), [§20.4](docs/design/mora.md#204-executable-examples). Crates: `xtask`. Blocked by: M0.8.

`cargo xtask spec-examples` extracts every `mora` code block in `docs/design/mora.md` and `docs/errors/*.md` whose first line is `// expect:` or `// error:` and runs it through the run or UI harness. Examples for features not yet built are listed in `tests/spec-examples.pending` with the deliverable that will enable them; the check fails when a pending example starts passing, so the list only shrinks.

Done when:

- Every example in the spec is either run or listed as pending with a deliverable ID.
- The `linux` job in `ci.yml` runs `cargo xtask spec-examples`, so it also runs for changes that touch only `docs/design/mora.md`, `docs/errors/` or `docs/guide/` (§20.5).

### M0.10 Runtime skeleton: portable core and hosted layer

Spec: [§13.1](docs/design/mora.md#131-the-runtime-library), [§13.2](docs/design/mora.md#132-two-builds-of-the-runtime), [§19.1](docs/design/mora.md#191-targets), [§19.4](docs/design/mora.md#194-portability). Crates: `mora-rt-core`, `mora-rt`. Blocked by: M0.3.

`mora-rt-core` as a `#![no_std]` crate that reaches the platform only through the `mora_pal_*` functions of §19.4, and `mora-rt` as the hosted layer implementing them with the Rust standard library. CI builds `mora-rt-core` for `thumbv7em-none-eabihf`. `mora-rt` is built as a static library that bundles `mora-rt-core`, in release and debug variants, for `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm`. Exposes a stub entry point that calls an extern `__mora_main` and returns its exit code. CI builds every variant.

Done when:

- A C test program linked against each Linux and Windows variant on its native runner exits with the code `__mora_main` returns.
- Both variants build on every Tier 1 runner.
- The `thumbv7em-none-eabihf` build of `mora-rt-core` is a required CI check.

### M0.11 Bundled linkers and sysroots for development

Spec: [§14.14](docs/design/mora.md#1414-linking), [§19.2](docs/design/mora.md#192-toolchain-archive). Crates: `xtask`, `mora-link`. Blocked by: M0.10.

`cargo xtask sysroot` downloads pinned `lld` binaries, the musl sysroot and the llvm-mingw pieces by URL and SHA-256 into `target/sysroot`, laid out as `lib/` of §19.2, so development builds link exactly as release toolchains do. Records each component's license in `LICENSES/`.

Done when:

- The download verifies hashes and refuses a mismatch.
- License review recorded in the pull request for lld, musl and mingw-w64 [verify].

### M0.12 CLI skeleton

Spec: [§17.1](docs/design/mora.md#171-commands). Crates: `mora-cli`. Blocked by: M0.6.

`mora` with every MVP subcommand of §17.1 registered (unimplemented ones print `not implemented yet` and exit 2), `mora version`, `--color`, `--message-format`, and the exit codes of §17.1.

Done when:

- CLI tests check `mora --help`, `mora version` and the exit code of an unknown subcommand.

### M0.13 Target descriptions

Spec: [§19.4](docs/design/mora.md#194-portability), [§17.2](docs/design/mora.md#172-manifest). Crates: `mora-target`, `targets`. Blocked by: M0.2.

The `mora-target` crate and `targets/*.toml` for every target of §19.1: triple, pointer width, endianness, alignments, object format, linker flavor, runtime layer, backend and platform capabilities, the `@Cfg` values derived from them, and loading a custom description from a path. A 32-bit test description (`tests/targets/test32.toml`) for layout tests.

Done when:

- Every built-in description parses and round-trips; an unknown key is an error.
- `cargo xtask layers` forbids target-name matching outside `mora-target`, `mora-link` and `mora-codegen` (a grep-based check with an allow list).

## M1: Hello, native

**Goal.** Parse every construct of the language, type-check the subset needed for functions, structs, primitives, strings and control flow, and produce a native executable for §2.1 on every Tier 1 target.

**Exit criteria.** §2.1 builds and runs on every Tier 1 target in CI; the M1 run tests pass through JIT and AOT.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M1.1](#m11-lexer) | Lexer | `syntax` | M0.5, M0.6 |
| [M1.2](#m12-syntax-tree-and-typed-ast) | Syntax tree and typed AST | `syntax` | M1.1 |
| [M1.3](#m13-parser-items-and-declarations) | Parser: items and declarations | `syntax` | M1.2 |
| [M1.4](#m14-parser-statements-and-expressions) | Parser: statements and expressions | `syntax` | M1.3 |
| [M1.5](#m15-withdrawn) | Withdrawn | `syntax` | M1.4, M0.7 |
| [M1.6](#m16-item-tree-name-resolution-and-entry-points) | Item tree, name resolution and entry points | `hir` | M1.3 |
| [M1.7](#m17-types-and-signatures) | Types and signatures | `hir` | M1.6 |
| [M1.8](#m18-body-lowering-to-hir) | Body lowering to HIR | `hir` | M1.7 |
| [M1.9](#m19-type-checking-core) | Type checking: core | `hir` | M1.8 |
| [M1.10](#m110-mir-and-lowering-from-hir) | MIR and lowering from HIR | `mir` | M1.9 |
| [M1.11](#m111-constant-evaluation) | Constant evaluation | `mir` | M1.10 |
| [M1.12](#m112-cranelift-code-generation) | Cranelift code generation | `codegen` | M1.10, M0.13 |
| [M1.13](#m113-runtime-startup-output-and-panics) | Runtime: startup, output and panics | `rt` | M0.10 |
| [M1.14](#m114-linking-on-linux-macos-and-windows) | Linking on Linux, macOS and Windows | `link` | M0.11, M1.12, M1.13 |
| [M1.15](#m115-standard-library-bootstrap-and-console-output) | Standard library bootstrap and console output | `std` | M1.9, M1.13 |
| [M1.16](#m116-driver-and-the-build-run-and-check-commands) | Driver and the build, run and check commands | `driver` | M1.14, M1.15 |
| [M1.17](#m117-debug-information-and-symbol-names) | Debug information and symbol names | `codegen` | M1.12 |
| [M1.18](#m118-third-instruction-set-under-emulation) | Third instruction set under emulation | `ci` | M1.16 |
| [M1.19](#m119-grammar-driven-parser-fuzzing) | Grammar-driven parser fuzzing | `syntax` | M1.4 |
| [M1.20](#m120-fix-applicability-mora-fix-and-watch-mode) | Fix applicability, `mora fix` and watch mode | `driver` | M1.16 |
| [M1.21](#m121-error-recovery-that-keeps-types) | Error recovery that keeps types | `check` | M1.9 |

### M1.1 Lexer

Spec: [§3](docs/design/mora.md#3-source-text-and-lexical-structure). Crates: `mora-syntax`. Blocked by: M0.5, M0.6.

Tokens and trivia for the whole language: identifiers with `@` and NFC normalization, every keyword and contextual keyword of §3.4, integer, real, character and every string form of §3.5 (regular, verbatim, raw, interpolated with nested holes, `u8`), comments and doc comments, `#region`, and the operators of §3.6. `logos` for ordinary tokens, a hand-written sub-lexer with a mode stack inside strings.

Done when:

- Table tests for every literal form and escape, including `MR0001` and `MR0010`.
- Concatenating every token's text reproduces the input for every file in `tests/` (fuzzed in M1.4).

### M1.2 Syntax tree and typed AST

Spec: [§14.4](docs/design/mora.md#144-lexer-and-parser). Crates: `mora-syntax`, `xtask`. Blocked by: M1.1.

Syntax kinds, the `rowan` green and red trees behind `mora-syntax`'s own API, and typed AST views generated by `cargo xtask codegen` from a grammar file `crates/mora-syntax/mora.ungram` that mirrors the EBNF of the spec.

Done when:

- `cargo xtask codegen --check` fails when the generated code is stale.
- Unit tests navigate the AST of hand-built trees.

### M1.3 Parser: items and declarations

Spec: [§4](docs/design/mora.md#4-program-structure), [§6](docs/design/mora.md#6-declarations), [§9.2](docs/design/mora.md#92-declaring-macros). Crates: `mora-syntax`. Blocked by: M1.2.

Recursive-descent parsing of compilation units: using directives, attributes, every type declaration (struct and positional struct, class, unique class, union, enum, interface, `ref struct`, `macro`), extension blocks, members (fields with `mut(...)` write scopes, `=>` and `stored` getters, indexers, methods with `uses` bounds, constructors including primary constructors, destructors, operators, conversions), modifiers, type parameters and constraints, with recovery at member and item boundaries.

Done when:

- Snapshot tests of the tree for each declaration form of §6.
- Error recovery tests: a missing brace or semicolon in one member still yields trees for the following members.
- Accessor bodies on fields and getters (`{ get; }`, `{ get; set; }`) are syntax errors that recover at the next member.

### M1.4 Parser: statements and expressions

Spec: [§7](docs/design/mora.md#7-expressions-and-statements), [§14.4](docs/design/mora.md#144-lexer-and-parser). Crates: `mora-syntax`, `fuzz`. Blocked by: M1.3.

Statements of §7.11, Pratt parsing with the precedence table of §7.1, the postfix `?` disambiguation and `await e?` rule of §7.4, generic argument disambiguation and cast rules of §14.4, lambdas with capture lists, patterns of §7.8, `match` expressions and statements, collection expressions, interpolation holes, `with`, `code { }` quotes and `#Name(...)` macro calls. A `cargo fuzz` target checks that parsing never panics and is lossless.

Done when:

- Snapshot tests for each row of §7.1 and each pattern of §7.8.
- `a ? b : c`, `x?`, `x?.y`, `await f()?` and `F<A, B>(c)` parse as §7.4 and §14.4 say.
- The fuzz target runs for 10 minutes in the nightly job without failures.

### M1.5 Withdrawn

Spec: [§3.4](docs/design/mora.md#34-keywords), [§23](docs/design/mora.md#23-decisions-register). Crates: `mora-syntax`, `mora-diag`. Blocked by: M1.4, M0.7.

Withdrawn by D-59: the compiler knows only Mora syntax, so code in another language's syntax gets the ordinary syntax error. The codes this deliverable added were removed.

### M1.6 Item tree, name resolution and entry points

Spec: [§4](docs/design/mora.md#4-program-structure), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`, `mora-syntax`. Blocked by: M1.3.

Per-file item trees and the project def map: namespaces from folders with `MR0121` (D-96), nested types, `using`, `using static`, aliases, `global using`, the implicit prelude, the folder-default visibility of §4.5 (D-97), dependency names for path dependencies (resolved in M3.14), and entry point detection with `MR0124` and `MR0125`. The paths of `using` directives are resolved here, with `MR0127`, `MR0128` and `MR0129`; names in signatures and bodies use the same lookup from M1.7 on.

Done when:

- UI tests for unknown names (`MR0127`), ambiguous names (`MR0128`), inaccessible items across folders (`MR0129`), folder names (`MR0121`) and both entry-point errors (`MR0124`, `MR0125`).
- A `salsa` test shows that editing a function body does not recompute the def map.

### M1.7 Types and signatures

Spec: [§4.5](docs/design/mora.md#45-visibility), [§5.1](docs/design/mora.md#51-kinds-of-type), [§5.2](docs/design/mora.md#52-primitive-types), [§5.5](docs/design/mora.md#55-tuples), [§6.2](docs/design/mora.md#62-structs). Crates: `mora-hir`. Blocked by: M1.6.

The type representation for primitives, `string`, unit, tuples, structs and function signatures (generic and class types arrive in M2 and M3), lowering of syntax types to types, and the signature query per item, which checks that a signature mentions no type less visible than its member (`MR0123`).

Done when:

- Unit tests resolve each primitive keyword and tuple form to its type.
- Unknown type names report an error with a suggestion of the closest name.
- A UI test for a signature that mentions a type less visible than the member (`MR0123`).

### M1.8 Body lowering to HIR

Spec: [§7](docs/design/mora.md#7-expressions-and-statements), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`. Blocked by: M1.7.

Lowering of function bodies and top-level statements to HIR expressions and statements, with scopes, local declarations of §7.10, shadowing check `MR0180`, and name resolution of locals, parameters, members and items.

Done when:

- UI tests for `MR0180` and unresolved names.
- HIR dumps of the M1 run tests are snapshot-tested.

### M1.9 Type checking: core

Spec: [§5.10](docs/design/mora.md#510-conversions), [§7](docs/design/mora.md#7-expressions-and-statements), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`, `mora-check`. Blocked by: M1.8.

Bidirectional checking of literals (target typing of §3.5), numeric conversions and promotions of §5.10 and §7.12, operators, calls with overload resolution and named and optional arguments, `let`/`var` locals and `MR0182`, `if`, `while`, `do`, `for`, `break` and `continue` (`MR0183`), `return` and functions that can end without a value (`MR0184`), and definite assignment (`MR0181`).

Done when:

- UI tests for each numeric conversion row of §5.10, the explicit numeric cast, the rule that nothing converts implicitly between `bool` and integers, between `char` and integers or to `string`, and each error code above.
- UI tests for overload resolution, named and optional arguments, argument markers and each of `MR0205` to `MR0208`.
- Run tests for integer promotion and float target typing (`let x: float = 1.5;`).

### M1.10 MIR and lowering from HIR

Spec: [§14.7](docs/design/mora.md#147-mir). Crates: `mora-mir`. Blocked by: M1.9.

The MIR of §14.7 (bodies, blocks, places, operands, rvalues, statements, terminators), the stable textual dump behind `--emit mir`, and lowering of the M1 subset from HIR.

Done when:

- MIR snapshot tests for each M1 run test.
- A MIR validator (types match, every block terminated) runs after every pass in debug builds of the compiler.

### M1.11 Constant evaluation

Spec: [§4.7](docs/design/mora.md#47-static-state), [§14.6](docs/design/mora.md#146-constant-evaluation). Crates: `mora-mir`. Blocked by: M1.10.

A MIR interpreter for `const` values and default parameter values with the subset of §14.6 and `MR0460` outside it.

Done when:

- Run tests using constants used as array sizes (the evaluated size; `new` itself lowers in M2.3), defaults and string concatenation.
- A UI test for `MR0460`.

### M1.12 Cranelift code generation

Spec: [§13.5](docs/design/mora.md#135-value-layouts), [§13.7](docs/design/mora.md#137-calling-convention-and-symbols), [§14.13](docs/design/mora.md#1413-cranelift-lowering). Crates: `mora-codegen`. Blocked by: M1.10, M0.13.

A `Backend` trait taking optimized MIR and a target description, and its Cranelift implementation: lowering of MIR for scalars, structs, tuples, calls, control flow, overflow-checked arithmetic and panics as `noreturn` runtime calls; object files through `cranelift-object`; in-process execution through `cranelift-jit`; struct layout of §13.5 computed from the target description and the calling convention of §13.7. To settle in the spec: §13.10 says how a frame larger than the guard page is probed, and §13.5 gives the largest size a type may have and the error for one that exceeds it.

Done when:

- The run harness executes every M1 run test through the JIT and through an object file linked by M1.14, and compares outputs.
- Overflow in a debug build panics with the message of §11.4.
- Run tests in both profiles, through the JIT and the object file: division and remainder by zero, `int.MinValue / -1` and `int.MinValue % -1` panic with the message of §11.4 and exit code 101, never with a CPU fault; a shift count of the type's width or more is masked (§7.12); a float-to-int cast of NaN, an infinity and an out-of-range value saturates (§5.10).
- A function whose frame is larger than the guard page touches every page of it on entry (stack probes): a run test calls one with a multi-megabyte struct local near the stack limit and gets `stack overflow` (§13.10) on every Tier 1 target, with M1.13.
- Layout arithmetic is checked: a struct or tuple whose size does not fit the target's address space is reported as an error, never wrapped (unit test against the 32-bit test description).

### M1.13 Runtime: startup, output and panics

Spec: [§13.1](docs/design/mora.md#131-the-runtime-library), [§13.8](docs/design/mora.md#138-startup-and-process-exit), [§11.4](docs/design/mora.md#114-panics). Crates: `mora-rt`. Blocked by: M0.10.

Process entry, argument collection, buffered stdout and stderr, panic message formatting (`panic at file:line:col: message`), backtraces when `MORA_BACKTRACE=1` or in the debug runtime, abort with exit code 101, stack overflow detection of §13.10, and immortal string literal objects. To settle in the spec: what the program receives for a process argument that is not valid UTF-8 (§13.8, §15.6).

Done when:

- Run tests for exit codes 0, an `int` from `Main`, and 101 on panic.
- A deep-recursion test prints `stack overflow` on every Tier 1 target.
- The stack-overflow handler does not allocate, and on Unix targets it runs on an alternate signal stack (§13.10); the large-frame test of M1.12 prints `stack overflow` too.
- Runtime tests: an allocation the platform refuses panics with `out of memory allocating N bytes` and never returns a null pointer to compiled code; a second panic raised while the first is being printed aborts at once (§11.4).
- Process arguments that are not valid UTF-8 never become a `string` holding invalid UTF-8 (§13.4); the spec states what the program receives instead.

### M1.14 Linking on Linux, macOS and Windows

Spec: [§14.14](docs/design/mora.md#1414-linking). Crates: `mora-link`. Blocked by: M0.11, M1.12, M1.13.

The linker driver of §14.14: `ld.lld` with musl for a fully static Linux executable, `xcrun ld` with `libSystem` on macOS, `lld-link` with MinGW pieces on Windows, an 8 MiB main-thread stack on Windows, and a clear error with the install command when a required tool is missing. To settle in the spec: whether Linux executables are position-independent (static PIE), the non-executable stack and read-only relocations, and the matching `lld-link` options (§14.14 gives none today).

Done when:

- Hello world links and runs on every Tier 1 runner.
- On Linux, `file` reports the executable as statically linked.
- Removing the linker from `PATH` on macOS yields the `xcode-select --install` hint.
- On Linux and Windows the linker, the start files and the libraries are taken from the toolchain directory by absolute path. A test puts a decoy `ld.lld` (`lld-link.exe` on Windows) first on `PATH` and in the project directory and shows it is not run; on macOS `xcrun` is started as `/usr/bin/xcrun`.
- Every linker argument is passed as one argument, never through a shell, and each path or name taken from the project is passed so that it cannot be read as an option or split in a response file: a test links a project whose folder name starts with `-` and contains a space and a quote.

### M1.15 Standard library bootstrap and console output

Spec: [§15.1](docs/design/mora.md#151-prelude-std), [§15.6](docs/design/mora.md#156-console-environment-and-time), [§14.15](docs/design/mora.md#1415-standard-library-compilation). Crates: `std`, `mora-driver`. Blocked by: M1.9, M1.13.

The `std/` source tree embedded in the toolchain, the prelude as an implicit `global using Std;`, `Console.Write` and `Console.WriteLine` for primitives and `string`, `IDisplay` for primitives, and interpolated strings lowered to `Formatter` writes, without format specifiers yet. To settle in the spec: the error for a project that takes the root namespace `Std` (§4.2, §4.4).

Done when:

- §2.1 prints `Hello, world!`.
- Run tests interpolate every primitive type.
- MIR has no `External` callee and no `Interpolate` rvalue, and diverging calls are known from their declarations: the M1.10 stand-ins are removed.
- The strings run test prints `name.Length`.
- "Compiling `Std`" means the source tree embedded in the toolchain and nothing else. UI tests: a project whose root namespace resolves to `Std` or to a name that starts with `Std.` (by its `namespace` key, or by the default taken from a project named `std`, §4.2) is rejected with the code the spec assigns, and `unsafe`, `RawPtr<T>` and `@Cheap` in a user project are `MR0107` and `MR0254` whatever the project is called.

### M1.16 Driver and the build, run and check commands

Spec: [§14.15](docs/design/mora.md#1415-standard-library-compilation), [§17.1](docs/design/mora.md#171-commands), [§17.2](docs/design/mora.md#172-manifest), [§17.3](docs/design/mora.md#173-project-layout), [§17.4](docs/design/mora.md#174-profiles). Crates: `mora-driver`, `mora-pkg`, `mora-cli`. Blocked by: M1.14, M1.15.

Reading `mora.toml` (the `[project]` keys and profiles; unknown keys are `MR0500`), discovering `src/**/*.mr` and rejecting files of 4 GiB or more with `MR0003` (§3.1), the dev and release profiles of §17.4, the `target/` layout, the on-disk cache of the standard library's checked MIR, and `mora build`, `mora run` and `mora check`. To settle in the spec: where the cache of `Std` lives and what makes an entry valid (§14.15), the error codes for an invalid `name` and `entry` (§17.2), and whether discovery follows symbolic links (§17.3).

Done when:

- CLI tests build and run a temporary hello-world project in both profiles.
- A second build reuses the standard-library cache (asserted through a cache-hit counter in `--verbose` output).
- A construct the compiler cannot lower yet reports an error that names the milestone bringing it, never an internal error; `F(xs: int[])` is one such case.
- A build of several projects reports a diagnostic once when two of them report the same one, as two sibling projects that run one library field initializer do (§14.6).
- A cache entry is never trusted because it is there, and a hash stored beside it does not make it trusted (§14.15 then states the check): a test replaces, truncates and bit-flips the files under `target/mora/cache/` and the build either rebuilds them or fails with an error, never loads them, and reading a damaged entry never panics.
- CLI tests: a `[project] name` outside lowercase letters, digits and `-`, or one that starts with `-`, and an `entry` that is absolute or leaves the project folder through `..`, are errors before any file is written; nothing is ever written outside the project's `target/`.
- Source discovery reads only regular files below `src/`: a special file such as a FIFO named `X.mr` is never opened and is reported with a diagnostic. Tests with such a FIFO and with a link cycle end without hanging, and a symbolic link that leaves the project follows the rule the spec gives (§17.3); the 4 GiB check of `MR0003` reads the size before the content.
- A text that is not valid UTF-8 is `MR0001` and a text of 4 GiB or more is `MR0003`, both reported before any lexing, through one checked entry point that the driver, the language server and the test kit all use; `LineIndex::new` returns an error instead of panicking on an oversized text.

### M1.17 Debug information and symbol names

Spec: [§13.7](docs/design/mora.md#137-calling-convention-and-symbols), [§14.13](docs/design/mora.md#1413-cranelift-lowering). Crates: `mora-codegen`, `mora-cli`. Blocked by: M1.12.

The mangling scheme of §13.7, `mora demangle`, demangled names in backtraces, and DWARF line tables from MIR spans on Linux and macOS. To settle in the spec: what in a mangled name tells two projects with one root namespace apart, the hash and its width, and which `@Export` names are refused (§13.7).

Done when:

- A panic backtrace in a debug build shows `UsersApi.Program.Main` style names with `src/Program.mr:LINE`.
- `gdb` or `lldb` stops at a breakpoint set by `file:line` (manual check recorded in the pull request).
- Symbols are unique per build, following the project-identity rule the spec then gives for mangled names (§13.7, §4.4): a test with two path dependencies that have the same root namespace and declare the same function path links and calls each one correctly, and code generation fails with an internal error, never emits both, when two different bodies get one symbol.
- `mora demangle` and the backtrace printer return a symbol unchanged when it is not a valid mangled name: a property test feeds lengths past the end, lengths that overflow, non-UTF-8 bytes and empty segments, with no panic and no read past the input. The printer does not allocate after a stack overflow or an out-of-memory panic.
- A `@Export` name is checked against the names the runtime and the C library define; see the spec.

### M1.18 Third instruction set under emulation

Spec: [§19.1](docs/design/mora.md#191-targets), [§19.4](docs/design/mora.md#194-portability). Crates: `targets`, `.github/workflows`. Blocked by: M1.16.

The `riscv64gc-linux` target description, `mora-rt` and the musl sysroot for it, and a CI job that cross-builds the run-test suite and runs it under QEMU user-mode emulation.

Done when:

- The run tests pass on `riscv64gc-linux` under QEMU as a required check.
- The `riscv64gc-linux` sysroot is fetched through the pinned table of M0.11 with its SHA-256, and QEMU comes from the runner image's package manager at a named version; the job downloads nothing else.

### M1.19 Grammar-driven parser fuzzing

Spec: [§14.4](docs/design/mora.md#144-lexer-and-parser), [§20.1](docs/design/mora.md#201-test-layers). Crates: `mora-syntax`, `fuzz`. Blocked by: M1.4.

A generator that writes programs from `mora.ungram`, adding parentheses where the rules of §14.4 and §7.4 would read the text another way, and a `cargo fuzz` target that parses each program, compares the tree with the shape it was generated from and prints it back (D-115).

Done when:

- The target runs for 10 minutes in the nightly job without failures.
- A unit test with a parser rule that disagrees with `mora.ungram` shows the check reporting the mismatch.
- The nightly fuzz jobs pass `-max_len=65536` or more and start from seeds that nest deeply, so inputs long enough to reach depth and size bugs are generated.
- The parse fuzz target asserts that the nesting depth it counts never exceeds the bound of §3.1 and §14.4, and that no node of the tree sits deeper than a second fixed bound, larger than the first because wrapper, name and list nodes add levels, which the crate documents and the recursive walks over the tree are sized for.

### M1.20 Fix applicability, `mora fix` and watch mode

Spec: [§17.1](docs/design/mora.md#171-commands), [§17.6](docs/design/mora.md#176-diagnostics), [§17.10](docs/design/mora.md#1710-tools-for-programs-and-agents). Crates: `mora-diag`, `mora-driver`, `mora-cli`. Blocked by: M1.16.

`applicability` on every fix and `schema` on every JSON object (D-113), the deterministic order of diagnostics, `mora fix` with `--code` and `--applicability`, and `mora check --watch` with its summary per round.

Done when:

- CLI tests: `mora fix` applies a `safe` fix and leaves a `review` one, applies both with `--applicability review`, and `--code` limits it to one code.
- Two runs of `mora check --message-format json` on the same project print the same bytes.
- A watch-mode test edits a file and receives a new round that ends with the summary object.
- CLI tests: a `safe` fix whose edit lies in a path dependency's file, in the standard library, or in a file reached through a symbolic link that leaves the project is not applied and the file is unchanged; `mora fix` writes no file other than `.mr` files under the project's `src/` and `tests/`.
- A file is written only when its content is still what the check read, and it is replaced in one step (write beside it, then rename), so an interrupted run leaves no half-written source file. A test with a writer that edits the file between the check and the replacement finds that edit kept and the fix reported as not applied, through a held lock or an atomic compare-and-replace on each platform.
- An edit whose span is outside its file or splits a character is refused, never clamped (the renderer clamps, §17.6; a writer must not); `placeholder` fixes are never applied, whatever the flags; the run stops after four rounds.
- The JSON objects escape control characters in messages, labels and file names, and the human format does not pass terminal control sequences from source text or file names through to the terminal.

### M1.21 Error recovery that keeps types

Spec: [§14.5](docs/design/mora.md#145-name-resolution-and-types), [§8.1](docs/design/mora.md#81-generic-declarations), [§17.6](docs/design/mora.md#176-diagnostics), [§17.8](docs/design/mora.md#178-language-server). Crates: `mora-hir`, `mora-check`. Blocked by: M1.9.

The checker gives `Ty::Error` to an expression it cannot type and reports no diagnostic caused by one, so a mistake does not cascade, but the type is lost: hover, completion and inlay hints show nothing for that expression and for every local built from it, and later mistakes that involve it are not reported. This makes recovery a rule of §14.5 for every construct M1 checks: an ill-typed expression keeps its diagnostic and the type it would have had whenever that type is known without guessing (the declared return type of the only candidate, the declared type of a local, the type expected from context), and `Ty::Error` stays for the rest. Later deliverables follow the rule for the constructs they add, so the language server and `mora query` keep working in code that does not compile yet. To settle in the spec: whether a call with two or more overloads and a bad argument takes the return type the other arguments select, and whether an unresolved name takes the type expected at its position.

Done when:

- §14.5 states which mistakes keep a recovered type and which give the error type, and that neither is ever the cause of a diagnostic.
- UI tests: for each M1 code that has a recovered type, a body with that mistake reports it once, and a type error placed after it in the same body is reported too.
- Analysis-database tests: the type of each local declared after the mistake is the recovered type.

## M2: Memory model

**Goal.** Classes, structs, unique classes, parameter modes, moves, drops, destructors, borrows, views, weak references and closures, with ARC inserted and optimized, and a test suite that proves the rules of §10.

**Exit criteria.** The suite of §20.3 passes; the §10.13 snapshots pass.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M2.1](#m21-classes-fields-getters-and-constructors) | Classes, fields, getters and constructors | `hir` | M1.16 |
| [M2.2](#m22-positional-structs) | Positional structs | `hir` | M2.1 |
| [M2.3](#m23-shared-objects-and-reference-counts-in-the-runtime) | Shared objects and reference counts in the runtime | `rt` | M2.1 |
| [M2.4](#m24-parameter-modes-and-this) | Parameter modes and `this` | `mir` | M2.1 |
| [M2.5](#m25-move-checking-and-drop-elaboration) | Move checking and drop elaboration | `mir` | M2.4 |
| [M2.6](#m26-destructors-and-drop-order) | Destructors and drop order | `mir` | M2.3, M2.5 |
| [M2.7](#m27-unique-classes-and-move-only-values) | Unique classes and move-only values | `mir` | M2.5 |
| [M2.8](#m28-borrows-from-exclusive-and-aliased-places) | Borrows from exclusive and aliased places | `mir` | M2.7 |
| [M2.9](#m29-spans-views-and-memory-slices) | Spans, views and memory slices | `mir` | M2.8 |
| [M2.10](#m210-weak-references-and-the-cycle-lint) | Weak references and the cycle lint | `mir` | M2.3 |
| [M2.11](#m211-closures-and-function-types) | Closures and function types | `mir` | M2.8 |
| [M2.12](#m212-arc-insertion) | ARC insertion | `mir` | M2.11 |
| [M2.13](#m213-arc-optimization-and-its-guarantees) | ARC optimization and its guarantees | `mir` | M2.12 |
| [M2.14](#m214-debug-runtime-poisoning-and-leak-report) | Debug runtime: poisoning and leak report | `rt` | M2.3 |
| [M2.15](#m215-memory-model-test-suite) | Memory-model test suite | `testkit` | M2.6, M2.9, M2.10, M2.13, M2.14 |

### M2.1 Classes, fields, getters and constructors

Spec: [§6.3](docs/design/mora.md#63-classes), [§6.9](docs/design/mora.md#69-members). Crates: `mora-hir`, `mora-mir`. Blocked by: M1.16.

Class declarations, read-only and `mut` fields (D-15), write scopes `mut(private)`, `mut(ns)` and `mut(internal)` with lint `redundant-mut-scope` (D-101), `=>` getters and `stored` getters with recalculation on write (`MR0252`), constructors with `: this(...)` chaining, definite initialization `MR0140`, memberwise creation (`MR0143`, `MR0144`), initializers after a constructor, and `new` producing a heap object (counting arrives in M2.3). The getter cheapness check (`MR0251`) and the part of `MR0252` about capabilities and `dyn` calls need the effect analysis of §6.14 and arrive with M3.18.

Done when:

- UI tests for `MR0140`, `MR0143`, `MR0144` and assigning a read-only field.
- UI tests for writes outside a field's write scope, `redundant-mut-scope` and `MR0252`.
- Run tests showing that a `stored` getter reads the value recalculated by the last write to each input.
- UI tests for reading a field the use cannot see (`MR0129`, §4.5), from another type, file, folder and project.
- The `MR0140` tests cover, before every field is assigned: reading a field, calling a method or getter of `this`, passing `this` as an argument, capturing it in a lambda or a local function, a field assigned on one branch only, and a `: this(...)` chain that leaves one out.

### M2.2 Positional structs

Spec: [§6.2](docs/design/mora.md#62-structs), [§6.5](docs/design/mora.md#65-positional-structs). Crates: `mora-hir`. Blocked by: M2.1.

Positional structs with their generated constructor, public read-only fields and deconstruction, and `with` expressions on copyable structs. Equality, hashing and printing come from macros in M4.9.

Done when:

- Run tests for `with` on plain and positional structs, and for deconstruction.

### M2.3 Shared objects and reference counts in the runtime

Spec: [§13.3](docs/design/mora.md#133-shared-objects), [§13.4](docs/design/mora.md#134-strings-and-arrays). Crates: `mora-mir`, `mora-rt`, `mora-codegen`. Blocked by: M2.1.

The object header of §13.3, inline retain and release sequences, `mora_rt_drop` with the iterative release work list, weak count handling, immortal objects, `T[]` layout and allocation, and reference-count overflow abort. To settle in the spec: the panic for a negative or too large array length (§5.4, §11.4), and that allocation sizes are checked in every profile (§13.4).

Done when:

- Releasing a linked list of 1,000,000 nodes does not overflow the stack.
- Retaining an immortal object never writes to it (the test places one in read-only memory).
- Layout unit tests compute every header, string, array and union layout for both the 64-bit targets and the 32-bit test description.
- MIR lowers array creation `new T[n]` and signatures with an array type.
- Runtime tests set `strong` to 2^31 - 1 and `weak` to 2^32 - 2 and show the next retain and the next weak retain abort with `reference count overflow`; a weak load of an object whose strong count is zero gives `none`; a release of an immortal object and a weak retain of one write nothing.
- The size of an array allocation is computed with checked arithmetic in every profile: on the 32-bit test description, `new long[n]` with a length whose size wraps panics and allocates nothing.
- `new T[n]` with a negative `n` panics with the message the spec gives.
- The 1,000,000-node release test also runs with a destructor on the node type that releases another list, so the work list is entered again while it runs.

### M2.4 Parameter modes and `this`

Spec: [§10.3](docs/design/mora.md#103-parameter-modes). Crates: `mora-hir`, `mora-mir`. Blocked by: M2.1.

`in`, `ref` and `move` parameters, call-site `ref`/`move`, struct `mut` methods and `MR0141`, `move` methods, and the representations of §10.3.

Done when:

- UI tests for each mode's errors.
- Run tests passing a large struct by `in` and by `ref`.
- A `ref` argument or the receiver of a `mut` method that is not a mutable place, as in `Bump(ref 1)` or `Make().Inc()`, is reported (§10.3), and lowering no longer stops on it.

### M2.5 Move checking and drop elaboration

Spec: [§10.5](docs/design/mora.md#105-moves), [§10.8](docs/design/mora.md#108-drops-and-destructors), [§14.8](docs/design/mora.md#148-mir-checks). Crates: `mora-mir`. Blocked by: M2.4.

The move check and drop elaboration of §14.8 steps 1 and 2: `MR0300` to `MR0303`, drop flags for maybe-moved locals, drops at every scope exit, temporaries dropped at the end of their statement, and `Drop(x)`.

Done when:

- UI tests for every move error, each showing the move and the use.
- Drop-order run tests for early return, `break`, `continue` and moves in one branch.

### M2.6 Destructors and drop order

Spec: [§6.9](docs/design/mora.md#69-members), [§10.8](docs/design/mora.md#108-drops-and-destructors). Crates: `mora-hir`, `mora-mir`, `mora-rt`. Blocked by: M2.3, M2.5.

`~T()` declarations, `MR0142` and `MR0360`, destructor then fields in declaration order, struct destructors making the struct move-only, the resurrection check of D-46, and abort on a panic inside a destructor. To settle in the spec: how the runtime marks an object whose destructor is running, so that a retain or a release of it there cannot start a second drop (§6.9, §13.3).

Done when:

- Run tests whose destructors print match the order of §10.8.
- A resurrecting destructor aborts with the message of §6.9.
- A destructor that stores `this` and removes it again before it returns (a retain and a release of the dying object inside its own destructor) runs once and frees once, or aborts with the resurrection message, as the spec states; the test runs under the debug runtime and in a release build.
- A weak load of the object from inside its own destructor gives `none`.
- A panic inside a destructor aborts at once with exit code 101 and runs no other destructor (§10.8, §11.4).
- UI test: `MR0142` for a destructor that stores, returns or captures `this` directly.

### M2.7 Unique classes and move-only values

Spec: [§6.4](docs/design/mora.md#64-unique-classes), [§10.2](docs/design/mora.md#102-copyable-and-move-only-types), [§10.3](docs/design/mora.md#103-parameter-modes). Crates: `mora-hir`, `mora-mir`. Blocked by: M2.5.

`unique class`: header-less layout, moves on assignment and return, struct-like mutation rules (D-47), `mut` and `move` methods, move-only structs and unions, and `MR0310`.

Done when:

- UI tests for storing a borrowed unique value and for mutating through an `in` borrow.
- A run test shows a unique object's destructor running when its owner leaves scope.

### M2.8 Borrows from exclusive and aliased places

Spec: [§10.4](docs/design/mora.md#104-borrowing-from-places), [§14.8](docs/design/mora.md#148-mir-checks). Crates: `mora-mir`. Blocked by: M2.7.

Classification of borrowed argument places, `MR0320` to `MR0323`, snapshots for `in` borrows and copy-in/copy-out for `ref` borrows of aliased non-plain places, root keep-alive, and the closure rule of §10.4.

Done when:

- The `// expect: OLD` example of §10.4 runs under the debug runtime without a poisoned read.
- One adversarial run test per row of the §10.4 table.
- UI tests for `MR0320` (`ref a` with `in a`, and two `ref a`, also through a field path of one local), `MR0321`, `MR0322` and `MR0323`.
- Adversarial run tests under the debug runtime, beyond the table: a call that borrows a local and also receives a non-escaping closure that assigns it (the borrow is a snapshot); a callee that drops the last other reference to the root of a read-only field it borrowed (the root is kept alive); and a `ref` borrow of a `mut` field whose object the callee replaces before it returns (the write-back goes to the object the caller held).

### M2.9 Spans, views and memory slices

Spec: [§5.9](docs/design/mora.md#59-views), [§6.9](docs/design/mora.md#69-members), [§10.6](docs/design/mora.md#106-views). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M2.8.

`Span<T>`, `ReadOnlySpan<T>`, `ref struct`, `stackalloc`, range slicing, `Memory<T>` and `ReadOnlyMemory<T>`, and the view check of §10.6: `MR0340` to `MR0344`, hidden keep-alives that last to the view's last use, the `where T : allows ref struct` constraint (§8.2), and `params` arguments collected into their `ReadOnlySpan<T>` (§6.9). To settle in the spec: the definition of `stackalloc`: element types, the largest length, a negative length and whether the memory is zeroed (§5.9, §10.6).

Done when:

- UI tests for each view error, with the wording of §10.6.
- `let s = box.Data.AsSpan();` compiles and the snapshot lives until `s`'s last use.
- Calls of a `params` function are checked and lowered, with their arguments collected into a span; until then such a call reports that it is not lowered until M2.9.
- Run tests in both profiles: an index, a range start or end out of range, a start past the end, and a `^` index past the length each panic with the message of §11.4 on spans, `Memory<T>` and arrays; the bounds check does not rely on arithmetic that wraps in a release build.
- `stackalloc` with a negative length, and with one larger than the limit the spec gives, panics; memory it returns is probed page by page, so a length near the stack limit gives `stack overflow` (§13.10), and its span cannot leave its block (`MR0343`).
- Run test under the debug runtime: a `Span<T>` over a `Memory<T>` or an array is live while the last other reference to the array is dropped by a callee; the hidden reference of §10.6 keeps it alive.

### M2.10 Weak references and the cycle lint

Spec: [§10.7](docs/design/mora.md#107-weak-references). Crates: `mora-hir`, `mora-mir`, `mora-rt`, `mora-check`. Blocked by: M2.3.

`weak T` fields and locals, reads as `T?` (options arrive in M3.5; until then a built-in `weak` read form is used in tests), `MR0350`, and the `strong-cycle` lint `MR2020`.

Done when:

- A run test reads `none` after the target dies.
- Lint tests: a parent and child pair is reported; a self-referential tree is not.
- UI tests: `weak` on a unique class, a struct, a `string`, an array and an interface value is `MR0350`.
- A run test takes a weak reference to an object, lets the last strong reference go at its last use, and reads `none` in the same scope (§10.7); the object's memory is freed only after the weak reference dies (debug runtime).

### M2.11 Closures and function types

Spec: [§5.8](docs/design/mora.md#58-function-types), [§7.6](docs/design/mora.md#76-lambdas), [§10.9](docs/design/mora.md#109-closures), [§13.5](docs/design/mora.md#135-value-layouts). Crates: `mora-hir`, `mora-mir`, `mora-codegen`. Blocked by: M2.8.

`Func` and `Action`, method groups, lambdas, the three closure forms of §10.9 (non-escaping, closure struct, escaping), shared boxes for reassigned captures, `[weak x]`, `[move x]`, `static` lambdas, `MR0330` to `MR0333`, and both function-value layouts of §13.5.

Done when:

- Run tests for capture semantics (§10.9): a counter that an escaping lambda increments is seen by the enclosing function.
- UI tests for each closure error.
- A non-escaping lambda allocates nothing (allocation counter in the debug runtime).

### M2.12 ARC insertion

Spec: [§14.11](docs/design/mora.md#1411-arc-insertion). Crates: `mora-mir`. Blocked by: M2.11.

Insertion of `Retain` and `Release` after monomorphization, with `in` at +0 and `move` and returns at +1.

Done when:

- Every M2 run test is leak-free under the debug runtime.

### M2.13 ARC optimization and its guarantees

Spec: [§10.13](docs/design/mora.md#1013-what-the-optimizer-guarantees), [§14.12](docs/design/mora.md#1412-arc-optimization). Crates: `mora-mir`. Blocked by: M2.12.

The six passes of §14.12: last-use moves, retain/release pair cancellation, borrowed forwarding, immortal elimination, snapshot and keep-alive elision, and release at last use, delivering the ten reference-count guarantees of §10.13. Ownership summaries mark a parameter borrow-only only within a frame-limited bound: a borrow inferred for a callee does not keep the caller's object alive across a call of unbounded depth when the caller could release it before the call, and where a `with` reuse would fire the summary prefers owned (frame-limited reuse, Lorenzen and Leijen, ICFP 2022). §14.12 states the rule.

Done when:

- One MIR snapshot test per guarantee of §10.13 with zero `Retain`/`Release` statements.
- The M2 run tests still pass and stay leak-free.
- A MIR snapshot of a recursive function that passes a list it no longer needs shows the release before the recursive call, and a 100,000-deep run test stays leak-free, with the peak live heap held by the list and the objects it reaches independent of the depth (stack frames are not counted).
- Summaries are conservative where the compiler cannot see the callee: a call through an escaping function value, through an `any I` value, of an `extern` function, or of a function in a call-graph cycle whose fixed point is not reached, counts as writing every field and releasing every type. MIR snapshots show the snapshot, the root keep-alive and the retain/release pair kept in each of these cases, and kept when the callee writes the field or releases the type through a function it calls.
- The adversarial run tests of M2.8 pass under the debug runtime with the optimizer on, and each has a twin in which the elision does apply.
- Release at last use never comes before the last use of a view, a snapshot or a hidden owner that depends on the value (§10.8), and never moves the drop of a value whose drop runs a destructor: one run test each, with a weak reference showing where the release happened.
- Pair cancellation keeps a pair when the code between them makes a weak load of the object (§14.12 item 2).
- Each hand-written summary of a `Std` `unsafe` function is compared with what the debug runtime observes (§14.12).

### M2.14 Debug runtime: poisoning and leak report

Spec: [§10.11](docs/design/mora.md#1011-leak-and-misuse-detection), [§13.2](docs/design/mora.md#132-two-builds-of-the-runtime). Crates: `mora-rt`, `mora-driver`. Blocked by: M2.3.

Freed-memory poisoning, traps on releasing a dead object and on count underflow, the live-object list, the exit report grouped by type, cycle detection by trial deletion naming the fields, and `mora run --leak-check`.

Done when:

- A deliberately leaked cycle is reported with both field names.
- The release runtime has none of this (symbol check in the test).

### M2.15 Memory-model test suite

Spec: [§10.14](docs/design/mora.md#1014-soundness-summary), [§20.3](docs/design/mora.md#203-memory-model-suite). Crates: `tests/memory`. Blocked by: M2.6, M2.9, M2.10, M2.13, M2.14.

The suite of §20.3 for everything M2 builds: leak-freedom of every run test, adversarial aliasing tests per §10.4 row, closure and view rules, drop order, weak references and cycles. Collection tests are added in M5.4.

Done when:

- `tests/memory` runs in CI on every Tier 1 target.
- Each invariant of §10.14 names the tests that check it in `tests/memory/README.md`.

## M3: Expressive core

**Goal.** Unions, enums, patterns, options, results, operators, generics, interfaces, statics and local path dependencies.

**Exit criteria.** Run and UI tests for every construct of §§5 to 8; every §2.4 example runs.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M3.1](#m31-unions) | Unions | `hir` | M2.15 |
| [M3.2](#m32-enums) | Enums | `hir` | M3.1 |
| [M3.3](#m33-patterns-and-match) | Patterns and match | `hir` | M3.2 |
| [M3.4](#m34-exhaustiveness-and-reachability) | Exhaustiveness and reachability | `check` | M3.3 |
| [M3.5](#m35-options-none-and-flow-narrowing) | Options, `none` and flow narrowing | `hir` | M3.4 |
| [M3.6](#m36-operators-conversions-and-indexers) | Operators, conversions and indexers | `hir` | M3.3 |
| [M3.7](#m37-results-and--propagation) | Results and `?` propagation | `hir` | M3.5, M3.6 |
| [M3.8](#m38-generics-and-constraints) | Generics and constraints | `hir` | M3.1 |
| [M3.9](#m39-monomorphization) | Monomorphization | `mir` | M3.8 |
| [M3.10](#m310-interfaces-and-static-dispatch) | Interfaces and static dispatch | `hir` | M3.9 |
| [M3.11](#m311-dynamic-interface-values) | Dynamic interface values | `codegen` | M3.5, M3.10 |
| [M3.12](#m312-tuples-collection-expressions-and-smaller-expressions) | Tuples, collection expressions and smaller expressions | `hir` | M3.9 |
| [M3.13](#m313-statics) | Statics | `hir` | M1.11, M3.5 |
| [M3.14](#m314-local-path-dependencies-and-library-projects) | Local path dependencies and library projects | `driver` | M3.10 |
| [M3.15](#m315-attributes-cfg-lint-levels-and-obsolete) | Attributes: `Cfg`, lint levels and `Obsolete` | `check` | M3.14 |
| [M3.16](#m316-platform-capabilities-and-standard-library-layers) | Platform capabilities and standard library layers | `std` | M3.15 |
| [M3.17](#m317-interpolation-handlers) | Interpolation handlers | `hir` | M3.10 |
| [M3.18](#m318-effect-inference-and-uses-bounds) | Effect inference and `uses` bounds | `check` | M2.11, M3.11, M3.16, M3.17 |
| [M3.19](#m319-let-with-a-refutable-pattern-and-else) | `let` with a refutable pattern and `else` | `syntax` | M3.3, M3.4 |
| [M3.20](#m320-labeled-break-and-continue) | Labeled `break` and `continue` | `syntax` | M1.10 |
| [M3.21](#m321-newtypes-and-refinements) | Newtypes and refinements | `hir` | M3.5, M3.6, M3.10 |

### M3.1 Unions

Spec: [§6.6](docs/design/mora.md#66-unions), [§13.5](docs/design/mora.md#135-value-layouts). Crates: `mora-hir`, `mora-mir`, `mora-codegen`. Blocked by: M2.15.

Union declarations with cases and members, construction, target-typed case names (D-16), `indirect` cases and unions, `MR0150`, `@NonExhaustive`, copyable versus move-only unions, and the tag-plus-payload layout with the null-pointer niche of §13.5.

Done when:

- Run tests for construction, members and `indirect` recursion.
- A layout test shows `User?` is one pointer wide.
- UI tests for `MR0150`: a union that holds itself directly, through a struct, through an option, through a tuple and through a generic argument; each is one error and the compiler does not recurse without bound.
- UI test: a function that receives `ref u` and a borrow of a field of `u`'s current case in one call is `MR0320`.
- A run test under the debug runtime: a union stored in a `mut` field is passed as a snapshot when a case field is borrowed (§10.4), so assigning another case during the call leaves the borrow valid.
- Layout tests for the niche rules of §13.5 on both pointer widths: a union with a niche, one without, and an option of each; the tag read from a value the program built is always a declared case.

### M3.2 Enums

Spec: [§6.7](docs/design/mora.md#67-enums). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.1.

Closed enums with explicit backing types, checked casts that panic, `TryFrom`, `@Flags` enums with bitwise operators, and printing by name (through `IDisplay` until M4.9).

Done when:

- A cast of an undeclared value panics.
- `@Flags` combinations print and compare.

### M3.3 Patterns and match

Spec: [§7.7](docs/design/mora.md#77-match-expressions-and-statements), [§7.8](docs/design/mora.md#78-patterns). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.2.

Every pattern of §7.8, `is` expressions with definite assignment of bindings, one `match` with arms as a statement and as an expression (D-89), guards, block arms and best-common-type. To settle in the spec: whether a binding copies, borrows or moves the part it names, and what happens to the rest of a move-only scrutinee (§7.8, §10.5).

Done when:

- One run test per pattern kind.
- Bindings in `is` are usable exactly where the `is` expression is known to be true (§7.8).
- A benchmark rebuilds an `indirect` union tree with `match` and counts allocations per node, the baseline for deciding whether general reuse (D-71, v0.4) comes earlier.
- A `match` and an `is` on a value with a move-only field, and on a union with a move-only case field, follow the rule the spec states for bindings; under the debug runtime every destructor runs exactly once on each arm, guard failures included.
- A `rest` binding of a list pattern over an array, a span and a `List<T>` follows the view rules of §10.6 and §10.10: a UI test stores it past its scope, and a run test changes the list while it is live.
- A `match` that reaches no arm at run time (a value the checker did not expect) traps; it never continues into the next block.

### M3.4 Exhaustiveness and reachability

Spec: [§7.8](docs/design/mora.md#78-patterns), [§25](docs/design/mora.md#25-references). Crates: `mora-check`. Blocked by: M3.3.

The usefulness algorithm of Maranget over unions, enums, `bool`, options, results and tuples, `MR0170` with one missing value, `MR2010` for unreachable arms, and the `_` rule for integers and strings.

Done when:

- UI tests show the missing case text, for example `missing case: Rect(_, _)`.
- A property test checks that no arm matches any reported missing value.
- A property test generates types (unions, enums, `bool`, options, results, tuples, nested) with arm lists, and for every list the checker accepts as exhaustive, every generated value of the type matches an arm; for every arm it reports as unreachable (`MR2010`), no generated value reaches it first.
- A `@NonExhaustive` union or enum of another project needs a `_` arm (§6.6), a `@Flags` enum is never exhaustive without one (§6.7), and an enum match counts a value that two members share once.

### M3.5 Options, `none` and flow narrowing

Spec: [§5.6](docs/design/mora.md#56-option-types-and-none), [§7.5](docs/design/mora.md#75-option-operators). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.4.

`Option<T>` in the standard library, `T?` syntax, `none` (D-54), `MR0131`, implicit `T` to `T?`, `??`, `??=`, `?.`, `?[]`, `as` with `MR0210` (D-153), the members of §5.6, `MR0162`, `MR2001`, and flow narrowing after `none` checks. To settle in the spec: the places flow narrowing applies to and where it ends (§5.6).

Done when:

- Run tests for every operator.
- UI tests: `x.y` on `T?` offers both fixes; after `if (u is none) return;` `u` has type `User`.
- UI tests for the `T` to `T?` row of §5.10.
- UI tests: `as` where the type cannot vary is `MR0210`, and `1 as long` offers the fix to `(long)1`.
- UI tests, with the places and the ends of narrowing as §5.6 then states them: narrowing applies only to a place nothing else can write between the check and the use. A `mut` field of a class, a `static mut`, an array or collection element, and a local captured by a closure that assigns it are not narrowed; a narrowed local loses its narrowing at an assignment, at a `ref` borrow, at the end of a branch that the check guards and that does not exit, and at a loop back edge that can reach an assignment. The example of §5.6 keeps working: after `if (user is none) return;` the variable has type `User` for the rest of the block.
- A run test under the debug runtime for each of those: the code compiles only with `?.`, `??` or a new `is` test, and reads no payload of a `none`.

### M3.6 Operators, conversions and indexers

Spec: [§6.9](docs/design/mora.md#69-members), [§5.10](docs/design/mora.md#510-conversions), [§10.4](docs/design/mora.md#104-borrowing-from-places). Crates: `mora-hir`. Blocked by: M3.3.

User-defined arithmetic, bitwise and unary operators, comparison operators from `IEquatable<T>` and `IComparable<T>` with `MR0238` (D-90), implicit conversions through `IFrom<S>` (D-91), and indexers, including addressor indexers: an accessor `ref => ...` that returns a `ref` into the collection's storage after the bounds check, with no coroutine. `List<T>` uses one once it exists (M5.4), and `Dictionary` keeps its `TValue?` getter; yielding accessors stay out until a collection needs them. The spec changes first: §10.4 (collection elements are not places, `MR0323`) for collections with an addressor, the accessor list of §6.9, and `MR0320` for two `ref` elements of one collection in a call. To settle in the spec: what keeps a `ref` from an addressor valid when the same element is written through the collection during the call, for an element type that is not plain: the addressor is limited to plain element types, or an element write panics while such a `ref` is live, as a structural change does (§10.4, §10.10).

Done when:

- Run tests for a vector type with operators and an indexer.
- UI test for an unpaired `==`.
- UI tests for a conversion through a user type's `IFrom<S>`.
- A `ref` argument that is an element or a field of one, as in `Bump(ref xs[0])`, and the receiver of a `mut` method that is one, as in `xs[0].Inc()`, are lowered; until then such a call reports that it is not lowered until M3.6.
- On a collection type with an addressor indexer, `Bump(ref slots[i])` and `slots[i].Hits += 1` compile, and the MIR snapshot shows one store and no copy-back; growing the collection during the call panics as for a view (§10.10).
- UI test: `ref slots[i]` and `ref slots[j]` in one call is `MR0320`.
- An adversarial run test under the debug runtime: a function takes `ref slots[i]` and the collection, borrows a `string` field of the element through the `ref`, has a callee assign `slots[i]` through the collection, and reads the borrow. It reads no freed memory: the write panics, or the element type is refused an addressor, as the spec states.
- The same test with the collection reached through a second reference (a field of another object) instead of an argument, and with `slots[i].Name = x` as the write.
- An addressor's bounds check runs before the `ref` exists, in both profiles, and a `ref` from an addressor cannot be returned, stored or captured (the view rules of §10.6).

### M3.7 Results and `?` propagation

Spec: [§4.6](docs/design/mora.md#46-entry-point), [§5.7](docs/design/mora.md#57-result-types), [§7.4](docs/design/mora.md#74-propagation-), [§11.1](docs/design/mora.md#111-two-kinds-of-failure), [§11.2](docs/design/mora.md#112-error-types), [§11.3](docs/design/mora.md#113-conversions-in-). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.5, M3.6.

`Result<T, E>`, `@MustUse` and `MR2002`, `IError` with `Code`, `PublicMessage` and `HttpStatus` (D-108), the `?` operator with identity and `IFrom` conversions (`MR0201`, `MR0202`, `MR0161`), `@From` and `MR0151` (D-100), the implicit `Ok` conversion and `MR0132`, and return-type inference for top-level statements with the exit codes of §4.6.

Done when:

- §2.4's `Total` runs.
- A top-level `?` that fails prints `error: ...` and exits with 1.
- UI tests for the `T` to `Result<T, E>` and `@From` rows of §5.10.
- Drop-order run tests for the `?` exit under the debug runtime: locals with destructors are dropped in reverse order when `?` returns early, a local moved before the `?` is not dropped, a temporary of the statement that holds the `?` is dropped once, and the error value converted through `IFrom` (§11.3) is dropped once. The tests are added to `tests/memory` (M2.15).

### M3.8 Generics and constraints

Spec: [§8.1](docs/design/mora.md#81-generic-declarations), [§8.2](docs/design/mora.md#82-constraints). Crates: `mora-hir`. Blocked by: M3.1.

Generic types, methods and local functions, the constraints of §8.2 including copyable-by-default and `where T : move` (D-23), method type inference (§8.1), and `MR0234` for variance annotations.

Done when:

- UI tests for violating each constraint.
- Inference tests for calls with lambdas and generic arguments.

### M3.9 Monomorphization

Spec: [§8.1](docs/design/mora.md#81-generic-declarations), [§14.10](docs/design/mora.md#1410-monomorphization). Crates: `mora-mir`. Blocked by: M3.8.

Instantiation from the roots of §14.10, deduplication by item and arguments, and `MR0220` at depth 64.

Done when:

- A program using `List<int>` and `List<string>` (stub list in tests) emits two instantiations.
- UI test for polymorphic recursion.
- The polymorphic-recursion test includes a function that calls itself with `(T, T)` and one that calls itself with `T[]`: each reports `MR0220` once, in bounded time and memory, and no layout of a type that does not fit the address space is computed (with the checked layout arithmetic of M1.12).
- Instantiations are keyed by item and canonical type arguments, the project that declares each type included (identified as the spec then states, §13.6), so two types with one name from two projects give two instantiations.

### M3.10 Interfaces and static dispatch

Spec: [§6.8](docs/design/mora.md#68-interfaces), [§8.3](docs/design/mora.md#83-static-dispatch), [§8.5](docs/design/mora.md#85-built-in-interfaces), [§8.7](docs/design/mora.md#87-associated-types). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.9.

Interface declarations with default members, static abstract members, interface inheritance, implementation by name and explicit implementation, associated types, `MR0232` and `MR0233`, and the built-in interfaces of §8.5 declared in `std/`.

Done when:

- Run tests for default methods and static abstract members in generic code.
- A generic `Max<T>` over `IComparable<T>` compiles to a direct call (MIR snapshot).
- `<`, `>`, `<=` and `>=` on two structs call the struct's `IComparable<T>` and are `MR0204` for a struct that does not implement it (§8.5).
- `==` and `!=` on a struct that declares the `Equals` of `IEquatable<T>` (one parameter of the struct's type, returning `bool`) call it instead of comparing field by field, in a function body and in a constant, and so does a tuple or struct that holds one (§8.5, §14.6).

### M3.11 Dynamic interface values

Spec: [§7.5](docs/design/mora.md#75-option-operators), [§8.4](docs/design/mora.md#84-dynamic-interface-values), [§13.6](docs/design/mora.md#136-vtables-and-type-ids). Crates: `mora-mir`, `mora-codegen`. Blocked by: M3.5, M3.10.

Interface values as data and vtable pointers, boxing of value types with the LSP-visible boxing marker, vtables and BLAKE3 type ids of §13.6, `is C c`, `as C`, checked casts, `MR0230`, `MR0231`, erasure of associated types for `IEnumerable<T>` values, and `any I` satisfying a constraint `T : I`. To settle in the spec: what identifies the declaring project in a canonical type name (§13.6, §4.4).

Done when:

- Run tests for `List<IShape>` holding a class and a boxed struct.
- A failing `(C)i` cast panics with the message of §11.4.
- UI tests for the boxing and interface-value rows of §5.10.
- Run tests: `x as T` on an `any I` value gives `T?`, the value when its run-time type is `T` and `none` otherwise (D-153).
- A run test, following the project-identity rule the spec then gives for canonical type names (§13.6, §4.4), with two path dependencies whose own root namespaces and type names are equal, bound under two keys: `is`, `as` and a checked cast tell the two types apart, and one library reached through two keys gives one type.
- The compiler knows every type used as a dynamic value in the build, so it fails the build with an internal error when two different types get one type id; a unit test forces a collision.
- A value boxed as `any I` keeps the drop function, size and alignment of its own type: a run test drops a boxed struct with a destructor through an interface value, once, under the debug runtime.

### M3.12 Tuples, collection expressions and smaller expressions

Spec: [§5.5](docs/design/mora.md#55-tuples), [§7.9](docs/design/mora.md#79-collection-expressions), [§7.13](docs/design/mora.md#713-other-expressions), [§6.9](docs/design/mora.md#69-members), [§7.6](docs/design/mora.md#76-lambdas). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.9.

Tuples and deconstruction, collection expressions with spreads and target types (D-20, `MR0171`) and `[]` as an empty dictionary (D-99), extension blocks with their parsing (D-98), `nameof`, `sizeof`, `default` with `MR0190`, ranges and `^` indexes, and tuple designations as lambda parameters, `((word, count)) => ...` (new in `LambdaParam`; the double parentheses are needed because `(a, b) =>` is two parameters, §7.6).

Done when:

- Run tests for each form; `[1, 2, 3]` with no target type is a `List<int>`.
- Two tuples of one shape whose element types differ, as `t == (1, 2)` with `t: (byte, byte)`, compare element by element (§5.5).
- Run test: the lambda `((word, count)) => count > 10`, passed where a function from `(string, int)` to `bool` is expected, binds both parts.
- Run tests in both profiles: `a[^0]`, `a[^(n + 1)]`, `a[5..2]` and a range past the end panic with the message of §11.4 on arrays, spans and strings, and a string range that splits a character panics (§5.3), so no `string` ever holds invalid UTF-8.
- The length of a collection expression with spreads is computed with checked arithmetic in every profile; storage is never allocated from a wrapped length.
- `default(T)` for a type without `IDefault` is `MR0190` for classes, unique classes, views and types that hold one, so no value is ever made from zero bytes.

### M3.13 Statics

Spec: [§4.7](docs/design/mora.md#47-static-state), [§6.10](docs/design/mora.md#610-constants-and-statics). Crates: `mora-hir`, `mora-mir`, `mora-rt`. Blocked by: M1.11, M3.5.

`const` in read-only data, constant sequences as `ReadOnlySpan<T>`, `static lazy` with run-time cycle detection and `MR0126`, `static mut` with its initializers evaluated at compile time, and `MR0155` for a static member named through a value (§6.10).

Done when:

- A lazy cycle panics with the message of §4.7.
- A `const` string longer than 15 bytes is an immortal object in read-only data, and reading it never writes its count.
- A UI test reports `MR0155` for a static method and a static getter named through a value, and its fix writes the type's name in place of the value.

### M3.14 Local path dependencies and library projects

Spec: [§4.4](docs/design/mora.md#44-dependencies), [§17.2](docs/design/mora.md#172-manifest). Crates: `mora-driver`, `mora-pkg`, `mora-hir`. Blocked by: M3.10.

`kind = "lib"`, root namespaces, `[dependencies]` path entries whose keys bind each dependency's root namespace (D-96), `MR0122`, `MR0501`, the dependency-cycle check `MR0502`, and `mora.lock` with path entries. To settle in the spec: whether `path` may be absolute (§4.4), and where the output of a path dependency goes (§17.3).

Done when:

- A two-project example (app plus library) builds and runs.
- A remote dependency source reports `MR0122`.
- Two path dependencies that depend on each other report `MR0502`.
- A project is identified by the resolved location of its manifest: a test reaches one library through two spellings of its path and through a symbolic link and gets one project, one set of types and one copy of its code, and `MR0502` is reported for a cycle written with different spellings.
- Building, `mora fmt` and `mora fix` write nothing inside a dependency's folder: its output and caches go under the root project's `target/`, and `mora.lock` is written at the root project only (CLI test with a read-only dependency folder).
- `mora.lock` records each path as written, relative to the manifest, never an absolute path of the machine; an absolute `path` in `mora.toml` follows the rule the spec gives.

### M3.15 Attributes: `Cfg`, lint levels and `Obsolete`

Spec: [§6.11](docs/design/mora.md#611-attributes), [§17.2](docs/design/mora.md#172-manifest). Crates: `mora-hir`, `mora-check`, `mora-driver`. Blocked by: M3.14.

Built-in attributes other than macros and `Test`: `@Cfg` with `Os`, `Arch`, `Debug`, `Test`, `Feature`; `[features]` in the manifest; `@Allow`, `@Warn`, `@Deny` and `[lints]`; `@Obsolete` with `MR2003`; `@Inline`; `@Layout(C)`; `@MustUse` on functions. To settle in the spec: the error for a lint level applied to a code that is not a warning or a lint (§6.11, §17.2).

Done when:

- A `@Cfg(Os = "windows")` function compiles only on Windows CI.
- `@Deny("MR2002")` turns the warning into an error.
- UI tests: `@Allow("MR0310")`, `@Warn("MR0300")` and a `[lints]` entry naming a code outside `MR2000` to `MR2999` change nothing: the error is still reported and the build fails, and the attribute or key itself is reported with the code the spec assigns. The same holds for `MR0107` and `MR0613`.
- Code under a `@Cfg` that does not hold is still parsed, and what the others compile is checked in full: a UI test puts a move error in a function compiled only for the current target.
- `@Layout(C)` changes field order and alignment only; a `@Layout(C)` struct with a counted or move-only field is dropped and counted like any other (run test under the debug runtime).

### M3.16 Platform capabilities and standard library layers

Spec: [§19.4](docs/design/mora.md#194-portability), [§15](docs/design/mora.md#15-standard-library-mvp-surface). Crates: `std`, `mora-hir`, `mora-target`. Blocked by: M3.15.

`Std.Core`, `Std.Alloc` and the hosted rest of `Std` as in §19.4 rule 4, capability tags on every hosted API, and `MR0510` when a program uses an API whose capability the target lacks.

Done when:

- A program using `File` against a test target description without `fs` reports `MR0510` naming `File`, `fs` and the target.
- `Std.Core` compiles for the 32-bit test description with no `heap` capability.
- A test over `std/` fails when a function that calls a runtime entry point or a platform function of §19.4 (files, sockets, environment, clock, random, process, console input) carries neither the capability tag for it nor a caller-independent reason listed in `std/README.md`; the list of entry points and their tags is one table the test reads.
- The table names the capability for every hosted API the spec lists in §15, `Environment.Exit` and reading standard input included.

### M3.17 Interpolation handlers

Spec: [§7.14](docs/design/mora.md#714-interpolation-handlers), [§7.13](docs/design/mora.md#713-other-expressions), [§8.5](docs/design/mora.md#85-built-in-interfaces). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.10.

`IInterpolate`, `IHole<T>` and `Literal` (D-105): lowering an interpolated string whose target implements `IInterpolate` to `Begin`, `AppendLiteral`, and `AppendHole` for holes that implement `IHole<T>` or `AppendDisplay` for other `IDisplay` holes, skipping every part when `Enabled` is `false`, the hole's format and source text, and lint `unchecked-injection`. Lint `skipped-hole-effects` needs the effect analysis of §6.14 and arrives with M3.18.

Done when:

- A test handler records its calls: literal parts and holes arrive in source order, with each hole's format and source text.
- A hole with a side effect does not run when `Enabled` is `false`.
- A hole type with only `IDisplay` goes to `AppendDisplay`, and one that also implements `IHole<T>` goes to `AppendHole`.
- UI tests: a hole type with neither `IHole<T>` nor `IDisplay` is an error naming both; assigning a `string` to a handler type does not compile.
- UI tests: a `Literal` cannot be created from a run-time `string`: no conversion, no cast, no constructor, no `default(Literal)`, no memberwise creation and no `with`, in a user project; a `const` string and a string literal convert, and an interpolated string with a hole does not.
- UI test: lint `unchecked-injection` flags a call of a function marked as an escape hatch, as `Html.Unchecked(text)` will be, including through a method group.
- `format` and `source` reach the handler as `Literal` values holding exactly the text in the source file.

### M3.18 Effect inference and `uses` bounds

Spec: [§6.14](docs/design/mora.md#614-effects), [§6.9](docs/design/mora.md#69-members). Crates: `mora-hir`, `mora-mir`, `mora-check`, `mora-cli`. Blocked by: M2.11, M3.11, M3.16, M3.17.

Effect sets per function (D-102) from `@RequiresCapability`, `@Blocking`, `extern` functions (`ffi`) and `Secret<T>.Use` (`secret`), inferred per strongly connected component of the call graph and per generic instantiation, with closures passed to `in` parameters charged to the caller and `dyn` for calls through escaping function values and `any I` without a bound. `uses` bounds on functions, interface methods and function types, checked with `MR0250` and the call path that brings the effect in, and `mora explain-effects`. The getter cheapness check `MR0251` (D-103) with `@Cheap` trusted inside `Std`, the part of `MR0252` about capabilities and `dyn` calls, and lint `skipped-hole-effects`. Hover (M7.3) and the inlay hint (M7.4) read the same data.

Done when:

- A `uses none` function that calls `Clock.Now()` is `MR0250`, and the message shows the path `clock <- Clock.Now`.
- Two mutually recursive functions, one of which reads the clock, both infer `clock` with no annotation.
- `list.Sort(compare)` infers exactly the effects of `compare`, and a call through a stored function value without a bound adds `dyn`.
- UI tests for `MR0251` reporting a loop, a heap allocation, a mutation and a failure, each naming the call.
- `mora explain-effects` prints each effect of a function with its call path.
- A function that calls an `extern` function infers `ffi`, at any depth, and `uses none` on it is `MR0250`.
- Calls the source does not write as calls carry their effects: a destructor run by a drop, a getter, an indexer, a user-defined operator and conversion (`IFrom`), an interpolation handler, a `static lazy` initializer on first access, a default interface method, a field initializer run by memberwise creation, and `foreach` over a user enumerator. One test each infers `clock` from a `Clock.Now()` placed there.
- A generic function is charged per instantiation with what its type arguments' members do, and a call through `any I` without a bound, through an escaping function value, and through a function value read from a field or a collection adds `dyn`; a bound on the interface method or the function type is then checked against every implementation and every lambda converted to it.
- `@RequiresCapability` and `@Blocking` only add effects; no attribute or bound removes one that inference found, and `@Cheap` outside `Std` is `MR0254`.

### M3.19 `let` with a refutable pattern and `else`

Spec: [§7.10](docs/design/mora.md#710-locals), [§7.8](docs/design/mora.md#78-patterns), [§7.11](docs/design/mora.md#711-statements). Crates: `mora-syntax`, `mora-hir`, `mora-check`, `mora-mir`. Blocked by: M3.3, M3.4.

`let PATTERN = expr else { ... };` binds the pattern's names for the rest of the block and runs the `else` block when the pattern does not match, as in `let Object(let fields) = config else { return Err(ConfigError.NotAnObject); };`. The `else` block must end its path (`return`, `panic`, `Unreachable`, `Todo`, `break`, `continue` or a loop that never exits) under the rules of D-138. The bindings of an `is` test reach only the end of its statement (D-132), so `if (r is not Ok(let v)) return;` cannot bind `v` for the code below, and taking a union apart step by step takes one `match` per step today; no existing construct does this job (D-66). The parser decides without types (D-115): after `let`, a name, a tuple of names or `_` stays a declaration or a deconstruction, and any other pattern starts this form and requires `else`. To settle in the spec: whether each binding keeps its inner `let`, and whether `var` starts the form too.

Done when:

- §7.10 and the ungrammar's `LocalDecl` and `DeconstructStmt` define the form, and the grammar fuzz target of M1.19 keeps passing.
- Run tests for a union case, a nested case and a constant pattern, each through the binding path and the `else` path.
- UI tests: an `else` block whose end is reachable, and a refutable pattern after `let` without `else`, each report a code the spec assigns; with a pattern that always matches, the `else` block is reported unreachable.
- UI test: the bindings are definitely assigned after the statement and not visible inside the `else` block.
- Run tests under the debug runtime with a scrutinee that has a destructor, one that is a temporary and one that is a move-only local: on the binding path and on the `else` path each destructor runs exactly once and nothing leaks, following the binding rule M3.3 settles; a moved-from scrutinee is not usable after the statement (`MR0300`).

### M3.20 Labeled `break` and `continue`

Spec: [§7.11](docs/design/mora.md#711-statements), [§7.10](docs/design/mora.md#710-locals). Crates: `mora-syntax`, `mora-hir`, `mora-check`, `mora-mir`. Blocked by: M1.10.

A loop may carry a label between its keyword and `(`, as in `foreach records (let line in lines)`, and `break records;` or `continue records;` act on that loop instead of the innermost one, so leaving an outer loop from an inner one no longer takes a flag variable and a second test. The label position parses without types (D-115). The other spellings clash with existing rules: `records: foreach` reads as a name and its type (D-58), `'records` as a character literal (§3.5) and `@records` as an attribute (§6.11). A lambda or a local function still does not see the loops around it, and labeled blocks that produce a value are not part of this: §7.7 asks for a function instead. To settle in the spec: where the label goes on `do`/`while`.

Done when:

- §7.11 and the ungrammar's loop, `BreakStmt` and `ContinueStmt` rules define labels, and the grammar fuzz target of M1.19 keeps passing.
- Run tests for a labeled `break` and `continue` on each loop kind (`while`, `do`, `for`, `foreach`), from two levels of nesting.
- UI tests: an unknown label, a label reused by a nested loop (no shadowing, as for locals under D-131) and a label named inside a lambda or local function within the loop each report a code the spec assigns.
- UI tests: D-138 and §7.10 count `break outer` as leaving `outer`, so a `while outer (true)` that an inner `break outer` leaves does not end the path, and definite assignment follows the labeled exits.
- Drop-order run tests under the debug runtime: `break outer` and `continue outer` from two levels down drop the locals of every scope they leave, innermost first, each once, with a local moved on one branch (drop flag) and a live view guard (§10.10) among them.
- UI tests: `MR0303` for a value declared outside `outer` and moved in the inner loop before `continue outer`, and `MR0300` for a use after a labeled exit skipped the assignment that would have refilled it.

### M3.21 Newtypes and refinements

Spec: [§6.12](docs/design/mora.md#612-newtypes), [§5.10](docs/design/mora.md#510-conversions), [§14.6](docs/design/mora.md#146-constant-evaluation). Crates: `mora-syntax`, `mora-hir`, `mora-mir`, `std`. Blocked by: M3.5, M3.6, M3.10.

`newtype Name : T;` and `newtype Name : T is pattern;` (D-62): the declaration and its body, the layout of the underlying type, the implicit conversion to it through a generated `IFrom`, `From` (returning an option when there is a refinement) and `Parse` (whose `ParseError` names the refinement), distinct types, members that read the underlying `value`, equality, hashing, ordering and printing from the underlying type, `IDefault` when the underlying default satisfies the refinement, and constant evaluation of refinements with `MR0260`. `@Arithmetic` comes with the standard-library macros of M4.9, the JSON mapping of a newtype with M5.11, and predicate refinements after the MVP (OQ-22).

Done when:

- Run tests for `UserId`, `Port` and `Slug` of §6.12: `From`, `Parse`, the implicit conversion to the underlying type and a member that reads `value`.
- UI tests: `let port: Port = 70000;` is `MR0260` with the message of §6.12, a `UserId` passed where an `OrderId` is expected is a type mismatch, `default(Port)` is `MR0190`, and the newtype-to-underlying-type row of §5.10.
- Constant evaluation matches a constant against a refinement pattern (§14.6), and a constant that satisfies it becomes the wrapped value (MIR snapshot).

## M4: Macros

**Goal.** Attribute and expression macros written in Mora, run at compile time on a typed model, plus the standard-library macros that replace a derive list.

**Exit criteria.** Every §9 example runs; `@Equatable`, `@Hashable`, `@Comparable`, `@Display`, `@Default`, `@Clone` and `#Assert` pass their `MacroTest` suites; structs get their automatic implementations from these macros.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M4.1](#m41-macro-declarations-and-quoted-code) | Macro declarations and quoted code | `macro` | M3.15 |
| [M4.2](#m42-the-stdmeta-model) | The `Std.Meta` model | `macro` | M4.1 |
| [M4.3](#m43-macro-runner-and-sandbox) | Macro runner and sandbox | `macro` | M4.2 |
| [M4.4](#m44-expansion-ordering-and-hygiene) | Expansion, ordering and hygiene | `macro` | M4.3 |
| [M4.5](#m45-expression-macros) | Expression macros | `macro` | M4.4 |
| [M4.6](#m46-expansion-cache) | Expansion cache | `macro` | M4.4 |
| [M4.7](#m47-seeing-and-testing-expansions) | Seeing and testing expansions | `macro` | M4.5 |
| [M4.8](#m48-error-pages-and-guide-for-macro-authors) | Error pages and guide for macro authors | `docs` | M4.7 |
| [M4.9](#m49-standard-library-macros) | Standard-library macros | `std` | M4.7, M3.21 |

### M4.1 Macro declarations and quoted code

Spec: [§9.2](docs/design/mora.md#92-declaring-macros), [§9.3](docs/design/mora.md#93-quoting-code). Crates: `mora-syntax`, `mora-hir`. Blocked by: M3.15.

Name resolution and checking of `macro` declarations (both kinds, attribute arguments as constant parameters, `adds`), the `Code` type, `code { }` and `code(...)` parsed when the macro compiles, splice typing with `MR0611`, `$..xs`, and `JoinWith`.

Done when:

- UI tests: a syntax error inside `code { }` is reported in the macro's project.
- A splice of a `Type` in expression position reports `MR0611` at the splice.

### M4.2 The `Std.Meta` model

Spec: [§9.5](docs/design/mora.md#95-the-typed-model-stdmeta). Crates: `mora-hir`, `std`. Blocked by: M4.1.

The read-only model of §9.5 built from HIR signatures (`Type`, `Field`, `Getter`, `Method`, `Case`, `Attribute`, `Expr`), `Fail` and `Warn` with locations in user code.

Done when:

- Unit tests build the model of each declaration kind and compare it with a snapshot.

### M4.3 Macro runner and sandbox

Spec: [§9.7](docs/design/mora.md#97-running-macros), [§14.2](docs/design/mora.md#142-crates). Crates: `mora-macro`, `mora-hir`. Blocked by: M4.2.

The `MacroRunner` interface in `mora-hir` and its implementation in `mora-macro`: compiling macros with the JIT, the capability sandbox (`MR0613`), the 5-second and 256 MiB limits, panics reported at the use site, and same-project cycles (`MR0614`). To settle in the spec: the state a macro invocation starts from, and how standard input, `Environment.Exit` and console output count in the sandbox (§9.7, §6.14).

Done when:

- A macro calling `File.ReadAllText` fails with `MR0613`.
- An infinite loop in a macro fails after 5 seconds with the use site in the message.
- UI tests for `MR0613`: a macro, or a function it calls at any depth, that calls an `extern` function (`ffi`), reads the clock, the environment or random numbers, opens a socket, or calls through an escaping function value or an `any I` value without a bound (`dyn`).
- A macro that panics, recurses until the stack ends, or allocates past 256 MiB fails the build with an error at the use site that carries its message; the compiler process and a running language server survive each case and go on to report the other diagnostics (test in `mora-macro` and a scripted LSP session).
- The time limit stops a loop that makes no call and no allocation, and the memory a stopped macro held is returned. The limits apply to each invocation in the language server too.
- Each invocation starts from the state the spec then gives (§9.7), so that running the macros of a build in any order or in parallel gives the same bytes (test runs a build twice with the order reversed). If the ruling resets state per invocation, `static mut` and `static lazy` values, the identity counter and hash keys do not carry over from another invocation or another macro.
- A macro cannot end or block the build: `Environment.Exit` and a read of standard input in a macro fail at the use site (`MR0613` or another code the spec assigns). To settle in the spec: how the `stdio` capability divides console output, which §9.8 allows in a macro, from input.

### M4.4 Expansion, ordering and hygiene

Spec: [§9.4](docs/design/mora.md#94-hygiene), [§9.6](docs/design/mora.md#96-what-a-macro-sees). Crates: `mora-macro`, `mora-hir`. Blocked by: M4.3.

The `adds` pre-pass, expansion between signatures and body checking, the visibility rule of §9.6, attribute-argument binding, hygienic renaming, definition-site resolution of literal names, `MR0610` and `MR0612`.

Done when:

- Two macros on one type produce the same result in either order.
- A local named like a macro-introduced local does not collide (run test).

### M4.5 Expression macros

Spec: [§9.2](docs/design/mora.md#92-declaring-macros), [§9.5](docs/design/mora.md#95-the-typed-model-stdmeta). Crates: `mora-macro`, `mora-hir`. Blocked by: M4.4.

`#Name(args)` calls, constant and `Expr` parameters, and replacement of the call by the returned expression. To settle in the spec: the limit on expansion depth and on the size of one expansion (§9.7).

Done when:

- A user-written expression macro that turns `#Twice(x)` into `x + x` runs.
- An expression macro whose result contains a call of itself, directly or through another macro, is reported once with the code the spec assigns, at the outermost use, and the compiler neither hangs nor overflows its stack; the nesting limit of §3.1 (`MR0018`) or the expansion limit the spec gives applies to the expanded expression.
- A constant argument and an `Expr` argument reach the macro as values of the model: text in a string argument is never parsed as code, and an `Expr` spliced into the result keeps the names it had at the call site (§9.4).

### M4.6 Expansion cache

Spec: [§9.7](docs/design/mora.md#97-running-macros). Crates: `mora-macro`. Blocked by: M4.4.

Caching each expansion under a hash of the toolchain version, the model it read, and the MIR of the macro and every function it calls transitively, in the `salsa` database and on disk. To settle in the spec: where the on-disk cache lives and what makes an entry valid, with §14.15.

Done when:

- Editing a method body does not rerun any macro (counter in `--verbose`).
- Editing a field reruns only the macros on that type.
- Editing a helper function a macro calls reruns that macro.
- A cached expansion is used only when the build can show it was written by this toolchain for this key. A hash stored beside the entry does not show that, so the check is one the project's own files cannot forge, as §14.15 then states (for example a cache outside the project, or an entry the toolchain authenticates). A test edits, truncates and swaps entry files and the macro runs again or the build fails; a damaged entry never panics the compiler. The key is a BLAKE3 hash (as §13.6 uses), not a shorter one.
- The key covers every input: editing a constant or a static initializer the macro reads, an attribute argument, a helper attribute on a member (§9.9), the `@Cfg` values in force, and the text of the type as the model shows it each rerun the macro; a change to the toolchain version discards every entry.
- The diagnostics and the console output of a cached expansion are replayed, so a warning from `Warn` is reported on every build.

### M4.7 Seeing and testing expansions

Spec: [§9.8](docs/design/mora.md#98-seeing-and-debugging-expansions). Crates: `mora-cli`, `mora-macro`, `std`. Blocked by: M4.5.

`mora expand`, errors in generated code with the secondary span at the producing `code` line, `[macro Name]`-prefixed console output, and `Std.Testing.MacroTest`. To settle in the spec: how macro console output is carried in the JSON format (§9.8, §17.10).

Done when:

- `mora expand --type UsersApi.User` prints formatted output (snapshot).
- A type error in generated code shows `note: generated by ...`.
- With `--message-format json`, macro output never appears as a bare line on the stream that carries diagnostics: each line is one object with its own `schema`, or goes to standard error, as the spec states. A test macro prints a line that is a valid `mora.diagnostic/1` object and the output still holds no diagnostic it did not report.
- In the human format, control characters in macro output are escaped, and the amount printed per invocation is bounded.
- `mora expand` runs macros under the same sandbox and limits as a build (M4.3) and writes no file.

### M4.8 Error pages and guide for macro authors

Spec: [§9](docs/design/mora.md#9-macros), [§17.6](docs/design/mora.md#176-diagnostics). Crates: `docs`. Blocked by: M4.7.

`docs/errors` pages for `MR0610` to `MR0615` and a guide `docs/guide/macros.md` that walks through writing, debugging and testing a macro, with every example executable.

Done when:

- `cargo xtask spec-examples` runs the guide's examples.

### M4.9 Standard-library macros

Spec: [§9.1](docs/design/mora.md#91-a-complete-example), [§9.6](docs/design/mora.md#96-what-a-macro-sees), [§9.9](docs/design/mora.md#99-standard-library-macros), [§6.2](docs/design/mora.md#62-structs), [§6.12](docs/design/mora.md#612-newtypes). Crates: `std`, `mora-hir`. Blocked by: M4.7, M3.21.

`@Equatable`, `@Hashable`, `@Comparable`, `@Display`, `@Default`, `@Clone` and `#Assert` with the behaviour of §9.9, and their automatic application to structs, tuples, enums and unions. `@Arithmetic` on newtypes (§6.12), with each result of a refined newtype checked against its refinement. `@Error` on unions and structs, with `@Code`, `@Public` and `@Status` (D-108), and helper attributes with `MR0615`.

Done when:

- A `MacroTest` suite per macro.
- §9.1's example prints `True`; structs compare by fields and print as `User { Id = 1, ... }`.
- Run test: `@Arithmetic` on `Port` adds two ports, and 65535 plus 1 panics because the result is not a `Port`.

## M5: Standard library core

**Goal.** Text, collections, sequence operators, console, environment, time, files, JSON, logging, secrets and testing: enough standard library for real programs.

**Exit criteria.** The std API reference builds from doc comments; JSONTestSuite parsing cases pass; sequence operator chains meet §15.5 with no allocation.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M5.1](#m51-standard-library-internals) | Standard library internals | `std` | M3.11 |
| [M5.2](#m52-strings-text-and-formatting) | Strings, text and formatting | `std` | M5.1, M4.9 |
| [M5.3](#m53-number-parsing-and-formatting) | Number parsing and formatting | `std` | M5.2 |
| [M5.4](#m54-list-arrays-spans-and-memory) | List, arrays, spans and memory | `std` | M5.1, M2.9 |
| [M5.5](#m55-dictionary-hashset-queue-and-stack) | Dictionary, HashSet, Queue and Stack | `std` | M5.4, M3.18 |
| [M5.6](#m56-enumeration-protocol-and-foreach) | Enumeration protocol and foreach | `std` | M5.4 |
| [M5.7](#m57-sequence-operators) | Sequence operators | `std` | M5.6, M2.11 |
| [M5.8](#m58-console-environment-time-and-random) | Console, environment, time and random | `std` | M5.2 |
| [M5.9](#m59-files-directories-and-paths) | Files, directories and paths | `std` | M5.8 |
| [M5.10](#m510-json-values-parser-and-writer) | JSON values, parser and writer | `std` | M5.5 |
| [M5.11](#m511-the-json-macro) | The `@Json` macro | `std` | M5.10, M4.9, M3.21 |
| [M5.12](#m512-testing-library-and-mora-test) | Testing library and `mora test` | `cli` | M5.7, M5.8, M5.11 |
| [M5.13](#m513-signals-and-observable) | Signals and `@Observable` | `std` | M5.4, M2.10, M4.9 |
| [M5.14](#m514-html-url-and-literal) | `Html`, `Url` and `Literal` | `std` | M5.2, M3.17 |
| [M5.15](#m515-logging-and-tracing) | Logging and tracing | `std` | M5.8, M5.10, M3.17 |
| [M5.16](#m516-secrett) | `Secret<T>` | `std` | M5.11, M5.15, M3.18 |
| [M5.17](#m517-report-and-context) | `Report` and `Context` | `std` | M5.2 |
| [M5.18](#m518-standard-library-api-reference) | Standard library API reference | `docs` | M5.9, M5.12, M5.13, M5.14, M5.15, M5.16, M5.17 |

### M5.1 Standard library internals

Spec: [§13.11](docs/design/mora.md#1311-standard-library-internals). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.11.

The private `unsafe` dialect of §13.11 accepted only when compiling `Std` (`MR0107` elsewhere), `RawPtr<T>`, `Uninit<T>`, the intrinsics, the `RawDeref` projection, and MIR checks skipping raw places. Documented in `std/README.md`. To settle in the spec: what makes a compilation the compilation of `Std` (the standard library sources of the toolchain archive, §19.2, and nothing a project, a dependency, a manifest key or a command-line option can claim), and how the dialect computes sizes and offsets so that an overflow panics in every profile instead of wrapping (§7.12).

Done when:

- Using `unsafe` in a user project reports `MR0107`.
- A test-only `std` module moves values through raw storage with no leak.
- UI test: a project whose root namespace is `Std`, and a path dependency that declares `namespace Std.Collections`, still report `MR0107` for `unsafe`, cannot declare an `extern` function with `@Import(Lib = "mora_rt")`, and get `MR0254` for `@Cheap`.
- Run test in `dev` and in `release`: a test-only `std` module that asks `Alloc`, `Realloc`, `Offset` or `Add` for a size or offset that overflows panics before any memory is allocated or written.

### M5.2 Strings, text and formatting

Spec: [§5.3](docs/design/mora.md#53-strings), [§15.2](docs/design/mora.md#152-strings-and-text). Crates: `std`, `mora-rt`. Blocked by: M5.1, M4.9.

The `string`, `StringBuilder`, `Formatter` and `char` members of §15.2, byte-offset slicing with boundary panics, `MR0130`, and the format specifiers of §15.2 for interpolation and `ToString(spec)`.

Done when:

- Run tests for every member, including non-ASCII and astral-plane text.
- Run tests order strings with `<`, `>`, `<=` and `>=` (§7.12).
- Format specifier tests match the culture-independent output of §15.2 for a table of values.
- Run tests in `dev` and in `release`: `"ab".Repeat(int.MaxValue)`, `"a".PadLeft(int.MaxValue)` joined to a non-empty string, and a `StringBuilder` grown past `int.MaxValue` bytes each panic with a message naming the limit before any buffer is written; none returns a string with a wrapped length. `string.FromUtf8` returns `Err` for overlong forms, surrogate code points, values above U+10FFFF and a sequence cut short at the end of the span.

### M5.3 Number parsing and formatting

Spec: [§8.5](docs/design/mora.md#85-built-in-interfaces), [§15.1](docs/design/mora.md#151-prelude-std). Crates: `std`, `mora-rt`. Blocked by: M5.2.

`IParse<T>`, `int.Parse` and the other primitives returning `Result<T, ParseError>`, shortest round-trip float formatting and correct float parsing in the runtime.

Done when:

- Round-trip property tests for `double`.
- Parsing errors carry the offending text.
- For every integer primitive, parsing the text of `MaxValue + 1` and of `MinValue - 1` returns `Err` in `dev` and in `release`, `MinValue` and `MaxValue` parse, and a digit string of 10,000 characters returns `Err` without a wrapped value. `double.Parse` of a 10,000-digit number and of an exponent beyond the range returns a value or an `Err` in time linear in the input.

### M5.4 List, arrays, spans and memory

Spec: [§10.10](docs/design/mora.md#1010-growable-collections-and-interior-views), [§15.3](docs/design/mora.md#153-collections). Crates: `std`, `mora-mir`. Blocked by: M5.1, M2.9.

`List<T>` with borrow count and version, the array, span and memory members of §15.3, `Sort` overloads, and the hidden span guards of §10.10 inserted by the view check.

Done when:

- Adding to a list while a span of it is live panics with the §10.10 message.
- Memory suite additions for collections (§20.3).

### M5.5 Dictionary, HashSet, Queue and Stack

Spec: [§15.3](docs/design/mora.md#153-collections), [§15.4](docs/design/mora.md#154-lookups-never-panic). Crates: `std`, `mora-rt`. Blocked by: M5.4, M3.18.

Insertion-ordered `Dictionary` (D-29) hashed with per-process random keys, `Hasher.Fast` (the keyed fast hash, the default) and `Hasher.Strong` (SipHash-1-3) with the optional `hasher` constructor argument of §15.3, the keys fixed under `mora test` and `mora bench`, the members of §15.3 and §15.4 including `Get` returning `TValue?`, `HashSet`, `Queue` and `Stack`. The `Dictionary` index `get` and `Count`, and `HashSet.Contains` and `Count`, are marked `@Cheap` (D-103), and `@Cheap` outside `Std` is `MR0254`. This issue closes OQ-24: the spec names the default and says which of the two a collection filled from input uses.

Done when:

- Iteration order equals insertion order after removals and re-insertions.
- `Add` of a present key returns `false` and changes nothing; assigning `none` through the indexer removes the key.
- UI test: a getter that reads `dict[key]` passes the getter check, and `@Cheap` on a function outside `Std` is `MR0254`.
- Over 16 runs of one program outside `mora test`, the hash of one string takes more than one value (all 16 agreeing by chance is negligible), and two runs under `mora test` with one seed hash it to the same value.
- A run test inserts 100,000 keys built to collide under the default hash with a known key, into a `Dictionary` made with a fresh random key, and the number of probes stays within a constant factor of the same test with random keys. As a control, the same keys inserted into a `Dictionary` that uses that known key do collide and the probe count grows, which shows the set is a real collision set.
- OQ-24 is closed in §24 with the measured cost of each hasher.

### M5.6 Enumeration protocol and foreach

Spec: [§7.11](docs/design/mora.md#711-statements), [§8.5](docs/design/mora.md#85-built-in-interfaces), [§10.10](docs/design/mora.md#1010-growable-collections-and-interior-views). Crates: `std`, `mora-mir`. Blocked by: M5.4.

`IEnumerable<T>` and `IEnumerator<T>`, `foreach` with `let`, `var` and `ref var`, a tuple designation in the head (`foreach (let (key, value) in pairs)`), version-checked enumerators (D-42), and boxed enumerators through `IEnumerable<T>` values.

Done when:

- Modifying a list inside `foreach` panics with `collection was modified during enumeration`.
- A method returning `IEnumerable<User>` works with `foreach` and the sequence operators.
- A run test iterates the collection expression `["a", "b", "c"]` with `foreach`.
- Run test: `foreach (let (word, count) in counts)` over a `List<(string, int)>` binds both parts; UI test: a name bound twice in the designation is `MR0180`.

### M5.7 Sequence operators

Spec: [§15.5](docs/design/mora.md#155-sequence-operators), [§10.9](docs/design/mora.md#109-closures). Crates: `std`. Blocked by: M5.6, M2.11.

The operators of §15.5 as iterator structs taking closure structs, buffering operators, and the `OrNull` variants.

Done when:

- A `Where`/`Select`/`Sum` chain allocates nothing (debug runtime counter).
- Each operator has run tests against expected sequences.

### M5.8 Console, environment, time and random

Spec: [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§15.6](docs/design/mora.md#156-console-environment-and-time). Crates: `std`, `mora-rt`. Blocked by: M5.2.

The World seam in the per-thread context (D-109), through which the clock and `Random.Shared` read, and the members of §15.6: `Console` input and error output, `Environment`, `Random`, `Math` and `MathF`, and the time types of D-111 (`Clock`, `Instant`, `MonoTime`, `TimeSpan`, `Stopwatch`, `LocalDate`, `LocalTime`, `LocalDateTime`, `Period`, `TimeError`) and lint `wall-clock-duration`. `@RequiresCapability` tags go on the members that use a capability, such as `Clock.Now()` and `MonoTime.Now()` (`clock`) and `Random.Shared` (`random`); value types and their arithmetic need none. To settle in the spec: which generator `Random.Shared` is, where the real World takes its seed and the hash keys of §15.3 (the operating system's random source, read once at startup), and whether its output is fit for tokens and keys; the API reference of each `Random` member says so.

Done when:

- `Clock.Now().ToString()` matches RFC 3339 in UTC, and `Instant.Parse` reads it back.
- `LocalDate.Of(2026, 2, 30)` is an `Err`, and January 31 plus `Period.Months(1)` is the last day of February.
- A UI test for `wall-clock-duration` and its fix.
- `Console.ReadLine()` returns `none` at end of input.
- Over 16 runs outside `mora test` of a program that prints `Random.Shared.Next(0, int.MaxValue)` ten times, the printed lines are not all the same (all 16 agreeing by chance is negligible), on every Tier 1 target; under `mora test --seed` they print the same lines.

### M5.9 Files, directories and paths

Spec: [§15.7](docs/design/mora.md#157-files-and-networking). Crates: `std`, `mora-rt`. Blocked by: M5.8.

`File`, `Directory`, `Path` and `IoError` of §15.7, with platform path rules on Windows. To settle in the spec: `Path.Combine` when a later part is rooted, has a drive or holds `..`; whether `Directory.Delete` removes a directory that is not empty, and that it removes a link itself and never what the link points to; and that a path with a NUL byte is an `IoError` on every target.

Done when:

- Run tests on a temporary directory on every Tier 1 target.
- `IoError.NotFound` for a missing file prints a message with the path.
- Run tests on every Tier 1 target for each rule the spec then states: `Path.Combine("data", "/etc/passwd")`, `Path.Combine("data", "..", "x")` and, on Windows, `Path.Combine("data", "C:\\x")` and a path with `/` and `\` mixed; `Directory.Delete` of a directory that holds a link to another directory leaves that other directory and its files in place; a path with a NUL byte returns `Err`.

### M5.10 JSON values, parser and writer

Spec: [§16.3](docs/design/mora.md#163-json-stdjson). Crates: `std`. Blocked by: M5.5.

`JsonValue`, `Json.Parse` over strings and bytes, compact and indented output, `JsonError` with line and column, the depth limit and duplicate-key rule.

Done when:

- The JSONTestSuite `y_` cases parse and `n_` cases fail.
- `{not json` fails with `at line 1, column 2: expected string`.
- 128 nested arrays parse and 129 fail with a `JsonError` naming the depth limit; 1,000,000 nested `[` in one input fail the same way, with no stack overflow, in `dev` and in `release`. The same holds for objects, and `JsonValue.ToString()` and dropping a `JsonValue` at the limit do not overflow the stack.
- `Json.Parse(ReadOnlySpan<byte>)` returns `JsonError` for bytes that are not valid UTF-8, inside a string and outside one, and for an escaped lone surrogate; no `string` it returns holds invalid UTF-8.
- `{"a":1,"a":2}` parses to one property `a` with the value 2.

### M5.11 The `@Json` macro

Spec: [§16.3](docs/design/mora.md#163-json-stdjson), [§9.9](docs/design/mora.md#99-standard-library-macros), [§6.12](docs/design/mora.md#612-newtypes). Crates: `std`. Blocked by: M5.10, M4.9, M3.21.

`@Json`, `IJsonSerialize`, `IJsonDeserialize`, `Json.Serialize` and `Json.Deserialize` with every mapping row of §16.3, `@JsonName`, `@JsonIgnore`, `@JsonStrict` and `@JsonTag`. A newtype maps as its underlying type, and deserializing one checks its refinement (§6.12). The generated deserializers and the skipping of unknown properties keep the nesting limit of §16.3.

Done when:

- Round-trip tests for each row of the §16.3 table.
- A missing non-optional property fails with `missing property "name"`.
- A `UserId` round-trips as a JSON number, and deserializing 70000 as a `Port` fails with a `JsonError` that names the refinement.
- `Json.Deserialize<Tree>` of an input nested 129 levels deep, for a recursive union and for a class that holds a `List` of itself, returns a `JsonError` naming the depth limit, and an input nested 1,000,000 levels deep does the same without a stack overflow, in `dev` and in `release`.
- An unknown property whose value is nested 1,000,000 levels deep is skipped with a `JsonError` at the limit, not by recursion.
- A discriminator that names no case, a number outside the field's integer type, and a `"type"` property that comes after the case fields each return a `JsonError`; none panics.

### M5.12 Testing library and `mora test`

Spec: [§15.9](docs/design/mora.md#159-testing), [§17.5](docs/design/mora.md#175-test-runner), [§20.4](docs/design/mora.md#204-executable-examples). Crates: `std`, `mora-driver`, `mora-cli`, `mora-testkit`. Blocked by: M5.7, M5.8, M5.11.

`@Test`, `Std.Testing.Assert`, `Assert.Panics` in a child process, unit tests in `src/` and integration tests in `tests/`, one process per test, `--leak-check` on by default, the output format of §17.5, and test seeds (D-109): one seed per run for `Random.Shared` (and for the scheduler once M6.3 lands), printed on failure and replayed with `--seed`. Doc-comment examples run as tests with the checks of §20.4, and `Assert.Snapshot`, `Assert.SnapshotJson` and `--bless` keep snapshot files in `tests/snapshots/` (D-114).

Done when:

- A project with a passing, a failing, a panicking and an ignored test prints `Passed: 1, Failed: 2, Ignored: 1` and exits 1.
- A test that fails for some values of `Random.Shared` prints its seed, and `--seed` with that seed fails it again with the same values.
- A doc-comment example with a wrong `// expect:` line fails under its example name, and an `// error: MRnnnn` example passes when that code is reported.
- `Assert.Snapshot` and `Assert.SnapshotJson` fail after the value changes, a second snapshot in the same test uses `<test>.2.snap`, `mora test --bless` rewrites every file the test uses, and the next run passes.

### M5.13 Signals and `@Observable`

Spec: [§15.8](docs/design/mora.md#158-events), [§9.9](docs/design/mora.md#99-standard-library-macros), [§6.11](docs/design/mora.md#611-attributes). Crates: `std`, `mora-hir`. Blocked by: M5.4, M2.10, M4.9.

`Signal<T>`, `Subscribe(move handler: (T) => ())` returning a unique `Subscription`, `SubscribeWeak`, and `Emit`. The `@Observable` macro (D-104) and the `@SoleWriter` attribute with `MR0253`. To settle in the spec: what `Emit` does when a handler it is calling drops a subscription of the same signal (its own or a later one), subscribes a new handler, or calls `Emit` on the same signal again.

Done when:

- Dropping the subscription stops delivery; a weak subscriber that died is skipped and removed.
- `SetX` on an `@Observable` class emits `XChanged` only when the value changed, and the signal of a `stored` getter that reads `X` after it.
- UI test: assigning an observed field outside `SetX`, inside or outside the class, is `MR0253` with a fix that calls `SetX`.
- Run tests for each of those three cases behave as the spec then says, and none panics with `collection was modified during enumeration`.

### M5.14 `Html`, `Url` and `Literal`

Spec: [§15.2](docs/design/mora.md#152-strings-and-text), [§7.14](docs/design/mora.md#714-interpolation-handlers). Crates: `std`. Blocked by: M5.2, M3.17.

The `Literal` members, and the `Html` and `Url` handlers of §15.2 (D-105): escaping by context for element text and quoted attribute values, percent-encoding by URL component, the panics for holes in other positions, nested `Html` holes, and `Unchecked`.

Done when:

- Escaping tests for text, quoted attributes, path segments, query names and values, and fragments, including non-ASCII text.
- A hole inside `<script>` panics with the context; an `Html` hole in element text is not escaped twice, and one in an attribute value is escaped.
- In `href`, a hole after `/users/` is percent-encoded, a hole at the start of the value that is not a `Url` panics, and a hole in `onclick` panics.
- Attribute names are matched without regard to case: a hole in `ONCLICK`, `OnLoad`, `STYLE` and `SrcDoc` panics, and `HREF`, `Src` and `FormAction` get the URL rule.
- A hole value holding `"`, `'`, `<`, `>`, `&` and a backtick cannot end a double-quoted or a single-quoted attribute value or start a tag in element text (the output is parsed back and holds one element and one attribute).
- A hole inside a comment, inside `<textarea>` or `<title>` markup that the literal leaves open, and directly after `<` panics with the context.
- In a URL attribute, a hole after the literal `JavaScript:` or `data:` panics, and a hole after `/` cannot produce a value that starts with `//` (§15.2). To settle in the spec: whether a value that starts with `/\` is refused too, since a browser reads it as `//`.

### M5.15 Logging and tracing

Spec: [§15.10](docs/design/mora.md#1510-logging-and-tracing), [§7.14](docs/design/mora.md#714-interpolation-handlers). Crates: `std`, `mora-rt`. Blocked by: M5.8, M5.10, M3.17.

`Log.Trace` to `Log.Error` through one log message handler per level (D-107), fields named after each hole's source text with `#2` for repeats, `MORA_LOG` and `MORA_LOG_FORMAT=json`, and `Trace.Span` with its end on drop. Spawned tasks inheriting the current span come with M6.3, and redacted `Secret<T>` holes with M5.16.

Done when:

- A disabled level evaluates no hole.
- JSON output has the template, one field per hole, the level, the time and the span; a span ends on an early `?` return with its duration.
- An event whose hole value holds a line break, a quote, a backslash, an escape character (U+001B) and U+2028, and one whose literal text holds the same, each produce exactly one line in the text format and one object that `Json.Parse` accepts in the JSON format, with those characters escaped; the same for a field name taken from a hole's source text and for a span's template.
- A value of 1 MiB in a hole is written without building the whole line in memory twice, or is cut at a length the spec states.

### M5.16 `Secret<T>`

Spec: [§15.11](docs/design/mora.md#1511-secrets), [§16.3](docs/design/mora.md#163-json-stdjson), [§6.14](docs/design/mora.md#614-effects). Crates: `std`, `mora-hir`. Blocked by: M5.11, M5.15, M3.18.

`Secret<T>` for `string` and `byte[]` (D-106): redacted `IDisplay`, `Use` through a span view, zeroing in the destructor, `Clone`, lint `secret-copied`, and `@Json` reading secrets and writing them only with `@JsonSecret`, leaving them out otherwise. The `secret` fact in `mora explain-effects` needs the effect analysis of §6.14 and arrives with it. To settle in the spec (§15.11): a comparison of a secret with a span or another secret whose time does not depend on where they differ.

Done when:

- Interpolating, logging and printing a secret show `Secret<string>(***)`.
- In `dev` and in `release`, a free hook sees the secret's bytes already zero when its buffer is released, for a secret made with `new`, one made by `Clone()` and one read by `@Json`; the zeroing is not removed as a dead store. Every buffer a secret outgrew while it was being read from JSON is zero when it is released too, and a deserialization that fails after the secret's field was read zeroes it.
- `Json.Serialize` of a type with a `Secret<T>` field and no `@JsonSecret` leaves the field out; with `@JsonSecret` it writes the plaintext.
- UI tests: returning the span from `Use`, storing it in a field and capturing it in an escaping closure are each rejected by the view check (§10.6); copying it with `ToArray()` or into a `string` inside `Use` warns `secret-copied`.
- `@Display`, `#Assert`, `Assert.Equal`, a panic message and a `Report` chain that hold a `Secret<T>` print `Secret<string>(***)`; a `JsonError` raised while reading a secret's property does not quote the input text.

### M5.17 `Report` and `Context`

Spec: [§11.5](docs/design/mora.md#115-reports), [§11.2](docs/design/mora.md#112-error-types). Crates: `std`. Blocked by: M5.2.

`Report` over `any IError` with context lines (D-108), `Context(message)` with a string or a lambda, `Report()`, the printed chain, and `MR0201` with fixes that add `.Report()` or `.Context("...")` for a bare `?` into `Report`.

Done when:

- A three-level chain prints the newest context first, then the error and the chain of its `Source()`.
- An `Ok` passed through `Context(() => ...)` never runs the lambda.
- A `Report` built from an internal error with two context lines has `PublicMessage` `none`; one built from an error with a `PublicMessage` returns exactly that text, with no context line in it; `Code` and `HttpStatus` equal the wrapped error's in both.

### M5.18 Standard library API reference

Spec: [§15](docs/design/mora.md#15-standard-library-mvp-surface). Crates: `xtask`, `std`. Blocked by: M5.9, M5.12, M5.13, M5.14, M5.15, M5.16, M5.17.

`cargo xtask std-docs` generates HTML reference pages from `std/` doc comments, linking backtick symbol names, and checks that every member §15 lists exists.

Done when:

- The check fails when a member named in §15 is missing from `std/`.

## M6: Async and HTTP

**Goal.** Async functions, the single-threaded executor, TCP, an HTTP/1.1 server with routing and JSON, and compile-time dependency injection: the REST API of §2.2 runs.

**Exit criteria.** The acceptance test of §20.2 passes on every Tier 1 target, and so do the dependency injection run tests of M6.9. The MVP is functionally complete.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M6.1](#m61-async-functions-and-futures-language-rules) | Async functions and futures: language rules | `async` | M5.7, M3.18 |
| [M6.2](#m62-async-lowering) | Async lowering | `async` | M6.1 |
| [M6.3](#m63-executor-tasks-and-cancellation) | Executor, tasks and cancellation | `async` | M6.2, M5.12, M5.15 |
| [M6.4](#m64-async-entry-points) | Async entry points | `async` | M6.3 |
| [M6.5](#m65-reactor-tcp-and-timers) | Reactor, TCP and timers | `rt` | M6.3 |
| [M6.6](#m66-http11-connections) | HTTP/1.1 connections | `http` | M6.5 |
| [M6.7](#m67-webapp-routing-and-results) | WebApp, routing and results | `http` | M6.6, M5.11, M5.15 |
| [M6.8](#m68-the-users-api-example-and-acceptance-test) | The users API example and acceptance test | `testkit` | M6.7 |
| [M6.9](#m69-dependency-injection) | Dependency injection | `check` | M6.7, M3.11, M3.15, M3.18 |

### M6.1 Async functions and futures: language rules

Spec: [§12.2](docs/design/mora.md#122-async-functions-and-futures), [§10.3](docs/design/mora.md#103-parameter-modes), [§7.4](docs/design/mora.md#74-propagation-). Crates: `mora-hir`, `std`. Blocked by: M5.7, M3.18.

`async` functions, methods and lambdas, `Future<T>` as a unique `@MustUse` cold type (D-27), `await`, `MR0312`, `MR0313`, `MR0400`, `MR0401` and `MR0404`, the `await e?` rule, and lint `blocking-in-async` (D-102).

Done when:

- UI tests for every code.
- `async (req: Request) => ...` infers its future type from the target.
- `blocking-in-async` reports an `async` function that reaches a test function marked `@Blocking` two calls down, and shows the path.

### M6.2 Async lowering

Spec: [§12.3](docs/design/mora.md#123-lowering), [§14.9](docs/design/mora.md#149-async-lowering). Crates: `mora-mir`. Blocked by: M6.1.

State machines per §14.9: parameters moved into the state, locals live across `await` as fields, numbered suspension points, per-state drop functions, and `MR0340` for borrows across `await`.

Done when:

- MIR snapshots of a lowered function with two `await`s.
- Dropping a future at each suspension point runs the right destructors (run test).

### M6.3 Executor, tasks and cancellation

Spec: [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§12.4](docs/design/mora.md#124-tasks), [§12.5](docs/design/mora.md#125-cancellation), [§15.10](docs/design/mora.md#1510-logging-and-tracing). Crates: `mora-rt`, `std`. Blocked by: M6.2, M5.12, M5.15.

Task scheduling through the World (D-109), with a choice among ready tasks seeded by the `mora test` seed, the single-threaded executor and wakers, `Task.WhenAll`, `WhenAny`, a fail-fast `Scope` (D-110), `SpawnDetached` with lint `detached-task`, `TaskHandle<T>`, `Delay`, `Yield`, inherited deadlines with `WithTimeout`, `WithDeadline` and `Remaining`, cancellation by drop, cancellation of detached tasks when `Main` returns, and spawned tasks inheriting the current log span (§15.10).

Done when:

- `WhenAll` of two 100 ms delays completes in under 150 ms.
- `WithTimeout` cancels its future at the deadline and the future's destructors run; a nested `WithTimeout` with a longer duration keeps the outer deadline.
- A scope child that returns `Err` cancels a sibling blocked in `Delay` and the scope returns that `Err`, without the body awaiting the child.
- Under one seed, a test with three tasks that interleave at `Yield` produces the same order on every run.
- An event logged inside a spawned task belongs to the span that was current where it was spawned.

### M6.4 Async entry points

Spec: [§4.6](docs/design/mora.md#46-entry-point), [§12.2](docs/design/mora.md#122-async-functions-and-futures). Crates: `mora-hir`, `mora-rt`. Blocked by: M6.3.

Async `Main`, top-level `await`, and their result handling with the exit codes of §4.6.

Done when:

- A top-level `await Task.Delay(...)` program runs and exits 0.

### M6.5 Reactor, TCP and timers

Spec: [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§15.7](docs/design/mora.md#157-files-and-networking). Crates: `mora-rt`, `std`. Blocked by: M6.3.

The `mio` reactor on epoll, kqueue and IOCP, timers, `TcpListener` and `TcpStream` as shared classes closing on drop (D-18), with `Memory<byte>` buffers (D-45), and `IoError` mapping.

Done when:

- An echo server and client in Mora exchange 10 MB on every Tier 1 target.
- Dropping a listener frees the port immediately for a new `Bind`.
- On every Tier 1 target, while a listener is open, a `Bind` to the same address and port from a second process returns `IoError.AddrInUse`; on Windows the listener is bound for exclusive use, so the immediate rebind after a drop does not come from an option that would let another process share the port.

### M6.6 HTTP/1.1 connections

Spec: [§16.1](docs/design/mora.md#161-http-server-stdnethttp). Crates: `std`, `mora-rt`. Blocked by: M6.5.

Request-head parsing with `httparse` in the runtime, keep-alive, `Content-Length` and chunked request bodies, `Expect: 100-continue`, `Connection: close`, `Date` and `Server` headers, every limit and timeout of §16.1 with its status code, and the global body memory budget with backpressure. To settle in the spec (§16.1): the framing of a request that has both `Content-Length` and `Transfer-Encoding`, several or malformed `Content-Length` values, a transfer coding other than a final `chunked`, chunk sizes that overflow, chunk extensions and trailers, bare line feeds and whitespace before a header's colon, with the status for each and the connection closed after it; a read timeout for request bodies and when a silent client's reservation is released; what happens to the connection after the limit of concurrent connections; and bytes that are not UTF-8 in the request line and in header values.

Done when:

- Protocol tests over raw TCP for each limit (413, 431, idle timeout).
- With the body budget set to 1 MiB, 64 clients each sending a 1 MiB body are served one budget at a time, and a client that waits more than 5 s gets 503 with `Retry-After: 1`.
- Pipelined requests on one connection are answered in order.
- Protocol tests over raw TCP for each framing case the spec then lists. In every case the bytes after the rejected request are never answered as a second request, and the connection is closed.
- A chunked body of more than the body limit gets 413 once the limit is passed, with at most the limit plus one 64 KiB step reserved; a `Content-Length` above the limit gets 413 before any of the body is read.
- 64 clients that send a head with `Content-Length: 1048576` and then nothing do not stop a 65th client's request with a body from being served once the body read timeout passes, also with `RequestTimeout` set to `none`.
- A request line or header value with bytes that are not valid UTF-8 gets the response the spec then states, and no `string` reached from `Request` holds invalid UTF-8 (checked with `FromUtf8` on its bytes).
- `cargo fuzz` target over the connection state machine: arbitrary bytes on a connection never panic the server, never grow memory past the head limit plus the body budget, and never produce a response to bytes that are part of an earlier request's body.

### M6.7 WebApp, routing and results

Spec: [§16.1](docs/design/mora.md#161-http-server-stdnethttp), [§16.2](docs/design/mora.md#162-httperror). Crates: `std`. Blocked by: M6.6, M5.11, M5.15.

`WebApp`, `WebAppOptions`, `--urls` and `MORA_URLS`, `Map*` with `move` handlers generic over any copyable `E : IError` (D-108), the routing rules of §16.1 (404, 405 with `Allow`, duplicate panics), `Request`, `RouteValue<T>`, `ReadTextAsync`, `ReadJsonAsync<T>`, `Response`, `Results`, `HttpError` with its conversions and `IError` members, error responses from `PublicMessage` and `HttpStatus` with `application/problem+json` for internal errors, request deadlines from `WebAppOptions.RequestTimeout` (503 when one passes), the request log as `Info` events in a span per request, and graceful shutdown on SIGINT, SIGTERM and Ctrl+Break. Handlers here take only the `Request`; M6.9 widens the map functions to take roots of the app's composition (§16.4). To settle in the spec (§16.1): whether `Path`, `Route` values and `{*rest}` are percent-decoded and when, how `%2F`, `%00`, invalid escapes, decoded bytes that are not UTF-8, `.` and `..` segments and empty segments are matched or rejected; what the server does with a response header name or value that holds a carriage return, a line feed or a NUL (it must not write it and must not panic); and the default listen address when neither an argument, `--urls` nor `MORA_URLS` gives one.

Done when:

- Routing table tests for literal-before-parameter order and `{*rest}`.
- Shutdown with an in-flight request completes it within 5 s.
- With `RequestTimeout` set to 100 ms, a handler that awaits `Task.Delay` for 1 s returns 503 at about 100 ms and its future's destructors run.
- An internal error returns 500 with a problem+json body holding only its code and trace id, and the log has the full chain under that trace id.
- Routing table tests for the request targets `/files/..%2Fsecret`, `/files/%2e%2e/secret`, `/files//x`, `/files/a%00b` and `/files/%ff` against `/files/{name}` and `/files/{*rest}`: each gives the status or the route values the spec then states, and no `Route` value holds invalid UTF-8.
- A handler that returns `Results.Created(location, value)` with `location` taken from the request and holding `\r\nSet-Cookie: x=1` produces no `Set-Cookie` header and no second response on the wire, and the server keeps running. The same for a header set through `Response.Headers`.
- `Results.BadRequest`, `Conflict` and `Problem` with a message that holds a quote, a backslash, a line break and a control character produce a body that `Json.Parse` reads back to the same message.
- A request path that holds a line break or an escape character, after decoding, appears in the request log on one line with those characters escaped (§15.10).

### M6.8 The users API example and acceptance test

Spec: [§2.2](docs/design/mora.md#22-the-mvp-acceptance-program), [§20.2](docs/design/mora.md#202-the-mvp-acceptance-test). Crates: `examples/users-api`, `mora-testkit`. Blocked by: M6.7.

`examples/users-api` exactly as §2.2 shows it, and the acceptance test of §20.2 as a required CI job on every Tier 1 target.

Done when:

- The transcript of §2.2 matches byte for byte on every Tier 1 target.
- The job is added to the org required checks.

### M6.9 Dependency injection

Spec: [§16.4](docs/design/mora.md#164-dependency-injection-stdinject), [§3.4](docs/design/mora.md#34-keywords), [§8.3](docs/design/mora.md#83-static-dispatch), [§16.1](docs/design/mora.md#161-http-server-stdnethttp). Crates: `mora-syntax`, `mora-hir`, `mora-check`, `mora-mir`, `std`. Blocked by: M6.7, M3.11, M3.15, M3.18.

Adding `services` blocks and their bindings to `mora.ungram` (`ServicesDecl`) together with their parser, so the grammar fuzz target of M1.19 keeps passing, bases with `override`, merged and conflicting bindings, `Base.IService` reuse, requirements, `+=` elements and `decorate` combined in base-list order, `@Cfg` on blocks and bindings, choice lambdas compiled to closed unions, factories that may be `async`, fail or return `none`, service classes checked once and instantiated per final composition, `IServices`, `NoServices`, `WebApp.Create<App>`, `ServiceHost.StartAsync` with `StartupError` and immortal singletons built at startup, `RunScoped` with provided values and kinds, `IRoot<S, R>`, `Scope<K>` and `per K`, lazy `() => T` parameters, generic bindings with declared parameters, generic blocks instantiated as bases, the escape check, one generated wiring function per root, map functions that take roots, `MR0700` to `MR0717`, and lint `unused-service` (D-122 to D-128, D-140).

Done when:

- UI tests for every code from `MR0700` to `MR0717`, reported by `mora check` and by the language server as well as by `mora build`, with `MR0703` and `MR0705` printing the whole chain from the root and `MR0712` naming both blocks.
- One binding reached through two bases compiles; two bases with different implementations or lifetimes for one service are `MR0712` until the combining block overrides it, also with `Base.IService`, whatever the base order.
- A generic block instantiated twice in one base list, with different type arguments, gives two bases: bindings that use the type parameter combine, a service they bind two ways is `MR0712`, an `override` matches after substitution or is `MR0702`, associated types are named by the substituted service, diagnostics name the instantiation, a host given a generic block is `MR0716`, and blocks that reach themselves through their bases by name, whatever the type arguments, are `MR0717`.
- Elements and decorators from two bases come in base-list order, then the block's own, so the block's own decorator is outermost (run test).
- `override singleton I = [X, Y];` and `override decorate` replace the inherited elements or decorators of a service, `+=` lines add after them, and `= []` empties a collection (run test).
- The MIR of a handler that takes a singleton and a scoped service shows direct constructor and method calls, and a choice binding compiles to a MIR `Switch` with a direct call per arm; `Assert.AllocatesAtMost` shows that running the handler with `RunScoped` allocates only the scoped service.
- A singleton that depends on a scoped service through a transient is `MR0705`, a root outside every `Connection` scope that needs a service per `Connection` is `MR0706`, and a root that returns its scoped service or hands it to a detached task is `MR0714`.
- In a run test, two child scopes inside one `Connection` scope share its service per `Connection`, each gets its own innermost scoped service, and destructors run in reverse construction order when each scope ends.
- A test composition that inherits the app's, overrides a file-backed service and carries `uses none` passes; without the override the build fails with `MR0250` and the call path.
- The acceptance test of §20.2 still passes with the map functions taking roots, and a warm `GET /users/1` still meets the allocation budget of §20.6.

## M7: Developer experience and v0.1

**Goal.** The editor, formatter, REPL, documentation and installers a developer expects, and the first public release.

**Exit criteria.** v0.1.0 is published on every channel of §19.3; §1.5 holds; the budgets of §20.6 are measured and either met or revised in the spec.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M7.1](#m71-formatter) | Formatter | `fmt` | M1.4, M1.20 |
| [M7.2](#m72-language-server-core-and-diagnostics) | Language server: core and diagnostics | `lsp` | M4.9 |
| [M7.3](#m73-language-server-navigation) | Language server: navigation | `lsp` | M7.2, M3.18 |
| [M7.4](#m74-language-server-editing-aids) | Language server: editing aids | `lsp` | M7.3 |
| [M7.5](#m75-vs-code-extension) | VS Code extension | `vscode` | M7.4 |
| [M7.6](#m76-repl) | REPL | `repl` | M4.9 |
| [M7.7](#m77-project-templates-and-mora-explain) | Project templates and `mora explain` | `cli` | M6.8 |
| [M7.8](#m78-complete-error-explanations) | Complete error explanations | `docs` | M6.8 |
| [M7.9](#m79-documentation-site) | Documentation site | `docs` | M7.8, M5.18 |
| [M7.10](#m710-release-workflow) | Release workflow | `release` | M0.11, M6.8 |
| [M7.11](#m711-installers-and-self-update) | Installers and self update | `release` | M7.10 |
| [M7.12](#m712-benchmarks-and-budgets) | Benchmarks and budgets | `ci` | M6.8, M1.20, M7.9 |
| [M7.13](#m713-v010-release) | v0.1.0 release | `release` | M7.5, M7.6, M7.7, M7.9, M7.11, M7.12 |
| [M7.14](#m714-parallel-code-generation-in-a-fixed-output-order) | Parallel code generation in a fixed output order | `codegen` | M1.12, M3.9 |
| [M7.15](#m715-reproducible-builds-checked-in-ci) | Reproducible builds checked in CI | `ci` | M7.14, M7.10 |

### M7.1 Formatter

Spec: [§17.7](docs/design/mora.md#177-formatter). Crates: `mora-fmt`, `mora-driver`, `mora-cli`. Blocked by: M1.4, M1.20.

`mora fmt` and `--check` with the layout of §17.7, idempotent and whitespace-only apart from trailing commas, `using` order and the parentheses around a propagation `?` before `:`, and the `format` lint of `mora check` (`MR2030`, D-115). `std/` and `examples/` are formatted in CI.

Done when:

- Idempotence and token-preservation tests over every `.mr` file in the repository.
- `c ? r? : d` formats as `c ? (r?) : d`, and `x?[i]` and `flag ? [1] : []` keep their spacing.
- `mora check` warns `MR2030` on an unformatted file, and `mora fix` formats it.
- `mora fmt --check` fails on an unformatted file.

### M7.2 Language server: core and diagnostics

Spec: [§17.8](docs/design/mora.md#178-language-server). Crates: `mora-lsp`, `mora-cli`. Blocked by: M4.9.

`mora lsp` on `lsp-server`: initialization, document sync, the shared database, debounced diagnostics with fixes as code actions, and the UTF-16 positions of M0.5. The limits of §9.7 apply to macros the server runs. To settle in the spec (§17.8): whether the server runs the macros of a project before the editor reports the folder as trusted, and what it shows in their place when it does not.

Done when:

- Scripted session tests: open, edit, receive and clear a diagnostic.
- Diagnostics arrive within the §20.6 budget on `users-api`.
- Scripted session tests open a project whose macro loops forever, one whose macro allocates without bound and one whose macro panics: each gives the §9.7 error at the use site within the time limit, the server answers a hover request afterwards, and its memory returns to within the §20.6 budget.
- The checked entry point of M1.16 reports `MR0001` for bytes that are not valid UTF-8 (a file the server reads from disk) and `MR0003` for a text of 4 GiB or more (a file or a document the editor sends), before any lexing and without ending the server; a malformed message on the transport is rejected without ending the server.

### M7.3 Language server: navigation

Spec: [§17.8](docs/design/mora.md#178-language-server). Crates: `mora-lsp`. Blocked by: M7.2, M3.18.

Hover with types, doc comments and effect sets (§6.14), go to definition (including into `std/`), find references, document and workspace symbols.

Done when:

- Scripted tests for each request on `users-api`.

### M7.4 Language server: editing aids

Spec: [§17.8](docs/design/mora.md#178-language-server), [§9.8](docs/design/mora.md#98-seeing-and-debugging-expansions). Crates: `mora-lsp`. Blocked by: M7.3.

Completion for members, locals, types, namespaces and keywords; signature help; formatting through `mora-fmt`; inlay hints for inferred types, effect sets after a signature, boxing sites, shared captures, implicit `IFrom` conversions, extension members, interface defaults and `static lazy` reads (D-115); and "Show expansion" for macros.

Done when:

- Scripted tests for completion after `.` and in an empty statement, and for each inlay hint kind.

### M7.5 VS Code extension

Spec: [§17.9](docs/design/mora.md#179-vs-code-extension). Crates: `editors/vscode`. Blocked by: M7.4.

The `wolfware.mora` extension: TextMate grammar, language configuration, snippets, the LSP client, status-bar version, the missing-toolchain prompt, and publishing to the Visual Studio Marketplace and Open VSX from the release workflow. The setting that names the `mora` executable is read from user settings only, never from a folder's settings. In a folder the editor has not marked as trusted the extension gives syntax colouring and snippets only and starts no process. The missing-toolchain prompt opens the install page of §19.3, runs its install script only in a trusted folder, and downloads nothing else. To settle in the spec (§17.9): these three rules.

Done when:

- Grammar snapshot tests over the spec examples.
- The extension's integration test starts `mora lsp` and receives diagnostics.
- Integration test: a folder whose `.vscode/settings.json` sets the executable path to a script in the folder does not run that script, in a trusted and in an untrusted folder.
- Integration test: in an untrusted folder no `mora` process is started; after the folder is trusted, `mora lsp` starts and diagnostics arrive.
- The `.vsix` holds no executable and its manifest declares limited support for untrusted folders; the store tokens are read only by the publishing job of M7.10.

### M7.6 REPL

Spec: [§14.16](docs/design/mora.md#1416-jit-and-the-repl). Crates: `mora-repl`, `mora-cli`. Blocked by: M4.9.

`mora repl` with `reedline`: declarations and statements, session globals, immediate execution through the JIT, `:type`, `:mir`, `:load`, `:reset`, history.

Done when:

- Scripted REPL tests feed input and compare output, including a class declared on one line and used on the next.

### M7.7 Project templates and `mora explain`

Spec: [§17.1](docs/design/mora.md#171-commands). Crates: `mora-cli`. Blocked by: M6.8.

`mora new` for apps, `--lib` and `--template web`, and `mora explain MRnnnn` printing the error page.

Done when:

- CLI tests: each template builds and its tests pass.
- `mora explain MR0310` prints the page.

### M7.8 Complete error explanations

Spec: [§17.6](docs/design/mora.md#176-diagnostics). Crates: `docs/errors`. Blocked by: M6.8.

A page for every code in use, written in plain terms, without the words §17.6 bans, with both examples executable.

Done when:

- `cargo xtask error-codes` passes with no exemptions.

### M7.9 Documentation site

Spec: [§1.4](docs/design/mora.md#14-anti-goals), [§17.10](docs/design/mora.md#1710-tools-for-programs-and-agents), [§19.3](docs/design/mora.md#193-installation). Crates: `docs/site`, `mora-cli`, `xtask`. Blocked by: M7.8, M5.18.

The site at `moralang.org` (D-130): install guide, language tour following §2, the standard library reference, the error pages, the macro guide and the ungrammar. `mora docs --llm`, served as `llms.txt`, and `mora-errors.jsonl` (D-112). The site serves `install.sh` and `install.ps1` of M7.11 at the addresses of §19.3. To settle in the spec (§19.3): which revision of the scripts the site serves (the ones of the latest release, the same bytes as the release assets, not the tip of `main`).

Done when:

- Every code sample on the site is run by `cargo xtask spec-examples`.
- The site deploys from `main` through GitHub Actions.
- `mora docs --llm` prints the same bytes on every run, and they match the site's `llms.txt` for that version.
- `mora-errors.jsonl` has one line per page in `docs/errors/`.
- A check after each deploy downloads `https://moralang.org/install.sh` and `install.ps1` and fails unless their SHA-256 equals the scripts of the release the spec names.
- The deploy runs only for `main`, in a job that has the pages and OIDC permissions and nothing else; the job that builds the site from a pull request has a read-only token and no secrets; every third-party action is pinned by commit SHA (§20.5).
- The install page and the pages that show an install command load no script or style from another origin.

### M7.10 Release workflow

Spec: [§19.2](docs/design/mora.md#192-toolchain-archive), [§19.3](docs/design/mora.md#193-installation). Crates: `.github/workflows`, `xtask`. Blocked by: M0.11, M6.8.

`cargo xtask dist` building the archive of §19.2 per host, the release workflow on tags producing archives, SHA-256 checksums and Sigstore signatures through GitHub OIDC, and GitHub Releases with notes. Jobs are split by what they may do: build jobs have a read-only token, no OIDC permission and no secrets; one signing job has the OIDC permission and runs no repository code beyond `cosign` on the built archives; one publishing job holds the release, tap, winget and extension-store credentials and runs after the signatures verify. To settle in the spec (§19.3): who may create a version tag (a tag ruleset on `v*` limited to maintainers, from a commit on `main`).

Done when:

- A release candidate tag produces signed archives for every host of §19.1.
- `cosign verify-blob` succeeds on each archive.
- The workflow starts only for tags that match the version pattern, checks that the tagged commit is on `main` and that the tag equals the workspace version, and fails otherwise before building.
- The workflow file declares `permissions: {}` at the top and grants each job only what the scope gives it; every third-party action is pinned by commit SHA (§20.5); the build jobs run with `--locked`.
- `SHA256SUMS` is produced from the archives after they are built and before signing, and is signed like an archive.
- `cosign verify-blob` with the identity of §19.3 fails on an archive signed by the same workflow run from a branch, and on one signed by another workflow of this repository.
- A ruleset on `v*` tags is in place and a push of such a tag by an account outside it is refused (recorded in the issue).

### M7.11 Installers and self update

Spec: [§19.3](docs/design/mora.md#193-installation). Crates: `install`, `mora-cli`. Blocked by: M7.10.

`install.sh` and `install.ps1` with checksum verification and optional `cosign` signature verification, the `wolfware-labs/tap` Homebrew formula, the winget manifest `Wolfware.Mora`, and `mora self update` with built-in Sigstore verification, all as the table in §19.3 specifies. To settle in the spec (§19.3): that `mora self update` refuses a release older than the installed one unless a version is asked for by name, and which hosts the scripts may follow a redirect to (release downloads leave `github.com`).

Done when:

- Install-script tests on clean CI runners for every Tier 1 target, with and without `cosign` on `PATH`.
- `mora self update` from a release candidate to the next one, and refusal of an archive whose signature identity does not match.
- `mora self update` refuses, each in its own test and before unpacking any file: an archive whose checksum differs; an archive with no signature; a valid Sigstore signature from another repository; one from another workflow of `wolfware-labs/mora-lang`; one from the release workflow run on a branch; a certificate whose OIDC issuer is not GitHub Actions; and a signature with no transparency-log entry. After each refusal `mora --version` and the files under `~/.mora` are unchanged.
- An update interrupted while it replaces files leaves a toolchain that runs: either the old one or the new one.
- Unpacking refuses an entry with an absolute path, a `..` segment or a link that points outside the install directory.
- `install.sh` and `install.ps1` verify the checksum, and the signature when `cosign` is present, before unpacking; on a mismatch they exit non-zero and leave `~/.mora` and `PATH` as they were. They refuse a download address that is not HTTPS, a redirect to plain HTTP, and a redirect to a host outside the list the spec gives.
- `install.sh` cut off at any byte, inside its final call included, runs nothing partial: its body is one function, and the final call is a form that cannot run when it is cut short (for example one that takes a fixed trailing argument). A test truncates the script at every byte. The same for `install.ps1`.

### M7.12 Benchmarks and budgets

Spec: [§20.6](docs/design/mora.md#206-budgets). Crates: `benches`, `evals`, `ci`. Blocked by: M6.8, M1.20, M7.9.

A `bench` workflow measuring every budget of §20.6, a Rust load generator for `users-api`, stored baselines, and the 10% regression gate. The agent eval suite of §20.6 (D-112): the tasks in `evals/`, `evals/models.toml`, and a release workflow that reports both rates per model against the previous release of the line. The job that builds and runs a pull request's code has a read-only token and no secrets; a separate job that runs no repository code posts the measurements, reading them as data. In the eval workflow the step that calls a model holds that model's key and runs no generated code, and the step that builds and tests the model's answer has no key, no token and no network.

Done when:

- The workflow posts the measurements on pull requests.
- A synthetic slowdown fails the gate.
- The eval workflow runs on a release candidate and reports both rates for every model in `evals/models.toml`.
- A pull request from a fork gets its measurements posted, and the job that ran its code had no write permission and no secret (shown by the workflow's `permissions` and by a run from a fork).
- A measurement file that holds Markdown, HTML or a mention is posted as inert text.
- An eval task whose model answer reads the environment and opens a socket finds no credential and no network.

### M7.13 v0.1.0 release

Spec: [§1.5](docs/design/mora.md#15-what-the-mvp-is), [§22](docs/design/mora.md#22-roadmap). Crates: repository. Blocked by: M7.5, M7.6, M7.7, M7.9, M7.11, M7.12.

Checks §1.5 end to end on fresh machines, revises the §20.6 budgets with measured values, updates the spec's status line, writes the edition policy into the spec, and publishes v0.1.0.

Done when:

- A developer who has not seen Mora installs it, opens §2.2 in VS Code and gets diagnostics, hover and go-to-definition (recorded in the issue).
- Every channel of §19.3 serves v0.1.0, and on a fresh machine each passes the integrity and authenticity check of its row of §19.3: the scripts with and without `cosign`, `mora self update` from the last release candidate, the pinned checksums of the Homebrew formula and the winget manifest against `SHA256SUMS`, and `cosign verify-blob` on every archive (recorded in the issue).
- The spec says what an edition may change, how an edition is chosen in `mora.toml`, and that every edition change ships with a `safe` fix.

### M7.14 Parallel code generation in a fixed output order

Spec: [§14.13](docs/design/mora.md#1413-cranelift-lowering), [§14.1](docs/design/mora.md#141-pipeline), [§20.6](docs/design/mora.md#206-budgets), [§24](docs/design/mora.md#24-open-questions). Crates: `mora-codegen`, `mora-driver`. Blocked by: M1.12, M3.9.

Cranelift code generation of monomorphized bodies runs on a thread pool, one body per job, and the results are written in a fixed order that does not depend on which job finished first, so the object files are the same bytes whatever the number of threads and a build stays a pure function of its inputs (D-119). Code generation is the phase that grows with the number of instantiated bodies, and once MIR is optimized it has no dependencies between bodies. A per-function cache of generated code is not part of this: it stays under OQ-17 and comes only if the budgets of §20.6 fail with parallel code generation in place. To settle in the spec: whether the order key is the mangled symbol name (§13.7) or the order in which monomorphization reached each body, and whether the pool also runs MIR optimization per body.

Done when:

- §14.13 says that bodies are generated independently and in what order their code, data and debug sections are written.
- `mora build --jobs <n>` sets the number of threads, defaulting to the available cores.
- `users-api` built with `--jobs 1` and `--jobs 8` gives byte-identical object files in `dev` and `release`; executables are compared in M7.15, once paths, clocks and timestamps are normalized.
- The `bench` workflow of M7.12 records the `users-api` build time with one job and with all cores.

### M7.15 Reproducible builds checked in CI

Spec: [§19.2](docs/design/mora.md#192-toolchain-archive), [§14.14](docs/design/mora.md#1414-linking), [§11.4](docs/design/mora.md#114-panics), [§20.5](docs/design/mora.md#205-continuous-integration). Crates: `.github/workflows`, `xtask`, `mora-driver`. Blocked by: M7.14, M7.10.

D-119 says a build is a pure function of its inputs, and this tests it: CI builds the same sources twice in different directories and compares the bytes. The release archives are signed through Sigstore (M7.10), and a byte-for-byte rebuild is what lets anyone check that a signed archive matches its sources. The usual causes of drift are absolute paths in panic locations and debug information, timestamps in archives, and output order, which M7.14 fixes for parallel code generation. Rebuilding published releases by a second party belongs with the registry and signing work of M12.

Done when:

- §19.2 states the guarantee and lists its inputs: sources, lockfile, toolchain version, target and profile.
- Panic locations (§11.4) and debug information name source paths relative to the project root, so the checkout path does not reach the output.
- A CI job builds `users-api` in `dev` and `release` twice, from two checkout paths and with different temporary directories and clocks, and fails unless the outputs are byte-identical.
- The same job runs `cargo xtask dist` twice and compares the toolchain archives of §19.2.

## Post-MVP milestones

| Milestone | Scope | Spec |
|---|---|---|
| M8: WebAssembly and libraries | Wasm targets, the `.wasm` library format with `mora.meta`, `mora.ir`, `mora.deps`, `mora.caps` and `mora.abi`, linking modes and ABI checks (OQ-15), and a stable plugin ABI at an opt-in boundary: `[abi] stable` interface packages, `@Frozen` types, and native and Wasm plugins loaded from one interface; WASI 0.3 with async in the Component Model for `wasm32-wasi`, and `mora bindgen wit` to import a WIT interface as Mora declarations. **Security questions to settle with the spec update**: (1) Which checks does the consuming compiler run on a library's `mora.ir` and `mora.meta` (the MIR validator of §14.7, types, moves and borrows, effects) so that a crafted library cannot break §10.14? (2) Does a build ever link precompiled code, or always compile from the checked `mora.ir`, and what proves the code matches it? (3) Does each custom section get a decoder that rejects malformed input, with size limits and a fuzz target (§20.1)? | [§18.1](docs/design/mora.md#181-webassembly-target), [§18.2](docs/design/mora.md#182-the-library-format), [§18.3](docs/design/mora.md#183-linking-libraries), [§18.4](docs/design/mora.md#184-abi-stability), [§24](docs/design/mora.md#24-open-questions) |
| M9: Packages and capabilities | URL and git dependency sources, inline imports in scripts, lockfile hashes, `mora fetch`, and capability grants enforced on wasm and native targets, narrowed per dependency, with sandboxed runs. **Security questions to settle with the spec update:** (1) Where is the content hash of an inline `import` kept, so that a script's dependency is pinned as a manifest dependency is (§18.5)? (2) A prebuilt `.wasm` from a git release: is it checked against a build of the tagged source, or is the source always built? (3) Do URL and git sources accept only `https`, and is `mora fetch` the only command that reaches the network, with `mora build` and `mora check` never doing so? (4) When `mora fetch` builds a dependency from source, does it run only that dependency's macros, inside the sandbox of §9.7, and nothing else of the dependency? For a prebuilt `.wasm`, what is executed, if anything, and what is verified before use (see the question above)? | [§18.5](docs/design/mora.md#185-imports-manifest-and-lockfile), [§18.6](docs/design/mora.md#186-capabilities) |
| M10: Threads | `shared` classes with atomic counts, sendable checks, `Thread`, `Task.Run`, `Mutex`, `Atomic`, `Channel`, a multi-threaded executor (OQ-6, OQ-7, OQ-1), supervised worker processes as a framework option, sending a freshly built object graph to another thread as one region, and `Parallel.For` and `Parallel.Map`, whose bodies the compiler checks for effects so that their results are deterministic. **Security questions to settle with the spec update:** (1) How the memory-model suite (§20.3) covers threads: a case per sendable rule of §12.6, and a debug-runtime check that aborts when a plain count is updated from two threads. (2) What rule keeps `static mut` state on the main thread (§4.7, OQ-6), and what is its diagnostic? (3) `SECURITY.md` lists a data race in safe code as in scope: does the milestone's exit test include it? | [§12.6](docs/design/mora.md#126-threads-post-mvp), [§24](docs/design/mora.md#24-open-questions) |
| M11: Language growth | Class inheritance, variance, `yield return`, retroactive implementations, arenas, allocators, user `unsafe`, `decimal`, conditionally async higher-order functions (`async?`), more macro kinds (OQ-3, 4, 5, 9, 10, 11, 12, 13), value parameters for generics (integers, `bool` and enums), and must-consume unique classes. **Security questions to settle with the spec update:** (1) Does a package that contains `unsafe` code need a grant (§18.6), as one that uses `@Import` needs `ffi` (§13.9)? (2) `SECURITY.md` scopes memory safety to code outside the standard library's `unsafe` dialect: is that scope restated when user `unsafe` lands? | [§9.10](docs/design/mora.md#910-not-in-the-mvp), [§10.12](docs/design/mora.md#1012-arenas-post-mvp), [§24](docs/design/mora.md#24-open-questions) |
| M12: Registry and signing | The package registry and registry dependency sources, Sigstore-signed publishing, the transparency log and mirrors (OQ-16), and rebuild verification of published releases. **Security questions to settle with the spec update:** (1) What does `mora fetch` verify before it writes `mora.lock`: the signature, the signer's identity, the transparency-log entry, and which signers a package name allows? (2) A mirror is untrusted for content (the lock hash and the log decide): how does a project pin the registry a name comes from, so a public package cannot replace a private one? (3) Name policy (OQ-16): case-only and look-alike names, and reuse of a deleted name. (4) What does a build do with a withdrawn locked version, and how is a known-vulnerable one reported? (5) How is a client protected from a replayed old index? (6) Does publishing run no package code on the registry's side, and a consumer's build only macros, inside the sandbox of §9.7? | [§18.7](docs/design/mora.md#187-registry), [§24](docs/design/mora.md#24-open-questions) |
| M13: Backends and targets | LLVM release backend, Component Model export, Windows debug information, embedded targets (OQ-2), and an aliasing model that lets the LLVM backend emit `noalias` facts. | [§18.3](docs/design/mora.md#183-linking-libraries), [§19.1](docs/design/mora.md#191-targets), [§24](docs/design/mora.md#24-open-questions) |
| M14: Contracts and checked types | `contract` sections with `requires`, `fails`, `ensures` and `invariant`, `where` predicates on newtypes, property tests generated from types and contracts, unit algebra, and compile-time evaluation of effect-free functions. | [§6.12](docs/design/mora.md#612-newtypes), [§6.13](docs/design/mora.md#613-contract-sections-post-mvp), [§14.6](docs/design/mora.md#146-constant-evaluation), [§15.9](docs/design/mora.md#159-testing) |
| M15: Test and run tooling | The simulated World, sound test caching and flaky-test triage from effects, `mora run --record` and `mora replay`, hot reload of function bodies in dev builds, mutation testing with `mora test --mutate`, coverage reports and coverage-guided fuzzing with a committed corpus, PCT scheduling in `mora test`, and error return traces in `dev` builds. | [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§15.9](docs/design/mora.md#159-testing), [§17.5](docs/design/mora.md#175-test-runner) |
| M16: Agent tooling and API evolution | `mora query`, `mora context` and `mora mcp`, JSON Schema from types and `@Tool` signatures as MCP tools, semantic diff with compiler-checked semver (contract changes count once M14 lands), editions with `mora migrate`, typed holes (`Todo()` reports its expected type and what fits), grammar export for constrained generation, and `mora query complete`. | [§6.14](docs/design/mora.md#614-effects), [§16.3](docs/design/mora.md#163-json-stdjson), [§17.1](docs/design/mora.md#171-commands), [§18.4](docs/design/mora.md#184-abi-stability) |
| M17: Standard library growth | Time zones and the tz database, typed configuration with `@Config`, `Retry` policies, `@CancelSafe` and `Task.Shield`, and `Task.Race`, `Task.Quorum` and collect-all completion policies. | [§12.5](docs/design/mora.md#125-cancellation), [§15.6](docs/design/mora.md#156-console-environment-and-time), [§15.12](docs/design/mora.md#1512-what-is-not-in-the-mvp) |
| M18: Optimizer | General reuse when unique, merging identical MIR bodies, `Vector128<T>`, closed-world devirtualization of `any I` and function values, checked `@NoAlloc` and `scoped`, and acyclic types inferred from the type graph. | [§10.13](docs/design/mora.md#1013-what-the-optimizer-guarantees), [§14.19](docs/design/mora.md#1419-reuse-when-unique), [§6.14](docs/design/mora.md#614-effects), [§13.1](docs/design/mora.md#131-the-runtime-library), [§14.14](docs/design/mora.md#1414-linking) |

