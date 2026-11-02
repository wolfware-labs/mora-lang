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

`FileId`, `Span`, `TextRange`, a `LineIndex` that converts offsets to line and column in UTF-8 bytes and in UTF-16 code units (for the LSP), string interning, and the base `salsa` database with file-text and project inputs.

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

Statements of §7.11, Pratt parsing with the precedence table of §7.1, the postfix `?` disambiguation and `await e?` rule of §7.4, generic argument disambiguation and cast rules of §14.4, lambdas with capture lists, patterns of §7.8, switch expressions and statements, collection expressions, interpolation holes, `with`, `code { }` quotes and `#Name(...)` macro calls. A `cargo fuzz` target checks that parsing never panics and is lossless.

Done when:

- Snapshot tests for each row of §7.1 and each pattern of §7.8.
- `a ? b : c`, `x?`, `x?.y`, `await f()?` and `F<A, B>(c)` parse as §7.4 and §14.4 say.
- The fuzz target runs for 10 minutes in the nightly job without failures.

### M1.5 Withdrawn

Spec: [§3.4](docs/design/mora.md#34-keywords), [§23](docs/design/mora.md#23-decisions-register). Crates: `mora-syntax`, `mora-diag`. Blocked by: M1.4, M0.7.

Withdrawn by D-59: the compiler knows only Mora syntax, so code in another language's syntax gets the ordinary syntax error. The codes this deliverable added were removed.

### M1.6 Item tree, name resolution and entry points

Spec: [§4](docs/design/mora.md#4-program-structure), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`, `mora-syntax`. Blocked by: M1.3.

Per-file item trees and the project def map: namespaces from folders with `MR0121` (D-96), nested types, `using`, `using static`, aliases, `global using`, the implicit prelude, the folder-default visibility of §4.5 (D-97), dependency names for path dependencies (resolved in M3.14), and entry point detection with `MR0124` and `MR0125`.

Done when:

- UI tests for ambiguous names, inaccessible members across folders (`MR0123`), folder names (`MR0121`) and both entry-point errors.
- A `salsa` test shows that editing a function body does not recompute the def map.

### M1.7 Types and signatures

Spec: [§5.1](docs/design/mora.md#51-kinds-of-type), [§5.2](docs/design/mora.md#52-primitive-types), [§5.5](docs/design/mora.md#55-tuples), [§6.2](docs/design/mora.md#62-structs). Crates: `mora-hir`. Blocked by: M1.6.

The type representation for primitives, `string`, unit, tuples, structs and function signatures (generic and class types arrive in M2 and M3), lowering of syntax types to types, and the signature query per item.

Done when:

- Unit tests resolve each primitive keyword and tuple form to its type.
- Unknown type names report an error with a suggestion of the closest name.

### M1.8 Body lowering to HIR

Spec: [§7](docs/design/mora.md#7-expressions-and-statements), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`. Blocked by: M1.7.

Lowering of function bodies and top-level statements to HIR expressions and statements, with scopes, local declarations of §7.10, shadowing check `MR0180`, and name resolution of locals, parameters, members and items.

Done when:

- UI tests for `MR0180` and unresolved names.
- HIR dumps of the M1 run tests are snapshot-tested.

### M1.9 Type checking: core

Spec: [§5.10](docs/design/mora.md#510-conversions), [§7](docs/design/mora.md#7-expressions-and-statements), [§14.5](docs/design/mora.md#145-name-resolution-and-types). Crates: `mora-hir`, `mora-check`. Blocked by: M1.8.

Bidirectional checking of literals (target typing of §3.5), numeric conversions and promotions of §5.10 and §7.12, operators, calls with overload resolution and named and optional arguments, `let`/`var` locals and `MR0182`, `if`, `while`, `do`, `for`, `break`, `continue`, `return`, and definite assignment (`MR0181`).

Done when:

- UI tests for each conversion row of §5.10 and each error code above.
- Run tests for integer promotion and float target typing (`float x = 1.5;`).

### M1.10 MIR and lowering from HIR

Spec: [§14.7](docs/design/mora.md#147-mir). Crates: `mora-mir`. Blocked by: M1.9.

The MIR of §14.7 (bodies, blocks, places, operands, rvalues, statements, terminators), the stable textual dump behind `--emit mir`, and lowering of the M1 subset from HIR.

Done when:

- MIR snapshot tests for each M1 run test.
- A MIR validator (types match, every block terminated) runs after every pass in debug builds of the compiler.

### M1.11 Constant evaluation

Spec: [§4.7](docs/design/mora.md#47-static-state), [§14.6](docs/design/mora.md#146-constant-evaluation). Crates: `mora-mir`. Blocked by: M1.10.

A MIR interpreter for `const` values, `static mut` initializers and default parameter values with the subset of §14.6 and `MR0460` outside it.

Done when:

- Run tests using constants in array sizes, defaults and string concatenation.
- A UI test for `MR0460`.

### M1.12 Cranelift code generation

Spec: [§13.5](docs/design/mora.md#135-value-layouts), [§13.7](docs/design/mora.md#137-calling-convention-and-symbols), [§14.13](docs/design/mora.md#1413-cranelift-lowering). Crates: `mora-codegen`. Blocked by: M1.10, M0.13.

A `Backend` trait taking optimized MIR and a target description, and its Cranelift implementation: lowering of MIR for scalars, structs, tuples, calls, control flow, overflow-checked arithmetic and panics as `noreturn` runtime calls; object files through `cranelift-object`; in-process execution through `cranelift-jit`; struct layout of §13.5 computed from the target description and the calling convention of §13.7.

Done when:

- The run harness executes every M1 run test through the JIT and through an object file linked by M1.14, and compares outputs.
- Overflow in a debug build panics with the message of §11.4.

### M1.13 Runtime: startup, output and panics

Spec: [§13.1](docs/design/mora.md#131-the-runtime-library), [§13.8](docs/design/mora.md#138-startup-and-process-exit), [§11.4](docs/design/mora.md#114-panics). Crates: `mora-rt`. Blocked by: M0.10.

Process entry, argument collection, buffered stdout and stderr, panic message formatting (`panic at file:line:col: message`), backtraces when `MORA_BACKTRACE=1` or in the debug runtime, abort with exit code 101, stack overflow detection of §13.10, and immortal string literal objects.

Done when:

- Run tests for exit codes 0, an `int` from `Main`, and 101 on panic.
- A deep-recursion test prints `stack overflow` on every Tier 1 target.

### M1.14 Linking on Linux, macOS and Windows

Spec: [§14.14](docs/design/mora.md#1414-linking). Crates: `mora-link`. Blocked by: M0.11, M1.12, M1.13.

The linker driver of §14.14: `ld.lld` with musl for a fully static Linux executable, `xcrun ld` with `libSystem` on macOS, `lld-link` with MinGW pieces on Windows, an 8 MiB main-thread stack on Windows, and a clear error with the install command when a required tool is missing.

Done when:

- Hello world links and runs on every Tier 1 runner.
- On Linux, `file` reports the executable as statically linked.
- Removing the linker from `PATH` on macOS yields the `xcode-select --install` hint.

### M1.15 Standard library bootstrap and console output

Spec: [§15.1](docs/design/mora.md#151-prelude-std), [§15.6](docs/design/mora.md#156-console-environment-and-time), [§14.15](docs/design/mora.md#1415-standard-library-compilation). Crates: `std`, `mora-driver`. Blocked by: M1.9, M1.13.

The `std/` source tree embedded in the toolchain, the prelude as an implicit `global using Std;`, `Console.Write` and `Console.WriteLine` for primitives and `string`, `IDisplay` for primitives, and interpolated strings lowered to `Formatter` writes, without format specifiers yet.

Done when:

- §2.1 prints `Hello, world!`.
- Run tests interpolate every primitive type.

### M1.16 Driver and the build, run and check commands

Spec: [§14.15](docs/design/mora.md#1415-standard-library-compilation), [§17.1](docs/design/mora.md#171-commands), [§17.2](docs/design/mora.md#172-manifest), [§17.3](docs/design/mora.md#173-project-layout), [§17.4](docs/design/mora.md#174-profiles). Crates: `mora-driver`, `mora-pkg`, `mora-cli`. Blocked by: M1.14, M1.15.

Reading `mora.toml` (the `[project]` keys and profiles; unknown keys are `MR0500`), discovering `src/**/*.mr` and rejecting files of 4 GiB or more with `MR0003` (§3.1), the dev and release profiles of §17.4, the `target/` layout, the on-disk cache of the standard library's checked MIR, and `mora build`, `mora run` and `mora check`.

Done when:

- CLI tests build and run a temporary hello-world project in both profiles.
- A second build reuses the standard-library cache (asserted through a cache-hit counter in `--verbose` output).

### M1.17 Debug information and symbol names

Spec: [§13.7](docs/design/mora.md#137-calling-convention-and-symbols), [§14.13](docs/design/mora.md#1413-cranelift-lowering). Crates: `mora-codegen`, `mora-cli`. Blocked by: M1.12.

The mangling scheme of §13.7, `mora demangle`, demangled names in backtraces, and DWARF line tables from MIR spans on Linux and macOS.

Done when:

- A panic backtrace in a debug build shows `UsersApi.Program.Main` style names with `src/Program.mr:LINE`.
- `gdb` or `lldb` stops at a breakpoint set by `file:line` (manual check recorded in the pull request).

### M1.18 Third instruction set under emulation

Spec: [§19.1](docs/design/mora.md#191-targets), [§19.4](docs/design/mora.md#194-portability). Crates: `targets`, `.github/workflows`. Blocked by: M1.16.

The `riscv64gc-linux` target description, `mora-rt` and the musl sysroot for it, and a CI job that cross-builds the run-test suite and runs it under QEMU user-mode emulation.

Done when:

- The run tests pass on `riscv64gc-linux` under QEMU as a required check.

### M1.19 Grammar-driven parser fuzzing

Spec: [§14.4](docs/design/mora.md#144-lexer-and-parser), [§20.1](docs/design/mora.md#201-test-layers). Crates: `mora-syntax`, `fuzz`. Blocked by: M1.4.

A generator that writes programs from `mora.ungram`, adding parentheses where the rules of §14.4 and §7.4 would read the text another way, and a `cargo fuzz` target that parses each program, compares the tree with the shape it was generated from and prints it back (D-115).

Done when:

- The target runs for 10 minutes in the nightly job without failures.
- A unit test with a parser rule that disagrees with `mora.ungram` shows the check reporting the mismatch.

### M1.20 Fix applicability, `mora fix` and watch mode

Spec: [§17.1](docs/design/mora.md#171-commands), [§17.6](docs/design/mora.md#176-diagnostics), [§17.10](docs/design/mora.md#1710-tools-for-programs-and-agents). Crates: `mora-diag`, `mora-driver`, `mora-cli`. Blocked by: M1.16.

`applicability` on every fix and `schema` on every JSON object (D-113), the deterministic order of diagnostics, `mora fix` with `--code` and `--applicability`, and `mora check --watch` with its summary per round.

Done when:

- CLI tests: `mora fix` applies a `safe` fix and leaves a `review` one, applies both with `--applicability review`, and `--code` limits it to one code.
- Two runs of `mora check --message-format json` on the same project print the same bytes.
- A watch-mode test edits a file and receives a new round that ends with the summary object.

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

### M2.2 Positional structs

Spec: [§6.2](docs/design/mora.md#62-structs), [§6.5](docs/design/mora.md#65-positional-structs). Crates: `mora-hir`. Blocked by: M2.1.

Positional structs with their generated constructor, public read-only fields and deconstruction, and `with` expressions on copyable structs. Equality, hashing and printing come from macros in M4.9.

Done when:

- Run tests for `with` on plain and positional structs, and for deconstruction.

### M2.3 Shared objects and reference counts in the runtime

Spec: [§13.3](docs/design/mora.md#133-shared-objects), [§13.4](docs/design/mora.md#134-strings-and-arrays). Crates: `mora-rt`, `mora-codegen`. Blocked by: M2.1.

The object header of §13.3, inline retain and release sequences, `mora_rt_drop` with the iterative release work list, weak count handling, immortal objects, `T[]` layout and allocation, and reference-count overflow abort.

Done when:

- Releasing a linked list of 1,000,000 nodes does not overflow the stack.
- Retaining an immortal object never writes to it (the test places one in read-only memory).
- Layout unit tests compute every header, string, array and union layout for both the 64-bit targets and the 32-bit test description.

### M2.4 Parameter modes and `this`

Spec: [§10.3](docs/design/mora.md#103-parameter-modes). Crates: `mora-hir`, `mora-mir`. Blocked by: M2.1.

`in`, `ref` and `move` parameters, call-site `ref`/`move`, struct `mut` methods and `MR0141`, `move` methods, and the representations of §10.3.

Done when:

- UI tests for each mode's errors.
- Run tests passing a large struct by `in` and by `ref`.

### M2.5 Move checking and drop elaboration

Spec: [§10.5](docs/design/mora.md#105-moves), [§10.8](docs/design/mora.md#108-drops-and-destructors), [§14.8](docs/design/mora.md#148-mir-checks). Crates: `mora-mir`. Blocked by: M2.4.

The move check and drop elaboration of §14.8 steps 1 and 2: `MR0300` to `MR0303`, drop flags for maybe-moved locals, drops at every scope exit, temporaries dropped at the end of their statement, and `Drop(x)`.

Done when:

- UI tests for every move error, each showing the move and the use.
- Drop-order run tests for early return, `break`, `continue` and moves in one branch.

### M2.6 Destructors and drop order

Spec: [§6.9](docs/design/mora.md#69-members), [§10.8](docs/design/mora.md#108-drops-and-destructors). Crates: `mora-hir`, `mora-mir`, `mora-rt`. Blocked by: M2.3, M2.5.

`~T()` declarations, `MR0142` and `MR0360`, destructor then fields in declaration order, struct destructors making the struct move-only, the resurrection check of D-46, and abort on a panic inside a destructor.

Done when:

- Run tests whose destructors print match the order of §10.8.
- A resurrecting destructor aborts with the message of §6.9.

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

### M2.9 Spans, views and memory slices

Spec: [§5.9](docs/design/mora.md#59-views), [§10.6](docs/design/mora.md#106-views). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M2.8.

`Span<T>`, `ReadOnlySpan<T>`, `ref struct`, `stackalloc`, range slicing, `Memory<T>` and `ReadOnlyMemory<T>`, and the view check of §10.6: `MR0340` to `MR0344`, hidden keep-alives that last to the view's last use, and the `where T : ref struct` constraint.

Done when:

- UI tests for each view error, with the wording of §10.6.
- `let s = box.Data.AsSpan();` compiles and the snapshot lives until `s`'s last use.

### M2.10 Weak references and the cycle lint

Spec: [§10.7](docs/design/mora.md#107-weak-references). Crates: `mora-hir`, `mora-mir`, `mora-rt`, `mora-check`. Blocked by: M2.3.

`weak T` fields and locals, reads as `T?` (options arrive in M3.5; until then a built-in `weak` read form is used in tests), `MR0350`, and the `strong-cycle` lint `MR2020`.

Done when:

- A run test reads `none` after the target dies.
- Lint tests: a parent and child pair is reported; a self-referential tree is not.

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

Last-use moves, retain/release pair cancellation, borrowed forwarding and immortal elimination, delivering the seven guarantees of §10.13.

Done when:

- One MIR snapshot test per guarantee of §10.13 with zero `Retain`/`Release` statements.
- The M2 run tests still pass and stay leak-free.

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
| [M3.3](#m33-patterns-and-switch) | Patterns and switch | `hir` | M3.2 |
| [M3.4](#m34-exhaustiveness-and-reachability) | Exhaustiveness and reachability | `check` | M3.3 |
| [M3.5](#m35-options-none-and-flow-narrowing) | Options, `none` and flow narrowing | `hir` | M3.4 |
| [M3.6](#m36-operators-conversions-and-indexers) | Operators, conversions and indexers | `hir` | M3.3 |
| [M3.7](#m37-results-and--propagation) | Results and `?` propagation | `hir` | M3.5, M3.6 |
| [M3.8](#m38-generics-and-constraints) | Generics and constraints | `hir` | M3.1 |
| [M3.9](#m39-monomorphization) | Monomorphization | `mir` | M3.8 |
| [M3.10](#m310-interfaces-and-static-dispatch) | Interfaces and static dispatch | `hir` | M3.9 |
| [M3.11](#m311-dynamic-interface-values) | Dynamic interface values | `codegen` | M3.10 |
| [M3.12](#m312-tuples-collection-expressions-and-smaller-expressions) | Tuples, collection expressions and smaller expressions | `hir` | M3.9 |
| [M3.13](#m313-statics) | Statics | `hir` | M1.11, M3.5 |
| [M3.14](#m314-local-path-dependencies-and-library-projects) | Local path dependencies and library projects | `driver` | M3.10 |
| [M3.15](#m315-attributes-cfg-lint-levels-and-obsolete) | Attributes: `Cfg`, lint levels and `Obsolete` | `check` | M3.14 |
| [M3.16](#m316-platform-capabilities-and-standard-library-layers) | Platform capabilities and standard library layers | `std` | M3.15 |
| [M3.17](#m317-interpolation-handlers) | Interpolation handlers | `hir` | M3.10 |
| [M3.18](#m318-effect-inference-and-uses-bounds) | Effect inference and `uses` bounds | `check` | M2.11, M3.11, M3.16, M3.17 |

### M3.1 Unions

Spec: [§6.6](docs/design/mora.md#66-unions), [§13.5](docs/design/mora.md#135-value-layouts). Crates: `mora-hir`, `mora-mir`, `mora-codegen`. Blocked by: M2.15.

Union declarations with cases and members, construction, target-typed case names (D-16), `indirect` cases and unions, `MR0150`, `@NonExhaustive`, copyable versus move-only unions, and the tag-plus-payload layout with the null-pointer niche of §13.5.

Done when:

- Run tests for construction, members and `indirect` recursion.
- A layout test shows `User?` is one pointer wide.

### M3.2 Enums

Spec: [§6.7](docs/design/mora.md#67-enums). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.1.

Closed enums with explicit backing types, checked casts that panic, `TryFrom`, `@Flags` enums with bitwise operators, and printing by name (through `IDisplay` until M4.9).

Done when:

- A cast of an undeclared value panics.
- `@Flags` combinations print and compare.

### M3.3 Patterns and switch

Spec: [§7.7](docs/design/mora.md#77-switch-expressions-and-statements), [§7.8](docs/design/mora.md#78-patterns). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.2.

Every pattern of §7.8, `is` expressions with definite assignment of bindings, one `switch` with arms as a statement and as an expression (D-89), guards, block arms and best-common-type.

Done when:

- One run test per pattern kind.
- Bindings in `is` are usable exactly where the `is` expression is known to be true (§7.8).

### M3.4 Exhaustiveness and reachability

Spec: [§7.8](docs/design/mora.md#78-patterns), [§25](docs/design/mora.md#25-references). Crates: `mora-check`. Blocked by: M3.3.

The usefulness algorithm of Maranget over unions, enums, `bool`, options, results and tuples, `MR0170` with one missing value, `MR2010` for unreachable arms, and the `_` rule for integers and strings.

Done when:

- UI tests show the missing case text, for example `missing case: Rect(_, _)`.
- A property test checks that no arm matches any reported missing value.

### M3.5 Options, `none` and flow narrowing

Spec: [§5.6](docs/design/mora.md#56-option-types-and-none), [§7.5](docs/design/mora.md#75-option-operators). Crates: `mora-hir`, `std`. Blocked by: M3.4.

`Option<T>` in the standard library, `T?` syntax, `none` (D-54), `MR0131`, implicit `T` to `T?`, `??`, `??=`, `?.`, `?[]`, the members of §5.6, `MR0162`, `MR2001`, and flow narrowing after `none` checks.

Done when:

- Run tests for every operator.
- UI tests: `x.y` on `T?` offers both fixes; after `if (u is none) return;` `u` has type `User`.

### M3.6 Operators, conversions and indexers

Spec: [§6.9](docs/design/mora.md#69-members), [§5.10](docs/design/mora.md#510-conversions). Crates: `mora-hir`. Blocked by: M3.3.

User-defined arithmetic, bitwise and unary operators, comparison operators from `IEquatable<T>` and `IComparable<T>` with `MR0238` (D-90), implicit conversions through `IFrom<S>` (D-91), and indexers.

Done when:

- Run tests for a vector type with operators and an indexer.
- UI test for an unpaired `==`.

### M3.7 Results and `?` propagation

Spec: [§4.6](docs/design/mora.md#46-entry-point), [§5.7](docs/design/mora.md#57-result-types), [§7.4](docs/design/mora.md#74-propagation-), [§11.1](docs/design/mora.md#111-two-kinds-of-failure), [§11.2](docs/design/mora.md#112-error-types), [§11.3](docs/design/mora.md#113-conversions-in-). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.5, M3.6.

`Result<T, E>`, `@MustUse` and `MR2002`, `IError` with `Code`, `PublicMessage` and `HttpStatus` (D-108), the `?` operator with identity and `IFrom` conversions (`MR0201`, `MR0202`, `MR0161`), `@From` and `MR0151` (D-100), the implicit `Ok` conversion and `MR0132`, and return-type inference for top-level statements with the exit codes of §4.6.

Done when:

- §2.4's `Total` runs.
- A top-level `?` that fails prints `error: ...` and exits with 1.

### M3.8 Generics and constraints

Spec: [§8.1](docs/design/mora.md#81-generic-declarations), [§8.2](docs/design/mora.md#82-constraints). Crates: `mora-hir`. Blocked by: M3.1.

Generic types, methods and local functions, the constraints of §8.2 including copyable-by-default and `where T : move` (D-23), C# method type inference, and `MR0234` for variance annotations.

Done when:

- UI tests for violating each constraint.
- Inference tests for calls with lambdas and generic arguments.

### M3.9 Monomorphization

Spec: [§8.1](docs/design/mora.md#81-generic-declarations), [§14.10](docs/design/mora.md#1410-monomorphization). Crates: `mora-mir`. Blocked by: M3.8.

Instantiation from the roots of §14.10, deduplication by item and arguments, and `MR0220` at depth 64.

Done when:

- A program using `List<int>` and `List<string>` (stub list in tests) emits two instantiations.
- UI test for polymorphic recursion.

### M3.10 Interfaces and static dispatch

Spec: [§6.8](docs/design/mora.md#68-interfaces), [§8.3](docs/design/mora.md#83-static-dispatch), [§8.5](docs/design/mora.md#85-built-in-interfaces), [§8.7](docs/design/mora.md#87-associated-types). Crates: `mora-hir`, `std`. Blocked by: M3.9.

Interface declarations with default members, static abstract members, interface inheritance, implementation by name and explicit implementation, associated types, `MR0232` and `MR0233`, the built-in interfaces of §8.5 declared in `std/`, and implicit type parameters for bare interface fields of classes, which spread to the classes that hold them, with `MR0239` (D-123).

Done when:

- Run tests for default methods and static abstract members in generic code.
- A generic `Max<T>` over `IComparable<T>` compiles to a direct call (MIR snapshot).
- A class with an `IUserStore` field, built with two different stores, compiles to two instantiations whose calls through the field are direct (MIR snapshot), and a diagnostic about one of them names it `OrderService<SqlUserStore>`.
- UI test for `MR0239` with its `any I` fix.

### M3.11 Dynamic interface values

Spec: [§8.4](docs/design/mora.md#84-dynamic-interface-values), [§13.6](docs/design/mora.md#136-vtables-and-type-ids). Crates: `mora-mir`, `mora-codegen`. Blocked by: M3.10.

Interface values as data and vtable pointers, boxing of value types with the LSP-visible boxing marker, vtables and BLAKE3 type ids of §13.6, `is C c`, `as C`, checked casts, `MR0230`, `MR0231`, erasure of associated types for `IEnumerable<T>` values, and `any I` satisfying a constraint `T : I`.

Done when:

- Run tests for `List<IShape>` holding a class and a boxed struct.
- A failing `(C)i` cast panics with the message of §11.4.

### M3.12 Tuples, collection expressions and smaller expressions

Spec: [§5.5](docs/design/mora.md#55-tuples), [§7.9](docs/design/mora.md#79-collection-expressions), [§7.13](docs/design/mora.md#713-other-expressions), [§6.9](docs/design/mora.md#69-members). Crates: `mora-hir`, `mora-mir`. Blocked by: M3.9.

Tuples and deconstruction, collection expressions with spreads and target types (D-20, `MR0171`) and `[]` as an empty dictionary (D-99), extension blocks with their parsing (D-98), `nameof`, `sizeof`, `default` with `MR0190`, ranges and `^` indexes.

Done when:

- Run tests for each form; `[1, 2, 3]` with no target type is a `List<int>`.

### M3.13 Statics

Spec: [§4.7](docs/design/mora.md#47-static-state). Crates: `mora-hir`, `mora-mir`, `mora-rt`. Blocked by: M1.11, M3.5.

`const` in read-only data, constant sequences as `ReadOnlySpan<T>`, `static lazy` with run-time cycle detection and `MR0126`, and `static mut`.

Done when:

- A lazy cycle panics with the message of §4.7.
- A `const` string longer than 15 bytes is an immortal object in read-only data, and reading it never writes its count.

### M3.14 Local path dependencies and library projects

Spec: [§4.4](docs/design/mora.md#44-dependencies), [§17.2](docs/design/mora.md#172-manifest). Crates: `mora-driver`, `mora-pkg`, `mora-hir`. Blocked by: M3.10.

`kind = "lib"`, root namespaces, `[dependencies]` path entries whose keys bind each dependency's root namespace (D-96), `MR0122`, `MR0501`, the dependency-cycle check `MR0502`, and `mora.lock` with path entries.

Done when:

- A two-project example (app plus library) builds and runs.
- A remote dependency source reports `MR0122`.
- Two path dependencies that depend on each other report `MR0502`.

### M3.15 Attributes: `Cfg`, lint levels and `Obsolete`

Spec: [§6.11](docs/design/mora.md#611-attributes), [§17.2](docs/design/mora.md#172-manifest). Crates: `mora-hir`, `mora-check`, `mora-driver`. Blocked by: M3.14.

Built-in attributes other than macros and `Test`: `@Cfg` with `Os`, `Arch`, `Debug`, `Test`, `Feature`; `[features]` in the manifest; `@Allow`, `@Warn`, `@Deny` and `[lints]`; `@Obsolete` with `MR2003`; `@Inline`; `@Layout(C)`; `@MustUse` on functions.

Done when:

- A `@Cfg(Os = "windows")` function compiles only on Windows CI.
- `@Deny("MR2002")` turns the warning into an error.

### M3.16 Platform capabilities and standard library layers

Spec: [§19.4](docs/design/mora.md#194-portability), [§15](docs/design/mora.md#15-standard-library-mvp-surface). Crates: `std`, `mora-hir`, `mora-target`. Blocked by: M3.15.

`Std.Core`, `Std.Alloc` and the hosted rest of `Std` as in §19.4 rule 4, capability tags on every hosted API, and `MR0510` when a program uses an API whose capability the target lacks.

Done when:

- A program using `File` against a test target description without `fs` reports `MR0510` naming `File`, `fs` and the target.
- `Std.Core` compiles for the 32-bit test description with no `heap` capability.

### M3.17 Interpolation handlers

Spec: [§7.14](docs/design/mora.md#714-interpolation-handlers), [§7.13](docs/design/mora.md#713-other-expressions), [§8.5](docs/design/mora.md#85-built-in-interfaces). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.10.

`IInterpolate`, `IHole<T>` and `Literal` (D-105): lowering an interpolated string whose target implements `IInterpolate` to `Begin`, `AppendLiteral`, and `AppendHole` for holes that implement `IHole<T>` or `AppendDisplay` for other `IDisplay` holes, skipping every part when `Enabled` is `false`, the hole's format and source text, and lint `unchecked-injection`. Lint `skipped-hole-effects` needs the effect analysis of §6.14 and arrives with M3.18.

Done when:

- A test handler records its calls: literal parts and holes arrive in source order, with each hole's format and source text.
- A hole with a side effect does not run when `Enabled` is `false`.
- A hole type with only `IDisplay` goes to `AppendDisplay`, and one that also implements `IHole<T>` goes to `AppendHole`.
- UI tests: a hole type with neither `IHole<T>` nor `IDisplay` is an error naming both; assigning a `string` to a handler type does not compile.

### M3.18 Effect inference and `uses` bounds

Spec: [§6.14](docs/design/mora.md#614-effects), [§6.9](docs/design/mora.md#69-members). Crates: `mora-hir`, `mora-mir`, `mora-check`, `mora-cli`. Blocked by: M2.11, M3.11, M3.16, M3.17.

Effect sets per function (D-102) from `@RequiresCapability`, `@Blocking`, `extern` functions (`ffi`) and `Secret<T>.Use` (`secret`), inferred per strongly connected component of the call graph and per generic instantiation, with closures passed to `in` parameters charged to the caller and `dyn` for calls through escaping function values and `any I` without a bound. `uses` bounds on functions, interface methods and function types, checked with `MR0250` and the call path that brings the effect in, and `mora explain-effects`. The getter cheapness check `MR0251` (D-103) with `@Cheap` trusted inside `Std`, the part of `MR0252` about capabilities and `dyn` calls, and lint `skipped-hole-effects`. Hover (M7.3) and the inlay hint (M7.4) read the same data.

Done when:

- A `uses none` function that calls `Clock.Now()` is `MR0250`, and the message shows the path `clock <- Clock.Now`.
- Two mutually recursive functions, one of which reads the clock, both infer `clock` with no annotation.
- `list.Sort(compare)` infers exactly the effects of `compare`, and a call through a stored function value without a bound adds `dyn`.
- UI tests for `MR0251` reporting a loop, a heap allocation, a mutation and a failure, each naming the call.
- `mora explain-effects` prints each effect of a function with its call path.

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
| [M4.9](#m49-standard-library-macros) | Standard-library macros | `std` | M4.7 |

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

The `MacroRunner` interface in `mora-hir` and its implementation in `mora-macro`: compiling macros with the JIT, the capability sandbox (`MR0613`), the 5-second and 256 MiB limits, panics reported at the use site, and same-project cycles (`MR0614`).

Done when:

- A macro calling `File.ReadAllText` fails with `MR0613`.
- An infinite loop in a macro fails after 5 seconds with the use site in the message.

### M4.4 Expansion, ordering and hygiene

Spec: [§9.4](docs/design/mora.md#94-hygiene), [§9.6](docs/design/mora.md#96-what-a-macro-sees). Crates: `mora-macro`, `mora-hir`. Blocked by: M4.3.

The `adds` pre-pass, expansion between signatures and body checking, the visibility rule of §9.6, attribute-argument binding, hygienic renaming, definition-site resolution of literal names, `MR0610` and `MR0612`.

Done when:

- Two macros on one type produce the same result in either order.
- A local named like a macro-introduced local does not collide (run test).

### M4.5 Expression macros

Spec: [§9.2](docs/design/mora.md#92-declaring-macros), [§9.5](docs/design/mora.md#95-the-typed-model-stdmeta). Crates: `mora-macro`, `mora-hir`. Blocked by: M4.4.

`#Name(args)` calls, constant and `Expr` parameters, and replacement of the call by the returned expression.

Done when:

- A user-written expression macro that turns `#Twice(x)` into `x + x` runs.

### M4.6 Expansion cache

Spec: [§9.7](docs/design/mora.md#97-running-macros). Crates: `mora-macro`. Blocked by: M4.4.

Caching each expansion under a hash of the toolchain version, the model it read, and the MIR of the macro and every function it calls transitively, in the `salsa` database and on disk.

Done when:

- Editing a method body does not rerun any macro (counter in `--verbose`).
- Editing a field reruns only the macros on that type.
- Editing a helper function a macro calls reruns that macro.

### M4.7 Seeing and testing expansions

Spec: [§9.8](docs/design/mora.md#98-seeing-and-debugging-expansions). Crates: `mora-cli`, `mora-macro`, `std`. Blocked by: M4.5.

`mora expand`, errors in generated code with the secondary span at the producing `code` line, `[macro Name]`-prefixed console output, and `Std.Testing.MacroTest`.

Done when:

- `mora expand --type UsersApi.User` prints formatted output (snapshot).
- A type error in generated code shows `note: generated by ...`.

### M4.8 Error pages and guide for macro authors

Spec: [§9](docs/design/mora.md#9-macros), [§17.6](docs/design/mora.md#176-diagnostics). Crates: `docs`. Blocked by: M4.7.

`docs/errors` pages for `MR0610` to `MR0615` and a guide `docs/guide/macros.md` that walks through writing, debugging and testing a macro, with every example executable.

Done when:

- `cargo xtask spec-examples` runs the guide's examples.

### M4.9 Standard-library macros

Spec: [§9.1](docs/design/mora.md#91-a-complete-example), [§9.6](docs/design/mora.md#96-what-a-macro-sees), [§9.9](docs/design/mora.md#99-standard-library-macros), [§6.2](docs/design/mora.md#62-structs). Crates: `std`, `mora-hir`. Blocked by: M4.7.

`@Equatable`, `@Hashable`, `@Comparable`, `@Display`, `@Default`, `@Clone` and `#Assert` with the behaviour of §9.9, and their automatic application to structs, tuples, enums and unions. `@Error` on unions and structs, with `@Code`, `@Public` and `@Status` (D-108), and helper attributes with `MR0615`.

Done when:

- A `MacroTest` suite per macro.
- §9.1's example prints `True`; structs compare by fields and print as `User { Id = 1, ... }`.

## M5: Standard library core

**Goal.** Text, collections, LINQ, console, environment, time, files, JSON, logging, secrets and testing: enough standard library for real programs.

**Exit criteria.** The std API reference builds from doc comments; JSONTestSuite parsing cases pass; LINQ chains meet §15.5 with no allocation.

| ID | Deliverable | Area | Blocked by |
|---|---|---|---|
| [M5.1](#m51-standard-library-internals) | Standard library internals | `std` | M3.11 |
| [M5.2](#m52-strings-text-and-formatting) | Strings, text and formatting | `std` | M5.1, M4.9 |
| [M5.3](#m53-number-parsing-and-formatting) | Number parsing and formatting | `std` | M5.2 |
| [M5.4](#m54-list-arrays-spans-and-memory) | List, arrays, spans and memory | `std` | M5.1, M2.9 |
| [M5.5](#m55-dictionary-hashset-queue-and-stack) | Dictionary, HashSet, Queue and Stack | `std` | M5.4, M3.18 |
| [M5.6](#m56-enumeration-protocol-and-foreach) | Enumeration protocol and foreach | `std` | M5.4 |
| [M5.7](#m57-linq) | LINQ | `std` | M5.6, M2.11 |
| [M5.8](#m58-console-environment-time-and-random) | Console, environment, time and random | `std` | M5.2 |
| [M5.9](#m59-files-directories-and-paths) | Files, directories and paths | `std` | M5.8 |
| [M5.10](#m510-json-values-parser-and-writer) | JSON values, parser and writer | `std` | M5.5 |
| [M5.11](#m511-the-json-macro) | The `@Json` macro | `std` | M5.10, M4.9 |
| [M5.12](#m512-testing-library-and-mora-test) | Testing library and `mora test` | `cli` | M5.7, M5.8, M5.11 |
| [M5.13](#m513-signals-and-observable) | Signals and `@Observable` | `std` | M5.4, M2.10, M4.9 |
| [M5.14](#m514-html-url-and-literal) | `Html`, `Url` and `Literal` | `std` | M5.2, M3.17 |
| [M5.15](#m515-logging-and-tracing) | Logging and tracing | `std` | M5.8, M5.10, M3.17 |
| [M5.16](#m516-secrett) | `Secret<T>` | `std` | M5.11, M5.15, M3.18 |
| [M5.17](#m517-report-and-context) | `Report` and `Context` | `std` | M5.2 |
| [M5.18](#m518-standard-library-api-reference) | Standard library API reference | `docs` | M5.9, M5.12, M5.13, M5.14, M5.15, M5.16, M5.17 |

### M5.1 Standard library internals

Spec: [§13.11](docs/design/mora.md#1311-standard-library-internals). Crates: `mora-hir`, `mora-mir`, `std`. Blocked by: M3.11.

The private `unsafe` dialect of §13.11 accepted only when compiling `Std` (`MR0107` elsewhere), `RawPtr<T>`, `Uninit<T>`, the intrinsics, the `RawDeref` projection, and MIR checks skipping raw places. Documented in `std/README.md`.

Done when:

- Using `unsafe` in a user project reports `MR0107`.
- A test-only `std` module moves values through raw storage with no leak.

### M5.2 Strings, text and formatting

Spec: [§5.3](docs/design/mora.md#53-strings), [§15.2](docs/design/mora.md#152-strings-and-text). Crates: `std`, `mora-rt`. Blocked by: M5.1, M4.9.

The `string`, `StringBuilder`, `Formatter` and `char` members of §15.2, byte-offset slicing with boundary panics, `MR0130`, and the format specifiers of §15.2 for interpolation and `ToString(spec)`.

Done when:

- Run tests for every member, including non-ASCII and astral-plane text.
- Format specifier tests match .NET invariant-culture output for a table of values.

### M5.3 Number parsing and formatting

Spec: [§8.5](docs/design/mora.md#85-built-in-interfaces), [§15.1](docs/design/mora.md#151-prelude-std). Crates: `std`, `mora-rt`. Blocked by: M5.2.

`IParse<T>`, `int.Parse` and the other primitives returning `Result<T, ParseError>`, shortest round-trip float formatting and correct float parsing in the runtime.

Done when:

- Round-trip property tests for `double`.
- Parsing errors carry the offending text.

### M5.4 List, arrays, spans and memory

Spec: [§10.10](docs/design/mora.md#1010-growable-collections-and-interior-views), [§15.3](docs/design/mora.md#153-collections). Crates: `std`, `mora-mir`. Blocked by: M5.1, M2.9.

`List<T>` with borrow count and version, the array, span and memory members of §15.3, `Sort` overloads, and the hidden span guards of §10.10 inserted by the view check.

Done when:

- Adding to a list while a span of it is live panics with the §10.10 message.
- Memory suite additions for collections (§20.3).

### M5.5 Dictionary, HashSet, Queue and Stack

Spec: [§15.3](docs/design/mora.md#153-collections), [§15.4](docs/design/mora.md#154-lookups-never-panic). Crates: `std`, `mora-rt`. Blocked by: M5.4, M3.18.

Insertion-ordered `Dictionary` (D-29) with SipHash-1-3 and per-process keys, the members of §15.3 and §15.4 including `Get` returning `TValue?`, `HashSet`, `Queue` and `Stack`. The `Dictionary` index `get` and `Count`, and `HashSet.Contains` and `Count`, are marked `@Cheap` (D-103), and `@Cheap` outside `Std` is `MR0254`.

Done when:

- Iteration order equals insertion order after removals and re-insertions.
- `Add` of a present key returns `false` and changes nothing; assigning `none` through the indexer removes the key.
- UI test: a getter that reads `dict[key]` passes the getter check, and `@Cheap` on a function outside `Std` is `MR0254`.

### M5.6 Enumeration protocol and foreach

Spec: [§7.11](docs/design/mora.md#711-statements), [§8.5](docs/design/mora.md#85-built-in-interfaces), [§10.10](docs/design/mora.md#1010-growable-collections-and-interior-views). Crates: `std`, `mora-mir`. Blocked by: M5.4.

`IEnumerable<T>` and `IEnumerator<T>`, `foreach` with `let`, `var` and `ref var`, version-checked enumerators (D-42), and boxed enumerators through `IEnumerable<T>` values.

Done when:

- Modifying a list inside `foreach` panics with `collection was modified during enumeration`.
- A method returning `IEnumerable<User>` works with `foreach` and LINQ.

### M5.7 LINQ

Spec: [§15.5](docs/design/mora.md#155-linq), [§10.9](docs/design/mora.md#109-closures). Crates: `std`. Blocked by: M5.6, M2.11.

The operators of §15.5 as iterator structs taking closure structs, buffering operators, and the `OrNull` variants.

Done when:

- A `Where`/`Select`/`Sum` chain allocates nothing (debug runtime counter).
- Each operator has run tests against expected sequences.

### M5.8 Console, environment, time and random

Spec: [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§15.6](docs/design/mora.md#156-console-environment-and-time). Crates: `std`, `mora-rt`. Blocked by: M5.2.

The World seam in the per-thread context (D-109), through which the clock and `Random.Shared` read, and the members of §15.6: `Console` input and error output, `Environment`, `Random`, `Math` and `MathF`, and the time types of D-111 (`Clock`, `Instant`, `MonoTime`, `TimeSpan`, `Stopwatch`, `LocalDate`, `LocalTime`, `LocalDateTime`, `Period`, `TimeError`) and lint `wall-clock-duration`. `@RequiresCapability` tags go on the members that use a capability, such as `Clock.Now()` and `MonoTime.Now()` (`clock`) and `Random.Shared` (`random`); value types and their arithmetic need none.

Done when:

- `Clock.Now().ToString()` matches RFC 3339 in UTC, and `Instant.Parse` reads it back.
- `LocalDate.Of(2026, 2, 30)` is an `Err`, and January 31 plus `Period.Months(1)` is the last day of February.
- A UI test for `wall-clock-duration` and its fix.
- `Console.ReadLine()` returns `none` at end of input.

### M5.9 Files, directories and paths

Spec: [§15.7](docs/design/mora.md#157-files-and-networking). Crates: `std`, `mora-rt`. Blocked by: M5.8.

`File`, `Directory`, `Path` and `IoError` of §15.7, with platform path rules on Windows.

Done when:

- Run tests on a temporary directory on every Tier 1 target.
- `IoError.NotFound` for a missing file prints a message with the path.

### M5.10 JSON values, parser and writer

Spec: [§16.3](docs/design/mora.md#163-json-stdjson). Crates: `std`. Blocked by: M5.5.

`JsonValue`, `Json.Parse` over strings and bytes, compact and indented output, `JsonError` with line and column, the depth limit and duplicate-key rule.

Done when:

- The JSONTestSuite `y_` cases parse and `n_` cases fail.
- `{not json` fails with `at line 1, column 2: expected string`.

### M5.11 The `@Json` macro

Spec: [§16.3](docs/design/mora.md#163-json-stdjson), [§9.9](docs/design/mora.md#99-standard-library-macros). Crates: `std`. Blocked by: M5.10, M4.9.

`@Json`, `IJsonSerialize`, `IJsonDeserialize`, `Json.Serialize` and `Json.Deserialize` with every mapping row of §16.3, `@JsonName`, `@JsonIgnore`, `@JsonStrict` and `@JsonTag`.

Done when:

- Round-trip tests for each row of the §16.3 table.
- A missing non-optional property fails with `missing property "name"`.

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

`Signal<T>`, `Subscribe(move handler: (T) => ())` returning a unique `Subscription`, `SubscribeWeak`, and `Emit`. The `@Observable` macro (D-104) and the `@SoleWriter` attribute with `MR0253`.

Done when:

- Dropping the subscription stops delivery; a weak subscriber that died is skipped and removed.
- `SetX` on an `@Observable` class emits `XChanged` only when the value changed, and the signal of a `stored` getter that reads `X` after it.
- UI test: assigning an observed field outside `SetX`, inside or outside the class, is `MR0253` with a fix that calls `SetX`.

### M5.14 `Html`, `Url` and `Literal`

Spec: [§15.2](docs/design/mora.md#152-strings-and-text), [§7.14](docs/design/mora.md#714-interpolation-handlers). Crates: `std`. Blocked by: M5.2, M3.17.

The `Literal` members, and the `Html` and `Url` handlers of §15.2 (D-105): escaping by context for element text and quoted attribute values, percent-encoding by URL component, the panics for holes in other positions, nested `Html` holes, and `Unchecked`.

Done when:

- Escaping tests for text, quoted attributes, path segments, query names and values, and fragments, including non-ASCII text.
- A hole inside `<script>` panics with the context; an `Html` hole in element text is not escaped twice, and one in an attribute value is escaped.
- In `href`, a hole after `/users/` is percent-encoded, a hole at the start of the value that is not a `Url` panics, and a hole in `onclick` panics.

### M5.15 Logging and tracing

Spec: [§15.10](docs/design/mora.md#1510-logging-and-tracing), [§7.14](docs/design/mora.md#714-interpolation-handlers). Crates: `std`, `mora-rt`. Blocked by: M5.8, M5.10, M3.17.

`Log.Trace` to `Log.Error` through one log message handler per level (D-107), fields named after each hole's source text with `#2` for repeats, `MORA_LOG` and `MORA_LOG_FORMAT=json`, and `Trace.Span` with its end on drop. Spawned tasks inheriting the current span come with M6.3, and redacted `Secret<T>` holes with M5.16.

Done when:

- A disabled level evaluates no hole.
- JSON output has the template, one field per hole, the level, the time and the span; a span ends on an early `?` return with its duration.

### M5.16 `Secret<T>`

Spec: [§15.11](docs/design/mora.md#1511-secrets), [§16.3](docs/design/mora.md#163-json-stdjson), [§6.14](docs/design/mora.md#614-effects). Crates: `std`, `mora-hir`. Blocked by: M5.11, M5.15, M3.18.

`Secret<T>` for `string` and `byte[]` (D-106): redacted `IDisplay`, `Use` through a span view, zeroing in the destructor, `Clone`, lint `secret-copied`, and `@Json` reading secrets and writing them only with `@JsonSecret`, leaving them out otherwise. The `secret` fact in `mora explain-effects` needs the effect analysis of §6.14 and arrives with it.

Done when:

- Interpolating, logging and printing a secret show `Secret<string>(***)`.
- With the debug runtime, a free hook sees the secret's bytes already zero when its buffer is released.
- `Json.Serialize` of a type with a `Secret<T>` field and no `@JsonSecret` leaves the field out; with `@JsonSecret` it writes the plaintext.

### M5.17 `Report` and `Context`

Spec: [§11.5](docs/design/mora.md#115-reports), [§11.2](docs/design/mora.md#112-error-types). Crates: `std`. Blocked by: M5.2.

`Report` over `any IError` with context lines (D-108), `Context(message)` with a string or a lambda, `Report()`, the printed chain, and `MR0201` with fixes that add `.Report()` or `.Context("...")` for a bare `?` into `Report`.

Done when:

- A three-level chain prints the newest context first, then the error and the chain of its `Source()`.
- An `Ok` passed through `Context(() => ...)` never runs the lambda.

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
| [M6.9](#m69-dependency-injection) | Dependency injection | `check` | M6.7, M3.15 |

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

### M6.6 HTTP/1.1 connections

Spec: [§16.1](docs/design/mora.md#161-http-server-stdnethttp). Crates: `std`, `mora-rt`. Blocked by: M6.5.

Request-head parsing with `httparse` in the runtime, keep-alive, `Content-Length` and chunked request bodies, `Expect: 100-continue`, `Connection: close`, `Date` and `Server` headers, every limit and timeout of §16.1 with its status code, and the global body memory budget with backpressure.

Done when:

- Protocol tests over raw TCP for each limit (413, 431, idle timeout).
- With the body budget set to 1 MiB, 64 clients each sending a 1 MiB body are served one budget at a time, and a client that waits more than 5 s gets 503 with `Retry-After: 1`.
- Pipelined requests on one connection are answered in order.

### M6.7 WebApp, routing and results

Spec: [§16.1](docs/design/mora.md#161-http-server-stdnethttp), [§16.2](docs/design/mora.md#162-httperror). Crates: `std`. Blocked by: M6.6, M5.11, M5.15.

`WebApp`, `WebAppOptions`, `--urls` and `MORA_URLS`, `Map*` with `move` handlers generic over any copyable `E : IError` (D-108), the routing rules of §16.1 (404, 405 with `Allow`, duplicate panics), `Request`, `RouteValue<T>`, `ReadTextAsync`, `ReadJsonAsync<T>`, `Response`, `Results`, `HttpError` with its conversions and `IError` members, error responses from `PublicMessage` and `HttpStatus` with `application/problem+json` for internal errors, request deadlines from `WebAppOptions.RequestTimeout` (503 when one passes), the request log as `Info` events in a span per request, and graceful shutdown on SIGINT, SIGTERM and Ctrl+Break.

Done when:

- Routing table tests for literal-before-parameter order and `{*rest}`.
- Shutdown with an in-flight request completes it within 5 s.
- With `RequestTimeout` set to 100 ms, a handler that awaits `Task.Delay` for 1 s returns 503 at about 100 ms and its future's destructors run.
- An internal error returns 500 with a problem+json body holding only its code and trace id, and the log has the full chain under that trace id.

### M6.8 The users API example and acceptance test

Spec: [§2.2](docs/design/mora.md#22-the-mvp-acceptance-program), [§20.2](docs/design/mora.md#202-the-mvp-acceptance-test). Crates: `examples/users-api`, `mora-testkit`. Blocked by: M6.7.

`examples/users-api` exactly as §2.2 shows it, and the acceptance test of §20.2 as a required CI job on every Tier 1 target.

Done when:

- The transcript of §2.2 matches byte for byte on every Tier 1 target.
- The job is added to the org required checks.

### M6.9 Dependency injection

Spec: [§16.4](docs/design/mora.md#164-dependency-injection-stdinject), [§8.3](docs/design/mora.md#83-static-dispatch), [§16.1](docs/design/mora.md#161-http-server-stdnethttp). Crates: `mora-hir`, `mora-check`, `mora-mir`, `std`. Blocked by: M6.7, M3.15.

`Std.Inject`: `Services` with `AddSingleton`, `AddScoped`, `AddTransient`, `AddEach`, `Replace`, `Decorate` and `.Per<K>()`, factories that may be `async` and return `Result`, library defaults, `@Cfg` on registrations, `BuildAsync` with `StartupError` and immortal singletons, `IServiceHost`, `RunScoped` with provided values and kinds, `IRoot<R>`, `Scope<K>`, lazy `() => T` parameters, generic registrations, one generated wiring function per root, `WebApp.CreateBuilder` and injected handler parameters, `MR0700` to `MR0711`, and lint `unused-service` (D-122 to D-127).

Done when:

- UI tests for every code from `MR0700` to `MR0711`, with `MR0703` and `MR0705` printing the whole chain from the root.
- The MIR of a handler that takes a singleton and a scoped service shows direct constructor and method calls, and `Assert.AllocatesAtMost` shows that running it with `RunScoped` allocates only the scoped service.
- A singleton that depends on a scoped service through a transient is `MR0705`, and a root outside every `Connection` scope that needs a service per `Connection` is `MR0706`.
- In a run test, two child scopes inside one `Connection` scope share its service per `Connection`, each gets its own innermost scoped service, and destructors run in reverse construction order when each scope ends.
- A test that calls the app's registration function, replaces a file-backed service and carries `uses none` passes; without the `Replace` the build fails with `MR0250` and the call path.

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

`mora lsp` on `lsp-server`: initialization, document sync, the shared database, debounced diagnostics with fixes as code actions, and the UTF-16 positions of M0.5.

Done when:

- Scripted session tests: open, edit, receive and clear a diagnostic.
- Diagnostics arrive within the §20.6 budget on `users-api`.

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

The `wolfware.mora` extension: TextMate grammar, language configuration, snippets, the LSP client, status-bar version, the missing-toolchain prompt, and publishing to the Visual Studio Marketplace and Open VSX from the release workflow.

Done when:

- Grammar snapshot tests over the spec examples.
- The extension's integration test starts `mora lsp` and receives diagnostics.

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

The site at the project domain (OQ-18): install guide, language tour following §2, the standard library reference, the error pages, the macro guide and the ungrammar. `mora docs --llm`, served as `llms.txt`, and `mora-errors.jsonl` (D-112).

Done when:

- Every code sample on the site is run by `cargo xtask spec-examples`.
- The site deploys from `main` through GitHub Actions.
- `mora docs --llm` prints the same bytes on every run, and they match the site's `llms.txt` for that version.
- `mora-errors.jsonl` has one line per page in `docs/errors/`.

### M7.10 Release workflow

Spec: [§19.2](docs/design/mora.md#192-toolchain-archive), [§19.3](docs/design/mora.md#193-installation). Crates: `.github/workflows`, `xtask`. Blocked by: M0.11, M6.8.

`cargo xtask dist` building the archive of §19.2 per host, the release workflow on tags producing archives, SHA-256 checksums and Sigstore signatures through GitHub OIDC, and GitHub Releases with notes.

Done when:

- A release candidate tag produces signed archives for every host of §19.1.
- `cosign verify-blob` succeeds on each archive.

### M7.11 Installers and self update

Spec: [§19.3](docs/design/mora.md#193-installation). Crates: `install`, `mora-cli`. Blocked by: M7.10.

`install.sh` and `install.ps1` with checksum verification and optional `cosign` signature verification, the `wolfware-labs/tap` Homebrew formula, the winget manifest `Wolfware.Mora`, and `mora self update` with built-in Sigstore verification, all as the table in §19.3 specifies.

Done when:

- Install-script tests on clean CI runners for every Tier 1 target, with and without `cosign` on `PATH`.
- `mora self update` from a release candidate to the next one, and refusal of an archive whose signature identity does not match.

### M7.12 Benchmarks and budgets

Spec: [§20.6](docs/design/mora.md#206-budgets). Crates: `benches`, `evals`, `ci`. Blocked by: M6.8, M1.20, M7.9.

A `bench` workflow measuring every budget of §20.6, a Rust load generator for `users-api`, stored baselines, and the 10% regression gate. The agent eval suite of §20.6 (D-112): the tasks in `evals/`, `evals/models.toml`, and a release workflow that reports both rates per model against the previous release of the line.

Done when:

- The workflow posts the measurements on pull requests.
- A synthetic slowdown fails the gate.
- The eval workflow runs on a release candidate and reports both rates for every model in `evals/models.toml`.

### M7.13 v0.1.0 release

Spec: [§1.5](docs/design/mora.md#15-what-the-mvp-is), [§22](docs/design/mora.md#22-roadmap). Crates: repository. Blocked by: M7.5, M7.6, M7.7, M7.9, M7.11, M7.12.

Checks §1.5 end to end on fresh machines, revises the §20.6 budgets with measured values, updates the spec's status line, writes the edition policy into the spec, and publishes v0.1.0.

Done when:

- A developer who has not seen Mora installs it, opens §2.2 in VS Code and gets diagnostics, hover and go-to-definition (recorded in the issue).
- Every channel of §19.3 serves v0.1.0.
- The spec says what an edition may change, how an edition is chosen in `mora.toml`, and that every edition change ships with a `safe` fix.

## Post-MVP milestones

| Milestone | Scope | Spec |
|---|---|---|
| M8: WebAssembly and libraries | Wasm targets, the `.wasm` library format with `mora.meta`, `mora.ir`, `mora.deps`, `mora.caps` and `mora.abi`, linking modes and ABI checks (OQ-15), and a stable plugin ABI at an opt-in boundary: `[abi] stable` interface packages, `@Frozen` types, and native and Wasm plugins loaded from one interface. | [§18.1](docs/design/mora.md#181-webassembly-target), [§18.2](docs/design/mora.md#182-the-library-format), [§18.3](docs/design/mora.md#183-linking-libraries), [§18.4](docs/design/mora.md#184-abi-stability), [§24](docs/design/mora.md#24-open-questions) |
| M9: Packages and capabilities | URL and git dependency sources, inline imports in scripts, lockfile hashes, `mora fetch`, and capability grants enforced on wasm and native targets, narrowed per dependency, with sandboxed runs. | [§18.5](docs/design/mora.md#185-imports-manifest-and-lockfile), [§18.6](docs/design/mora.md#186-capabilities) |
| M10: Threads | `shared` classes with atomic counts, sendable checks, `Thread`, `Task.Run`, `Mutex`, `Atomic`, `Channel`, a multi-threaded executor (OQ-6, OQ-7, OQ-1), and supervised worker processes as a framework option. | [§12.6](docs/design/mora.md#126-threads-post-mvp), [§24](docs/design/mora.md#24-open-questions) |
| M11: Language growth | Class inheritance, variance, `yield return`, retroactive implementations, arenas, allocators, user `unsafe`, `decimal`, conditionally async higher-order functions (`async?`), and more macro kinds (OQ-3, 4, 5, 9, 10, 11, 12, 13). | [§9.10](docs/design/mora.md#910-not-in-the-mvp), [§10.12](docs/design/mora.md#1012-arenas-post-mvp), [§24](docs/design/mora.md#24-open-questions) |
| M12: Registry and signing | The package registry and registry dependency sources, Sigstore-signed publishing, the transparency log and mirrors (OQ-16). | [§18.7](docs/design/mora.md#187-registry), [§24](docs/design/mora.md#24-open-questions) |
| M13: Backends and targets | LLVM release backend, Component Model export, Windows debug information, embedded targets (OQ-2). | [§18.3](docs/design/mora.md#183-linking-libraries), [§19.1](docs/design/mora.md#191-targets), [§24](docs/design/mora.md#24-open-questions) |
| M14: Contracts and checked types | `contract` sections with `requires`, `fails`, `ensures` and `invariant`, `where` predicates on newtypes, property tests generated from types and contracts, unit algebra, and compile-time evaluation of effect-free functions. | [§6.12](docs/design/mora.md#612-newtypes), [§6.13](docs/design/mora.md#613-contract-sections-post-mvp), [§14.6](docs/design/mora.md#146-constant-evaluation), [§15.9](docs/design/mora.md#159-testing) |
| M15: Test and run tooling | The simulated World, sound test caching and flaky-test triage from effects, `mora run --record` and `mora replay`, and hot reload of function bodies in dev builds. | [§12.1](docs/design/mora.md#121-execution-model-in-the-mvp), [§15.9](docs/design/mora.md#159-testing), [§17.5](docs/design/mora.md#175-test-runner) |
| M16: Agent tooling and API evolution | `mora query`, `mora context` and `mora mcp`, JSON Schema from types and `@Tool` signatures as MCP tools, semantic diff with compiler-checked semver (contract changes count once M14 lands), and editions with `mora migrate`. | [§6.14](docs/design/mora.md#614-effects), [§16.3](docs/design/mora.md#163-json-stdjson), [§17.1](docs/design/mora.md#171-commands), [§18.4](docs/design/mora.md#184-abi-stability) |
| M17: Standard library growth | Time zones and the tz database, typed configuration with `@Config`, `Retry` policies, `@CancelSafe` and `Task.Shield`. | [§12.5](docs/design/mora.md#125-cancellation), [§15.6](docs/design/mora.md#156-console-environment-and-time), [§15.12](docs/design/mora.md#1512-what-is-not-in-the-mvp) |
| M18: Optimizer | General reuse when unique, merging identical MIR bodies, `Vector128<T>`, closed-world devirtualization of `any I` and function values, and checked `@NoAlloc` and `scoped`. | [§10.13](docs/design/mora.md#1013-what-the-optimizer-guarantees), [§14.19](docs/design/mora.md#1419-reuse-when-unique), [§6.14](docs/design/mora.md#614-effects), [§13.1](docs/design/mora.md#131-the-runtime-library), [§14.14](docs/design/mora.md#1414-linking) |

