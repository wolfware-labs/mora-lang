# Mora language and toolchain specification

Version 0.1 (draft). Status: under review.

Mora is a programming language with familiar curly-brace syntax, memory managed by reference counting and single ownership instead of a garbage collector, and compilation to a single native executable. This document is the design the MVP is built against. It defines the language, its memory model, the runtime, the standard library surface the MVP ships, the compiler architecture and the toolchain. Work that comes after the MVP is specified at the level needed to keep MVP decisions compatible with it.

In linguistics a *mora* is the unit of timing that decides a syllable's weight, written μ. In Spanish, *mora* is a blackberry.

## Contents

1. Introduction
2. A tour of Mora
3. Source text and lexical structure
4. Program structure
5. Types
6. Declarations
7. Expressions and statements
8. Generics and interfaces
9. Macros
10. Memory model
11. Errors and panics
12. Async and concurrency
13. Runtime and ABI
14. Compiler architecture
15. Standard library (MVP surface)
16. HTTP and JSON
17. Toolchain, CLI and editor support
18. Libraries, WebAssembly and packages (post-MVP)
19. Targets, distribution and installation
20. Testing and quality
21. Project conventions
22. Roadmap
23. Decisions register
24. Open questions
25. References

## 1. Introduction

### 1.1 Who it is for

Mora is for application developers who want native binaries with predictable memory use and no collector pauses, without learning lifetime annotations to get them. Its syntax and idioms are the ones a developer who knows a mainstream curly-brace language expects, so that developer reads Mora on the first day. Where its model differs from those languages (ownership, errors as values, interfaces instead of inheritance), Mora follows its own model (D-53).

### 1.2 Goals

- **G1. Familiar, not a copy.** Syntax, names and idioms are the familiar ones wherever they fit Mora's model. Mora does not aim for compatibility with any other language: it leaves out features that conflict with deterministic memory or safety, such as exceptions, inheritance, reflection and `null` references.
- **G2. Deterministic memory.** No garbage collector, no finalizer thread, no pauses. Values are freed at a point the reader can see in the source. Allocation and reference-count costs are visible.
- **G3. Safe by default.** No null references, no use-after-free, no data races, no unchecked casts. Programs cannot reach undefined behaviour without an `unsafe` block (post-MVP) or an FFI call.
- **G4. One native binary.** `mora build` produces one executable with no runtime to install, no JIT and no framework.
- **G5. Rust toolchain.** The compiler, runtime, language server, formatter and package tool are written in Rust and ship as one `mora` executable plus a runtime library per target.
- **G6. Libraries as WebAssembly** (post-MVP). A library ships as one `.wasm` file that carries its metadata and typed IR, for both wasm and native consumers (§18).
- **G7. Portable core.** Linux, macOS and Windows come first, but no part of the compiler, runtime or standard library may assume an operating system, a CPU, a pointer width or a byte order outside a target-specific layer. Adding a target, including an embedded device with no OS, means adding a target description, a runtime platform layer and, if Cranelift cannot generate its code, a backend; it never means changing the front end, MIR or the memory model (§19.4).

### 1.3 Non-goals

- Source or binary compatibility with another language, its runtime or its package ecosystem (D-9). Code written in another language is reached through the C ABI (§13.9) and, after the MVP, as a plain wasm module (§18.3).
- Runtime reflection, a `dynamic` type, a universal base type or loading code by name at runtime.
- Lifetime annotations. Borrowing uses the familiar `in` and `ref` parameter conventions, and a borrow never outlives the call that created it (§10).
- A virtual machine. The REPL and `mora run` compile to machine code too.

### 1.4 Anti-goals

These would make Mora a failure even if every test passed:

- **A systems language with different syntax.** A developer writing their first REST endpoint must not meet lifetime errors, borrow-checker puzzles or ownership vocabulary they have to look up. Every memory-model error says what would go wrong in plain terms and offers a concrete fix (§17.6).
- **A clone of another language.** Adding a feature because another language has it, or keeping another language's quirk for the sake of familiarity.
- **Hidden costs.** An allocation, a boxing conversion or an atomic operation that the reader cannot predict from the source.
- **A second-class platform.** Windows is a first-class target from the first milestone, because many application developers work on it.

### 1.5 What the MVP is

The MVP is done when the program in §2.2, a REST API with JSON request and response bodies, builds with `mora build` into a native executable on Linux, macOS and Windows (x86_64 and aarch64 where §19.1 lists them), passes its integration tests in CI on all three, and a developer new to Mora can install the toolchain with one command, open the project in VS Code and get diagnostics, hover types and go-to-definition.

The release lines follow from that:

| Line | Milestones | Contents |
|---|---|---|
| **MVP (v0.1)** | M0 to M7 | Native compilation with Cranelift, the memory model, the expressive core, macros, the standard library core, async with a single-threaded executor, HTTP/1.1 server, JSON, compile-time dependency injection, LSP, VS Code extension, formatter, REPL, installers |
| **v0.2** | M8, M9, M16 | WebAssembly target and `.wasm` libraries, a stable plugin ABI, packages from URL and git sources, capability enforcement, semantic diff and semver checks (contracts join in v0.3), editions, `mora query`, `mora context` and `mora mcp` |
| **v0.3** | M14, M15, M17 | Contract sections, property tests, unit algebra, compile-time evaluation, the simulated World, test caching, record and replay, hot reload, time zones, `@Config`, retry policies |
| **v0.4** | M10, M11, M18 | Threads and `shared` classes, worker processes, class inheritance, `async?`, more macro kinds, arenas, `decimal`, general reuse, vector types, devirtualization |
| **v0.5** | M12, M13 | Registry, registry dependencies and signing, LLVM release backend, Component Model, embedded targets |

### 1.6 How to read this document

- **MUST**, **MUST NOT**, **SHOULD** and **MAY** follow RFC 2119.
- Section references look like §10.4. Decisions look like D-12 and are listed in §23. Open questions look like OQ-3 and are listed in §24.
- **[MVP]** and **[post-MVP]** mark which release line a feature belongs to when a section mixes both. Unmarked text in §§3 to 17 is MVP.
- **[verify]** marks a claim about third-party behaviour (an OS, a linker, a crate) that a test or manual check must confirm before code relies on it.
- Code blocks tagged `mora` are Mora source. Blocks whose first line is `// expect:`, `// error:` or `// warning:` are executable examples that the test suite extracts and runs (§20.4).
- Grammar uses EBNF: `?` optional, `*` zero or more, `+` one or more, `|` alternatives, quoted strings are tokens. The fragments summarize the normative grammar, the ungrammar of §14.4.

### 1.7 Terms

| Term | Meaning |
|---|---|
| **Value type** | `struct`, `union`, `enum`, primitive, tuple. A value with no identity: copied or moved, and stored where the compiler decides (§6.1). |
| **Shared class** | `class`. Heap object with identity and a reference count; aliases allowed. |
| **Unique class** | `unique class`. Heap object with exactly one owner; moved, never aliased. |
| **Move-only type** | A unique class, or any value type that contains a move-only field or declares a destructor (§10.2). |
| **View type** | `ref struct`, `Span<T>`, `ReadOnlySpan<T>` and non-escaping closures. Can only live on the stack, for at most the current function call (§10.6). |
| **Borrow** | Access to a value through an `in` or `ref` parameter or a view, without owning it. |
| **Retain / release** | Increment / decrement of a shared object's strong count. |
| **Drop** | Running a value's destructor and releasing what it owns. |
| **Place** | A memory location: a local, a parameter, a field of a place, an element of a place. |

## 2. A tour of Mora

This section is informative. The rules are in §§3 to 17.

### 2.1 Hello, world

```mora
// expect: Hello, world!
Console.WriteLine("Hello, world!");
```

A project with this one line in `src/Program.mr` builds into a native executable. The file uses top-level statements, which form the entry point (§4.6). It needs no `using` directive, because `Console` lives in the prelude (§15.1).

### 2.2 The MVP acceptance program

This program is the oracle for the MVP (§1.5). It lives in the repository as `examples/users-api`, and its integration test (§20.2) drives it over real TCP on every CI platform.

`mora.toml`:

```toml
[project]
name = "users-api"
version = "0.1.0"
kind = "app"
```

`src/Model.mr`:

```mora
@Json
struct User(Id: int, Name: string, Email: string?);

@Json
struct NewUser(Name: string, Email: string?);
```

`src/UserStore.mr`:

```mora
class UserStore {
    private mut _nextId: int = 1;
    private _users: Dictionary<int, User> = [];

    Add(input: NewUser): User {
        let user = new User(_nextId, input.Name, input.Email);
        _nextId += 1;
        _users[user.Id] = user;
        return user;
    }

    Find(id: int): User? => _users[id];

    All(): List<User> => _users.Values.OrderBy(u => u.Id).ToList();

    Remove(id: int): bool => _users.Remove(id);
}
```

`src/Program.mr`:

```mora
using Std.Net.Http;

let store = new UserStore();
let app = WebApp.Create(args);

app.MapGet("/users", async (req: Request) => Results.Json(store.All()));

app.MapGet("/users/{id}", async (req: Request) => {
    let id = req.RouteValue<int>("id")?;
    return match (store.Find(id)) {
        User u => Results.Json(u),
        none => Results.NotFound(),
    };
});

app.MapPost("/users", async (req: Request) => {
    let input = await req.ReadJsonAsync<NewUser>()?;
    if (input.Name.Length == 0) {
        return Results.BadRequest("name is required");
    }
    let user = store.Add(input);
    return Results.Created($"/users/{user.Id}", user);
});

app.MapDelete("/users/{id}", async (req: Request) => {
    let id = req.RouteValue<int>("id")?;
    return store.Remove(id) ? Results.NoContent() : Results.NotFound();
});

await app.RunAsync("127.0.0.1:8080")?;
```

What the program relies on, with the section that defines each:

| Line | Feature | Section |
|---|---|---|
| `src/Model.mr` | A file's namespace comes from its folder; all three files are in the root namespace `UsersApi` | §4.2 |
| `class UserStore`, `Add(...)` | A declaration without a visibility modifier is visible in its folder | §4.5 |
| `@Json` | Standard-library macro that generates JSON serialization | §9.9, §16.3 |
| `struct User(...)` | Value with read-only fields, a positional constructor and generated equality | §6.2, §6.5 |
| `= []` | An empty collection of the target type, here a `Dictionary` | §7.9 |
| `Email: string?` | `T?` is an option type; `none` is its empty value | §5.6 |
| `private mut _nextId: int` | Fields are read-only unless marked `mut` | §6.3 |
| `let` / `var` | Immutable and mutable locals | §7.10 |
| `_users[id]` | Lookup returning `V?`, `none` when the key is missing | §15.4 |
| `OrderBy(...).ToList()` | Sequence operators over monomorphized iterator structs | §15.5 |
| `async (req: Request) => ...` | Escaping async lambda; captures `store` by retain | §10.9, §12 |
| `req.RouteValue<int>("id")?` | `Result` propagation; `HttpError` becomes a 400 response | §11.2, §16.2 |
| `await ...ReadJsonAsync<NewUser>()?` | `?` applies to the awaited value | §7.4 |
| `return Results.NotFound();` in a lambda that returns `Result<Response, HttpError>` | Implicit `Ok` wrapping through a user-defined conversion | §11.3 |
| `match (x) { User u => ..., none => ... }` | Exhaustive pattern match over `User?` | §7.7 |
| `await app.RunAsync(...)?` at top level | Async entry point whose error ends the process with exit code 1 | §4.6 |

Expected behaviour, which the integration test checks byte for byte:

```text
POST /users {"name":"Ada"}          -> 201, Location: /users/1, {"id":1,"name":"Ada","email":null}
GET  /users/1                       -> 200, {"id":1,"name":"Ada","email":null}
GET  /users                         -> 200, [{"id":1,"name":"Ada","email":null}]
GET  /users/abc                     -> 400, {"error":"route value 'id' is not a valid int"}
GET  /users/9                       -> 404, empty body
POST /users {"name":""}             -> 400, {"error":"name is required"}
POST /users {not json               -> 400, {"error":"invalid JSON at line 1, column 2: expected string"}
DELETE /users/1                     -> 204
DELETE /users/1                     -> 404
```

### 2.3 Memory at a glance

```mora
public struct Point(X: float, Y: float);     // value: copied, no identity

public class Node {                          // shared: reference counted
    public Children: List<Node> = [];
    public Parent: weak Node;                // back-reference: does not keep the parent alive
}

public unique class TempFile {               // unique: one owner, moved, never aliased
    public Path: string;
    public TempFile(path: string) { Path = path; File.WriteAllText(path, "").Expect("create temp file"); }
    ~TempFile() { _ = File.Delete(Path); }     // runs exactly when the owner goes out of scope
}

static Length(p: Point): float => MathF.Sqrt(p.X * p.X + p.Y * p.Y);  // `in` borrow by default

static Grow(ref p: Point) { p = p with { X = p.X * 2 }; }             // exclusive borrow

static Keep(n: Node, into: List<Node>) { into.Add(n); }               // storing a shared class retains it
```

### 2.4 Unions, options and results

```mora
public union Shape {
    Circle(Radius: float),
    Rect(Width: float, Height: float),
}

public static Area(s: Shape): float => match (s) {
    Circle(let r) => MathF.PI * r * r,
    Rect(let w, let h) => w * h,
};

public static Total(csv: string): Result<int, ParseError> {
    var sum = 0;
    foreach (let field in csv.Split(',')) {
        sum += int.Parse(field.Trim())?;
    }
    return sum;
}
```

## 3. Source text and lexical structure

### 3.1 Source files

- Source files have the extension `.mr` (D-2) and MUST be UTF-8. A leading byte order mark is accepted and ignored. Any other encoding is error `MR0001`.
- A source file MUST be smaller than 4 GiB (2^32 bytes), because offsets are 32-bit; a larger file is error `MR0003`.
- Expressions, statements, types, patterns and member lists MUST NOT nest more than 256 levels deep (D-134). Each one inside another is a level; the fields of a case pattern, `(let r)` in `Circle(let r)`, are part of that pattern. A chain such as `a + b + c`, `x.A().B()` or `if ... else if ...` nests too, one level per link, because each link holds the rest of the chain. Deeper nesting is error `MR0018`, reported once per statement, and no syntax error after it in that statement is reported, since the parser skipped what is too deep. For reporting, each member, union case and enum member is one statement, its `;` included, and the statements and members of a block or member list inside a statement are statements of their own; a member's expression body is not a level of its own. Code a person writes stays far below the limit; generated code that reaches it splits the expression into locals.
- Line endings may be LF or CRLF. The lexer treats CRLF as one newline. A lone CR is whitespace.
- Identifiers are normalized to NFC before comparison. Two identifiers that differ only in normalization are the same identifier.

### 3.2 Comments and doc comments

- `//` to end of line, and `/* ... */`, which does not nest. A block comment that is not closed is error `MR0012`.
- `///` starts a doc comment. Doc comments are **Markdown** (D-21). A doc comment attaches to the next declaration. Inline code in backticks that names a symbol, for example `` `List<T>.Add` ``, becomes a link in the LSP hover and in generated docs.
- A `mora` code block in a doc comment whose first line is `// expect:`, `// error:` or `// warning:` is a test that `mora test` runs (§15.9).
- `#Name(...)` calls an expression macro (§9).

### 3.3 Identifiers

`identifier = XID_Start_or_underscore XID_Continue*` following Unicode UAX #31. A reserved keyword is never an identifier, and there is no escape that makes one usable as a name (D-94). A name that must match outside text, such as a JSON key or an imported C symbol, is given in an attribute: `@JsonName("class")`, `@Import(Name = "default")`.

Naming conventions (PascalCase types and members, camelCase locals and parameters, `_camelCase` private fields, `I` prefix on interfaces) are the style the standard library uses. The compiler does not enforce them; the formatter does not rename anything; lint `naming` (off by default) reports deviations.

### 3.4 Keywords

Reserved keywords (never identifiers):

```text
abstract as async await bool break byte char class const continue default do
double else enum extern false float for foreach if import in indirect int
interface internal is lazy let long macro move mut new nint none nuint operator
out params private public ref return sbyte shared short sizeof stackalloc static
string struct this true uint ulong union unique unsafe ushort using var
weak where while yield
```

`out` has no meaning in the MVP; it stays reserved for variance annotations (§8.8). `import` has no meaning in the MVP either; it stays reserved for inline imports in scripts (§18.5).

Contextual keywords (keywords only in the listed position): `get`, `set`, `value` (indexer accessors, §6.9); `stored` (modifier, §6.9); `ns` (inside `mut(...)`, §6.9); `uses` (after a signature or a function type, §6.14); `match` (before a parenthesized scrutinee and `{`, §7.7, §14.4); `when` (patterns); `and`, `or`, `not` (patterns); `with` (expressions); `file` (modifiers); `nameof` (expression); `global` (`global using`); `arena` (`using arena`, post-MVP); `args` (top-level statements); `code` and `adds` (macros, §9); `from` (`import ... from`, post-MVP, §18.5); `type` (associated types, §8.7); `allows` (`allows move`, `allows ref struct`, §8.2); `any` (`any IShape`, §8.4); `newtype` (declaration start, §6.12); `extension` (declaration start, §6.9); `test` (section position, §15.9); `contract` (section position, §6.13), and inside a contract section `invariant`, `requires`, `fails`, `ensures`, `also`, `old` and `result`; `services` (declaration start, §16.4), and inside a `services` block `singleton`, `scoped`, `transient`, `per`, `override` and `decorate`; `_` (discards in patterns and deconstruction, §7.8).

The compiler knows only Mora's syntax (D-59). A word that is not a keyword is an identifier, and code written in another language's syntax gets the ordinary syntax error, `MR0110`.

### 3.5 Literals

**Integer literals.** Decimal `1234`, hexadecimal `0x4D2`, binary `0b1001`, with `_` separators allowed between digits. Suffixes `u`, `l`, `ul` (any case) pick `uint`, `long`, `ulong`. An unsuffixed literal takes the expected type when there is one and the value fits; otherwise it is the first of `int`, `long` and `ulong` that holds its value. A value fits `float` or `double` when it is exact there, and a `-` written before a literal belongs to its value, so `let b: sbyte = -128;` compiles. A literal of type `nint` or `nuint` must fit `int` or `uint`, so that it is valid on every target; a larger value comes from a cast of a `long` or `ulong`. A literal that does not fit its type is error `MR0010`. A malformed literal (no digits after `0x` or `0b`, a `_` that is not between two digits, an unknown suffix) is error `MR0014`.

**Real literals.** `1.5`, `1e9`, `1.5e-3`, with suffixes `f` (`float`), `d` (`double`). An unsuffixed real takes the expected type when it is `float` or `double`; otherwise `double`, so `let x: float = 1.5;` compiles. The suffix `m` (`decimal`) is reserved (OQ-12) and is error `MR0014`. A real literal too large for its type is error `MR0010`.

**Operations on literals.** An expression whose every operand is an unsuffixed literal, combined only with parentheses, unary `-`, `+` and `~`, and binary `+`, `-`, `*`, `/`, `%`, `&`, `|` and `^`, takes the numeric type it is expected to have as one literal would. Its value is computed exactly for integers and as a `double` computes it for reals, and it is error `MR0010` when the value does not fit that type, an integer too large to compute included (a real result fits unless computing it gives an infinity at any step, so `0.0 / 0.0` fits and `(1e308 * 10.0) * 0.0` does not), so `let f: float = 1.5 * 2.0;` and `let b: byte = 1 + 2;` compile and `let b: byte = 200 + 100;` does not. The value of such an expression is that exact result converted to the expected type, and the program uses it as written rather than evaluating the operators in that type: `let f: float = 1 / 3;` is 0 and `let f: float = ~1;` is -2, because the operators act on the exact integers, and `let x: double = 9007199254740993 - 9007199254740992;` is 1 although the two operands are not exact as `double` values. An integer division by zero, a real where an integer type is expected, and an expression without an expected type follow the rules of §7.12 instead.

**Character literals.** `'a'`. A `char` is a Unicode scalar value (§5.2), so `'𝄞'` (U+1D11E, outside the BMP) is one `char`. Escapes: `\'`, `\"`, `\\`, `\0`, `\a`, `\b`, `\f`, `\n`, `\r`, `\t`, `\v`, `\uXXXX` (four hex digits, not a surrogate), `\u{X...}` (one to six hex digits). Any other escape, `\u` with the wrong number of digits, or a code point that is not a scalar value is error `MR0013`. A character literal holds exactly one character: `''` and `'ab'` are error `MR0015`.

**String literals.**

| Form | Example | Notes |
|---|---|---|
| Regular | `"a\tb"` | Escapes as for characters |
| Raw | `` `C:\temp` ``, ``` ``a`b`` ``` | No escapes; between runs of backticks, on one line or several (below) |
| Interpolated | `$"x = {x}"`, `` $`{x} "q"` ``, `` $$`{{x}} {y}` `` | Holes `{expr}`, `{expr,alignment}`, `{expr:format}`; with `$$` before a raw string, holes are `{{expr}}` |
| UTF-8 bytes | `"abc"u8` | Type `ReadOnlySpan<byte>` |

String literals have type `string`. A literal of at most 15 bytes is an inline value, and a longer one is an immortal static heap object (§13.4). Loading a literal never allocates; an operation that needs a heap string, such as `AsMemory()` on an inline one, allocates as §13.4 says.

**Raw strings.** A raw string has no escapes: its value is the text between its delimiters, as written. It suits paths, regular expressions, quoted text and embedded documents.

```mora
let path  = `C:\data\users.json`;
let regex = `^\d{3}-\d{4}$`;
let quote = `She said "hi"`;
let sql = `
    select id, name
    from users
    `;
let json = $$`{"id": {{user.Id}}}`;
```

- A raw string opens with a run of N ≥ 1 backticks and closes with the next run of exactly N backticks. A run of any other length is text, so N only has to differ from the length of every run inside: ``` ``a`b`` ``` holds `` a`b ``. One backtick more than the longest run inside always works.
- When the opening run is followed by other text on its line, the string is on one line: the closing run is on the same line, and the value is everything between the two runs.
- When the opening run is followed only by whitespace up to the end of its line, the string spans lines. The value starts on the next line and ends before the line of the closing run, which holds nothing but whitespace before the run. That whitespace is removed from the start of every content line, so the value can be indented with the code around it; a line that holds only whitespace may be shorter. Line breaks inside the value are kept as written.
- `$` before a raw string makes it interpolated. With N `$`, a hole is N braces on each side, and a shorter run of braces is text: with one `$` every `{` opens a hole `{expr}`, and in `` $$`{"id": {{id}}}` `` `{{id}}` is a hole and the outer braces are text. A run of N or more `}` outside a hole, or of 2N or more `{`, is an error, so text with braces uses more `$`. The hole syntax (alignment, format) is the same for every interpolated string.

When a raw string goes inside Markdown code, as in this document, an error page or a doc comment, the code span or fence around it needs more backticks than any run inside it.

A `:` at the top level of a hole starts its format, so a conditional expression in a hole goes in parentheses: `$"{(ok ? 1 : 2)}"`.

A string, character literal, raw string or interpolated string that is not closed is error `MR0012`; a regular string ends at the end of its line. Error `MR0016` covers the rules of the other forms: a lone `}` in the text of an interpolated string (write `}}`), more than one `$` before a string that is not raw, in a raw interpolated string with N `$`, a run of N or more `}` outside a hole, or a run of 2N or more `{`, which could open a hole in two ways, and a raw string that spans lines but holds text on its opening line, whose closing backticks do not stand on their own line, or whose content lines do not start with the closing backticks' indentation.

**Other literals.** `true`, `false`, `none` (the empty value of any `T?`, §5.6), `default` (§7.13).

### 3.6 Operators and punctuation

```text
{ } [ ] ( ) . , : ; ? ?? ??= ?. ?[ => -> .. ^ @ $
+ - * / % & | ^ ! ~ << >> >>> == != < > <= >= && ||
= += -= *= /= %= &= |= ^= <<= >>= >>>= ++ --
+% -% *% +%= -%= *%=
```

`#` before an identifier calls an expression macro (§9), and `@` before one names an attribute (§6.11). `+%`, `-%` and `*%` are wrapping arithmetic (§7.12). `->` is reserved; where `=>` belongs, it gets a fix to `=>` (§5.8). A character that cannot start any token, such as `\` or a non-breaking space, is error `MR0017`.

## 4. Program structure

### 4.1 Projects and compilation units

A **project** is a directory with a `mora.toml` manifest (§17.2). Every `.mr` file under `src/` belongs to the project, and the project compiles as one unit: the order of files does not matter. A project builds one artifact: an executable (`kind = "app"`) or a library (`kind = "lib"`).

### 4.2 File layout

```ebnf
CompilationUnit = UsingDirective* TopLevelItem* ;
TopLevelItem    = Attribute* Modifier* ( TypeDecl | FunctionDecl | ConstDecl | StaticDecl | ExtensionDecl ) | Statement ;
```

- **Namespaces come from folders** (D-96). The project's **root namespace** is its `[project] namespace` value, which defaults to the project name in PascalCase (`users-api` gives `UsersApi`). A file directly under `src/` is in the root namespace; a file under a folder of `src/` is in the root namespace followed by each folder name in PascalCase: `src/Store/UserStore.mr` is in `UsersApi.Store`, and `src/http-client/Retry.mr` is in `UsersApi.HttpClient`. A file never names its namespace, so moving a file is how its items change namespace.
- A folder name that does not give a valid identifier in PascalCase (`src/2d/`), or two sibling folders that give the same one (`src/http-client/` and `src/HttpClient/`), is error `MR0121`.
- Code in a namespace finds the items of that namespace and of every enclosing one by simple name (§14.5), among those visible to it (§4.5): a file in `UsersApi.Store` finds the `internal` and `public` items of `UsersApi`. Items of a nested namespace are reached through it, `Store.UserStore`, or opened with `using UsersApi.Store;`.
- **Statements** are allowed at top level only in the entry file (§4.6); elsewhere they are error `MR0125`.
- **Functions** may be declared directly in a namespace, as in the tour's `Area`. They behave as static methods of the namespace. The `static` modifier is allowed there and has no effect.

### 4.3 Using directives

| Form | Meaning |
|---|---|
| `using Std.Net.Http;` | Makes the namespace's types and functions visible by simple name |
| `using static Std.Math;` | Makes the static members of a type visible by simple name |
| `using Json = Std.Json.JsonValue;` | Alias for a type or namespace |
| `global using Std.Collections;` | Any of the above, for every file in the project |

The prelude (§15.1) is an implicit `global using Std;`. A value's destructor runs when its owner goes out of scope (§10.8), so `using` only imports names.

### 4.4 Dependencies

A project names its dependencies only in `mora.toml` (§17.2); source files have no import statement (D-96).

```toml
[dependencies]
core = { path = "../core" }
```

Each entry binds its key, in PascalCase, to the **root namespace** of that dependency, in every file of the project: with the entry above, `Core.Parse(...)` calls a function in the root namespace of the project at `../core`, and `using Core.Text;` opens a namespace inside it. The key decides the name, so two dependencies whose own root namespaces are the same never clash. A key that gives the same name as `Std`, as the project's own root namespace or as another key (`foo-bar` and `FooBar`) is error `MR0501`. The standard library is always available as `Std` and is not listed.

In the MVP an entry names a local project by `path`, relative to the manifest, and that project is compiled from source in the same build. Version, git, URL and registry sources are post-MVP (§18.5); in the MVP they are error `MR0122`. Dependencies never form a cycle: a project that depends on itself through other projects is error `MR0502`.

### 4.5 Visibility

The folder is the default boundary (D-97). A declaration without a visibility modifier is visible in its folder, that is in its namespace:

| Modifier | Visible to |
|---|---|
| `private` | The enclosing type |
| `file` | The declaring file |
| *(none)* | The declaring folder (namespace) |
| `internal` | This project |
| `public` | Every project that depends on this one |

- Nested namespaces are separate folders: an unmarked item of `UsersApi` is not visible in `UsersApi.Store`, nor an unmarked item of `UsersApi.Store` in `UsersApi`. Crossing a folder takes `internal` or `public`, so a subsystem's internals stay inside its folder however large the project grows.
- A type's members default to the folder, like top-level items, so `private` is written only where a member should stay inside its type.
- A declaration is never visible beyond the type that contains it: its **effective visibility** is the narrower of its own and its type's. A `public` member of an unmarked type is visible in the folder.
- A member's signature MUST NOT mention a type whose effective visibility is narrower than the member's effective visibility (`MR0123`): a `public` member of an unmarked type may mention a type visible in the folder.
- Naming a declaration that is not visible from the use is error `MR0129`. Moving a file to another folder can make an unmarked item invisible to code that used it; the error then names both folders and offers a fix that marks the item `internal`.

### 4.6 Entry point

An app has exactly one entry point:

- **Top-level statements** in the entry file (`src/Program.mr` unless `[project] entry` names another). The statements form the body of a generated `Main`. `args` is in scope with type `string[]`. If any statement contains `await`, the generated `Main` is async (§12.2).
- **Or** one function named `Main` declared anywhere in the project, at namespace level or as a `static` method of a type other than an interface, with one of these signatures, optionally `async`: `Main()`, `Main(args: string[])`, `Main(...): int`, `Main(...): Result<(), E>`, `Main(...): Result<int, E>`. An omitted return type means `()`.

The return type of generated `Main` is inferred: `()` when no statement uses `?` or `return`, otherwise the type that the `return` statements and `?` operators require (for example `Result<(), IoError>`).

| Outcome | Process exit code | Output |
|---|---|---|
| Returns normally, `()` or `Ok(())` | 0 | |
| Returns `int` or `Ok(int)` | that value, truncated to the OS range | |
| Returns `Err(e)` | 1 | `error: {e}` on stderr, using `e`'s `IDisplay` (§8.6) |
| Panics | 101 | Panic message, location and, in debug builds, a backtrace on stderr (§11.4) |
| `Environment.Exit(code)` | `code` | Destructors of live values do not run |

Having both top-level statements and a `Main`, or more than one `Main`, is error `MR0124`. Top-level statements in a library, or in an app's file other than the entry file, are error `MR0125`.

### 4.7 Static state

- `const Name: T = expr;` declares a constant. `expr` must be evaluable at compile time (§14.6), and `T` may be any type the evaluator can build: primitives, `string`, enums, plain structs, tuples and unions of these, and `ReadOnlySpan<T>` of these for a constant sequence (`const Primes: ReadOnlySpan<int> = [2, 3, 5, 7];`). A constant cannot have an array type, because array elements can be written; a constant sequence is a read-only view of read-only data, and `.ToArray()` copies it into an array that can be. A constant string longer than 15 bytes, and every heap string inside a constant, is an immortal object in read-only data (§13.3); a constant string of at most 15 bytes is an inline value. Other constants are values in read-only data.
- `static lazy Name: T = expr;` runs `expr` on first access. A cycle between lazy initializers is a panic (`lazy initialization cycle: A -> B -> A`) at run time, and error `MR0126` when the compiler can see the cycle statically.
- `static mut Name: T = expr;` is mutable global state, with `expr` evaluable at compile time. In the MVP, programs are single-threaded (§12.1). Post-MVP, `static mut` of a non-`shared` type is only reachable from the main thread (OQ-6).

## 5. Types

### 5.1 Kinds of type

| Kind | Declared as | Assignment and passing | Freed when | §|
|---|---|---|---|---|
| Value | `struct`, `union`, `enum`, primitives, tuples | Copy, or move if move-only | Its owner goes out of scope or is overwritten | 6.2, 6.6 |
| Shared reference | `class` | Copies the reference and retains | The last strong reference is released | 6.3 |
| Unique reference | `unique class` | Moves the reference | Its single owner goes out of scope | 6.4 |
| Weak reference | `weak T` where `T` is a shared class | Copies the weak reference | Never owns its target | 9.7 |
| View | `ref struct`, `Span<T>`, `ReadOnlySpan<T>`, non-escaping closures | Copy; can never leave the current call | Never owns | 9.6 |

There is no universal base type, and nothing is boxed implicitly except when a value is converted to a dynamic interface value (§8.4).

### 5.2 Primitive types

| Type | Size | Notes |
|---|---|---|
| `bool` | 1 byte | `true`, `false` |
| `byte`, `sbyte` | 1 | |
| `short`, `ushort` | 2 | |
| `int`, `uint` | 4 | |
| `long`, `ulong` | 8 | |
| `nint`, `nuint` | pointer size | Used for sizes and indexes in FFI. `Length` and indexes are `int` |
| `float` | 4 | IEEE 754 binary32 |
| `double` | 8 | IEEE 754 binary64 |
| `char` | 4 | A Unicode scalar value: U+0000 to U+D7FF or U+E000 to U+10FFFF. Not a UTF-16 code unit (D-14) |
| `()` | 0 | The unit type. An omitted return type means `()` |

`decimal` and 128-bit integers are post-MVP (OQ-12).

### 5.3 Strings

`string` is an immutable sequence of UTF-8 bytes that is always valid UTF-8 (D-14). It has value equality: `==` compares contents. A `string` is a 16-byte value: up to 15 bytes are stored inline, and a longer string points to a shared, reference-counted heap object (§13.4, D-75).

- `s.Length` is the length **in bytes**. It does not count UTF-16 code units. `s.Chars()` iterates `char` values.
- Indexing a string by `int` (`s[i]`) is error `MR0130`. The hint offers `s.Chars().ElementAt(i)` for characters or `s.AsBytes()[i]` for bytes. Ranges `s[a..b]` slice by byte offset, returning a new `string`, and panic if an offset is not on a character boundary.
- `s.AsSpan()` returns `ReadOnlySpan<byte>` without copying. For a string of at most 15 bytes the span points into the string value itself, so it borrows the place that holds the string (§10.6); `AsMemory()` copies such a string to the heap first (§13.4). `StringBuilder` (§15.3) builds strings.
- String literals and `const` strings of at most 15 bytes are inline values; longer ones are immortal static heap objects. Neither is retained or released at run time (§13.4).

### 5.4 Arrays

`T[]` is a fixed-length array on the heap: a shared reference type with a reference count, a length and elements stored inline. `new int[10]` creates an array of default values and requires `T : IDefault` (§8.5); a collection expression, `let primes: int[] = [2, 3, 5];`, creates an initialized one (§7.9). Elements are mutable when the array is reached through a mutable place. `a.Length` is `int`. Out-of-bounds access panics.

Multi-dimensional (`T[,]`) and jagged arrays work as `T[][]`; `T[,]` is post-MVP. Fixed-size inline arrays (`InlineArray<T, N>`) are post-MVP.

### 5.5 Tuples

`(int, string)` and named `(Id: int, Name: string)` are value types. Parentheses around one type without a name only group it, so `(int)` is `int`; `(int,)` and `(Id: int)` are tuples of one element. Tuples support deconstruction (`let (id, name) = pair;`), structural equality and hashing when their elements do, and an `IDisplay` implementation. Deconstruction is also an assignment: `(id, name) = pair;` stores each element in the place at the same position on the left, which is a local, a field, an element or a nested tuple of places, and `_` there discards its element, as in a declaration, while still counting toward the number of elements. Each place is assigned (§7.10) and takes the type of its element, so `(a, b) = (1, "x");` checks `1` against `a` and `"x"` against `b` (D-139). `()` is the empty tuple and the unit type.

### 5.6 Option types and `none`

`T?` is `Option<T>` for every type `T`, value or reference (D-6). `Option<T>` is the standard-library union:

```mora
public union Option<T> {
    Some(Value: T),
    None,
}
```

- `none` is a literal that converts to any `T?` and means `None` (D-54). It has no type of its own: `let x = none;` is error `MR0131`. A non-optional type can never hold `none`.
- `T` converts implicitly to `T?`, because `Option<T>` implements `IFrom<T>` (§5.10).
- The option operators work on options: `a ?? b`, `a ??= b`, `a?.Member`, `a?[i]`, and the patterns `none`, `not none`, `T x` and `{ } x` (§7.8).
- Members: `HasValue`, `Value` (panics on `None`, with the message `called Value on none: T?`), `GetValueOrDefault(fallback: T)`, `Expect(message: string)`, `Map`, `AndThen`.
- `T??` is legal and distinct from `T?`. The compiler warns (`MR2001`) when it appears in source.
- **Layout.** `T?` of a type with a niche, an invalid bit pattern the compiler can use for `None`, has the same size as `T` and no tag (D-78). §13.5 lists the types that have one: every pointer type, `string`, `bool`, `char`, enums, unions with a spare tag value, and options of these when their payload has a pattern left over. Other `T?` use a tag byte plus `T`.

Flow typing narrows an option after a check, and the narrowing is a real type change: after `if (user is none) return;` the variable `user` has type `User`, not `User?`.

### 5.7 Result types

```mora
@MustUse
public union Result<T, E> {
    Ok(Value: T),
    Err(Error: E),
}
```

`Result<T, E>` is the only way to report a recoverable error (§11). Members: `IsOk`, `IsErr`, `Value` (panics on `Err`), `Error` (panics on `Ok`), `Expect(string)`, `Map`, `MapError`, `AndThen`, `GetValueOrDefault(T)`. A discarded `Result` is warning `MR2002`; write `_ = expr;` to discard it on purpose.

`Result<T, E>` implements `IFrom<T>` (§5.10, §11.3), so `return value;` works in a function that returns `Result<T, E>`. When `T` and `E` are the same type the conversion is ambiguous and the compiler requires `Ok(value)` (`MR0132`).

### 5.8 Function types

A function type is written with the same arrow as a lambda: the parameter types in parentheses, then `=>` and the result type (D-92).

```mora
Sort<T>(items: List<T>, compare: (T, T) => int) { ... }

items.Sort((a, b) => a.Age - b.Age);       // the lambda has the shape of the type
let isAdult: (User) => bool = u => u.Age >= 18;
let run: () => () = () => Console.WriteLine("tick");
```

- The parentheses are always written, even for one parameter: `(int) => bool`. `() => ()` takes nothing and returns nothing.
- `=>` binds to the right, so `(int) => (int) => int` returns a function. Parentheses group a function type as any type: `((int) => int)?`, `((int) => int)[]`.
- Where `=>` can start a body, after a member's name or a function's parameters, `Name: (int, int) => ...` reads as a tuple type and a body. A member or a return value of function type is written in parentheses there: `OnTick: ((int) => ());`, `Make(): ((int) => int) => x => x * 2;`. Parameters, locals, type arguments and tuple elements take a function type without them.
- `->` is reserved and means nothing; writing it where `=>` belongs is error `MR0110` with a fix to `=>`.
- A lambda or a method group converts to a function type when the signatures match. There is no limit on the number of parameters, and no named function types: a delegate-style name is a `using` alias, `using Comparison<T> = (T, T) => int;`.

A value of a function type is one of two things (§10.9):

- **Non-escaping**: a view (code pointer plus environment pointer on the caller's stack). This is what a lambda becomes when it is passed directly to an `in` parameter.
- **Escaping**: a shared reference to a heap closure object. This is what a lambda becomes in every other position: stored in a field, returned, passed to a `move` parameter, or assigned to a local that is used after it.

A function type parameter declared with the default `in` mode accepts both. A `move` parameter of function type, `move action: () => ()`, accepts only escaping closures. Function types have no equality.

### 5.9 Views

`Span<T>` and `ReadOnlySpan<T>` are views: a pointer and an `int` length over memory the view does not own. User-defined views are declared `ref struct`. View rules are in §10.6. Spans are created from arrays, lists (§10.10), strings, `stackalloc` and other spans.

`Memory<T>` and `ReadOnlyMemory<T>` are the buffers of async code: a strong reference to an array (or a string, for `ReadOnlyMemory<byte>`), an offset and a length. They are copyable, not views, so they can be stored and held across `await`. `memory.Span` gives a view for synchronous work. `List<T>` does not produce `Memory<T>`, because its storage can move.

### 5.10 Conversions

A value converts implicitly from `S` to `T` only when `T` implements the standard-library interface `IFrom<S>` (D-91):

```mora
public interface IFrom<S> {
    static abstract From(value: S): Self;
}
```

The compiler knows no conversion of its own beyond boxing into `any I`: the standard library's types get theirs by implementing `IFrom` like any other type. Until the compiler can look `IFrom` implementations up (roadmap deliverable M3.6), it carries the numeric rows of the table below as a built-in table that stands in for those of `Std` and gives the same conversions (D-135).

A value of type `S` where a value of type `T` is expected, as the initializer of a local declared `T`, a value assigned to a place of type `T`, a value returned from a function that returns `T` or a condition, which expects `bool`, is error `MR0203` unless `S` is `T` or `T` implements `IFrom<S>`.

| From | To | Through |
|---|---|---|
| Integer type | Wider integer type of the same signedness, or wider signed type | `IFrom` implementations in `Std` |
| Integer type | `float`, `double` | `IFrom` in `Std`, only where exact (`int` to `double`, not `long` to `double`) |
| `float` | `double` | `IFrom` in `Std` |
| `T` | `T?` | `Option<T> : IFrom<T>` |
| `T` | `Result<T, E>` | `Result<T, E> : IFrom<T>` (§11.3) |
| A newtype | Its underlying type | An `IFrom` the compiler generates for the newtype (§6.12) |
| `E` | An error type with an `@From` case for `E`, which is never `indirect` | The `IFrom<E>` that `@From` generates (§11.3) |
| `T` where `T : I` | `any I` (dynamic interface value) | Boxing, which changes how the value is stored rather than its type (§8.4) |
| Any numeric | Any numeric | Explicit cast `(T)x`: truncates and wraps for integers, rounds to nearest for floats, saturates float-to-int (NaN becomes 0) |
| Interface value `any I` | Concrete type `C` | Pattern `is C c` or `as C` (gives `C?`). `(C)i` is a checked cast that panics on mismatch |

- An `IFrom` used implicitly MUST be infallible and free of effects: it does not panic, allocate, do I/O or read mutable state. A conversion that allocates, such as boxing a future (§12.2), is an explicit call instead. Effect inference checks this when it arrives; until then it is a rule that `Std` follows and code review enforces.
- A conversion that can fail, or that has a cost the reader should see, is a named function called explicitly: `int.TryFrom(x)` gives `int?`, `Port.From(n)` gives `Port?`.
- Conversions do not chain: `A` to `C` through `B` needs `C : IFrom<A>`.
- There is no implicit conversion between `bool` and integers, between `char` and integers, or from anything to `string`.
- An explicit cast `(T)x` converts only between numeric types; any other cast is error `MR0204`, unless `x` already has type `T` or the cast is from an interface value as the last row allows.
- `implicit operator` and `explicit operator` declarations do not exist.

## 6. Declarations

### 6.1 Overview

```ebnf
TypeDecl  = StructDecl | ClassDecl | UnionDecl | EnumDecl | InterfaceDecl | NewtypeDecl ;
NewtypeDecl = Attribute* Modifier* "newtype" Identifier ":" Type ( "is" Pattern )? ( ";" | TypeBody ) ;   (* §6.12 *)
Modifier  = "public" | "internal" | "private" | "file" | "static" | "mut"
          | "unique" | "shared" | "ref" | "async" | "lazy" | "const" | "indirect"
          | "extern" ;
FunctionDecl = Attribute* Modifier* Identifier TypeParams? "(" Params? ")" ( ":" Type )? Constraints*
               ( Block | "=>" Expr ";" | ";" ) ;   (* ";" only with "extern", §13.9 *)
                                                   (* no ": Type" means the function returns () *)
TypeParams = "<" TypeParam ( "," TypeParam )* ">" ;   (* §8 *)
BaseList   = ":" Type ( "," Type )* ;                 (* interfaces only in the MVP *)
```

Each modifier is written at most once in the modifiers of a declaration, a parameter or a lambda. A repeated one, as in `public public X` or `mut mut N`, is error `MR0111`. The fix removes the repetition when it is written as the first one; a `mut` repeated with another write scope, as in `mut mut(private) N`, gets no fix, because only the author knows which scope is meant (D-149).

Some modifiers exclude each other, and at most one of them is written: a declaration has one visibility (`private`, `file`, `internal` or `public`, §4.5), a parameter one mode (`in`, `ref` or `move`, §10.3), and a method takes `this` one way (`mut` by `ref`, `move` by `move`, §10.3). A later one that conflicts with an earlier one, as in `public private X` or `ref move n: int`, is error `MR0112`, with a fix that removes the later one (D-151).

A type may be generic (§8), may list interfaces it implements, and may nest other type declarations. Every type is sealed: there is no class inheritance in the MVP (OQ-4),, and a base list may only name interfaces.

**Struct or class.** The choice between `struct` and `class` is about meaning, never about cost (D-56):

- A `struct` is a value. It has no identity: assignment copies a copyable struct and moves a move-only one, and two structs are equal when their fields are (§6.2).
- A `class` is an object with identity. Assignment copies a reference to the same object, which is shared and reference counted (§6.3). A `unique class` is an object with identity and one owner (§6.4).

The compiler decides how a value is stored (inline in its owner, on the stack or in a heap cell), under rules that §10.13 will publish (OQ-21). The programmer does not choose it. A struct has no identity, so where it is stored cannot change what a program does.

### 6.2 Structs

```mora
public struct Vector3 {
    public mut X: float;
    public mut Y: float;
    public mut Z: float;

    public Length: float => MathF.Sqrt(X * X + Y * Y + Z * Z);

    public mut Scale(k: float) { X *= k; Y *= k; Z *= k; }
}
```

- A struct is a value type with no identity (§6.1). It is **copyable** when every field is copyable and it declares no destructor; otherwise it is **move-only** (§10.2).
- Fields are read-only unless marked `mut`, as in classes (D-15, §6.3).
- Every struct gets these members without writing them: `IEquatable<T>` with `==` and `!=` comparing fields in order, `IHashable`, and `IDisplay`, which prints `Vector3 { X = 1, Y = 2, Z = 3 }` (§9.6). Each one is generated only when every field supports it. A member the struct declares itself replaces the generated one. A generated member that the program never uses costs nothing: generics are monomorphized and unused functions are not compiled.
- `v with { X = 0 }` creates a copy of `v` with the listed fields replaced. It may name read-only fields, because it creates a new value and does not change `v`. It may name only fields that are visible where it is written. It needs a copyable struct.
- Struct methods receive `this` as an `in` borrow. A method marked `mut` receives `this` as a `ref` borrow and may assign fields; calling it requires a mutable place (`MR0141`).
- `ref struct` declares a view type (§10.6).
- A struct that declares no constructor is created memberwise, `new Vector3 { X = 1, Y = 2, Z = 3 }` (§6.9).

### 6.3 Classes

```mora
public class Counter {
    public Name: string;                     // read-only: set when the object is created
    public mut Step: int = 1;                // written wherever it can be read
    public mut(private) Count: int = 0;      // read anywhere, written only inside Counter

    public IsIdle: bool => Count == 0;                   // getter: checked to be cheap
    public stored Label: string => $"{Name}: {Count}";   // recalculated when Count changes

    public Tick() { Count += Step; }
}

let counter = new Counter { Name = "requests" };
counter.Step = 2;                            // a plain store
counter.Tick();                              // work happens in a method call
```

- A class is a shared reference type with identity: heap-allocated with a reference count (§10.4, §13.3).
- **Fields are read-only unless marked `mut`** (D-15). A read-only field is assigned only in its initializer, a constructor or a memberwise `new` (§6.9). Who may write a `mut` field, and getters, are in §6.9.
- A class gets no generated equality, hashing, printing or `with`: two objects with the same fields are still two objects. `==` on a class is error `MR0233` unless the class implements `IEquatable<T>`, by hand or with `@Equatable` (§9.9). `ReferenceEquals(a, b)` compares identity.
- An object's address is never observable. A value derived from identity, such as an identity hash or an object number in debug output, comes from a per-process counter and is assigned the first time it is asked for, so two runs that receive the same inputs in the same order, I/O completions and task scheduling included, produce the same values (D-61).
- Mutating a `mut` field through any reference is allowed. A `let` local of class type means the local cannot be reassigned; it does not freeze the object.
- A class marked `shared` may be referenced from several threads (post-MVP, §12.6). In the MVP the modifier is parsed, checked for its field rules and otherwise has no effect.

### 6.4 Unique classes

```mora
public unique class FileLock {
    private _path: string;
    private mut _renewals: int;
    public FileLock(path: string) { _path = path; File.WriteAllText(path, "").Expect("lock"); }
    public mut Renew() { _renewals += 1; }
    public move Release() { }                   // consumes the lock; the destructor runs at the end
    ~FileLock() { _ = File.Delete(_path); }
}
```

A unique class is a heap object with exactly one owner. Assigning it, passing it to a `move` parameter or returning it moves it. Using a moved-from variable is error `MR0300` (§10.5). A unique class has no reference count: the owner frees it when it goes out of scope. Like a class, it gets no generated equality, and one that declares no constructor is created memberwise (§6.9).

Because a unique object is reached through one owner, it follows the mutation rules of structs, not of shared classes: its `mut` fields can be assigned only through a `ref` borrow, so methods that assign fields are marked `mut` (they take `ref this`) and calling them requires a mutable place (`MR0141`). An `in` borrow of a unique object is read-only. A method marked `move` consumes the object.

In the standard library, `Future<T>` (the boxed future, §12.2), `TaskHandle<T>` and `Subscription` are unique (D-18). I/O handles (`TcpStream`, `TcpListener`) are shared classes whose destructors close the handle, because async methods need to keep their receiver alive across `await` (§10.3).

### 6.5 Positional structs

`struct User(Id: int, Name: string, Email: string?);` declares a struct whose positional parameters become public read-only fields. Besides the members every struct gets (§6.2), a positional struct gets:

- a constructor from its positional parameters: `new User(1, "Ada", none)`;
- deconstruction: `let (id, name, email) = user;`, and into places that exist, `(id, name, email) = user;` (§5.5), which positional patterns also use (§7.8).

`user with { Name = "Grace" }` and printing as `User { Id = 1, Name = Ada, Email = none }` come from §6.2, as for every struct. A positional struct may declare more members in a body, `struct Point(X: int, Y: int) { public Sum: int => X + Y; }`. Its positional parameters declare its constructor, so it is not created memberwise: `new User { ... }` is error `MR0153` (§6.9).

Data without identity is a positional struct (D-56).

### 6.6 Unions

```mora
public union Json {
    Null,
    Bool(Value: bool),
    Number(Value: double),
    String(Value: string),
    Array(Items: List<Json>),
    Object(Fields: Dictionary<string, Json>);

    public IsNull: bool => this is Null;
}
```

- A union is a value type holding exactly one of its **cases**. Cases are separated by commas. Members, if any, follow a `;` after the last case.
- A case has zero or more positional fields. Cases are not types: `Json.Bool` is a constructor, and `Bool(let b)` is a pattern. Within code where the expected type is the union (a `match` on it, an assignment to it, a return from a function that returns it), cases may be named without the union prefix (D-16). Elsewhere, write `Json.Bool(true)`.
- A union is copyable when every case field is copyable, and move-only otherwise.
- **Recursion.** A union that contains itself without indirection has infinite size and is error `MR0150`. Mark the case `indirect` (or the whole union `indirect union`): its fields then live in an immutable, reference-counted heap cell. `List<Json>` in the example already gives indirection.
- `@NonExhaustive` requires code in other projects to include a `_` arm when matching, so that adding a case is not a breaking change (§18.4).

### 6.7 Enums

`enum Color { Red, Green, Blue = 10 }` declares a closed set of named integer constants backed by `int`, or by another integer type with `enum Color : byte`. Enums are copyable value types with equality, hashing and `IDisplay` (prints the name).

An enum is **closed**: `match` over it is checked for exhaustiveness, and the cast `(Color)7` panics when 7 is not a declared value. `Color.TryFrom(7)` returns `Color?`. An enum marked `@Flags` is open: any combination of its bits is valid, bitwise operators are defined, and a `match` over it is not checked for exhaustiveness.

### 6.8 Interfaces

```mora
public interface IShape {
    Area(): float;
    Name: string => "shape";                // getter with a default implementation
    Sides: int;                             // getter without one
    static abstract Unit: IShape;           // static abstract getter
}
```

- Interfaces declare methods, getters, indexers, operators and static abstract members, with or without default implementations. An interface may extend other interfaces. An interface has no fields: `Sides: int;` declares a getter, which an implementing type provides with a field it can read there, a getter or a `stored` getter (§6.9).
- A struct, class or union implements an interface by listing it in its base list and providing every member that has no default. Implementation is by name and signature; explicit implementation (`IShape.Area(): float => ...`) is allowed and makes the member callable only through the interface.
- Members of different interfaces are different members, even when they have the same name and signature. A type that implements `IReader` and `IWriter`, each with a default `Describe()`, uses `IReader`'s default through `IReader` and `IWriter`'s through `IWriter`. A default is not a member of the implementing type: `pipe.Describe()` compiles only if the type declares `Describe()`, and a public member the type declares implements every interface member it matches.
- A type's own implementation always replaces a default, whether the member is called through a generic parameter or through a dynamic interface value. Inside a default, `this` is the implementing type: generic code calls the default directly, specialized for that type, and a dynamic interface value calls it through the vtable (§13.6).
- An interface may replace a default of an interface it extends (`IShape.Name: string => "round";`), and the more derived default wins. When one member gets different defaults from two interfaces and neither of them extends the other, the implementing type MUST implement the member itself (`MR0235`); the error names both defaults. One default reached through two paths is not a conflict.

  ```mora
  public interface IShape { Name: string => "shape"; }
  public interface IRound : IShape { IShape.Name: string => "round"; }
  public interface IPointy : IShape { IShape.Name: string => "pointy"; }

  public class Badge : IRound, IPointy {
      public Name: string => "badge";  // required: IRound and IPointy disagree about IShape.Name
  }
  ```

- Implementing an interface for a type declared elsewhere (a retroactive implementation) is post-MVP (OQ-5).
- How interfaces are dispatched is in §8.3 and §8.4.

### 6.9 Members

**Declaration order.** Every declaration puts the name first and the type after a colon (D-58). A field is `Balance: decimal`, and a getter `Total: decimal => ...`. A method is its name, its parameters and then `:` and the return type: `public Deposit(amount: decimal): decimal`. A function with no `: Type` after its parameter list returns `()`. A parameter is `amount: decimal`, with any mode or modifier before the name: `ref p: Point`, `params xs: ReadOnlySpan<int>`. The same order holds for locals (§7.10), lambda parameters (§7.6), local functions, union case fields and positional parameters.

```mora
public class Account {
    public mut(private) Balance: decimal = 0;
    public Owner: string;

    public Account(owner: string) { Owner = owner; }

    public Deposit(amount: decimal): decimal {
        Balance += amount;
        return Balance;
    }

    public Reset() { Balance = 0; }
}
```

- Type patterns keep their order: `x is Circle c` and the arm `Circle c => ...` test a type and bind a name; they do not declare anything. Base lists (`class A : I`), `where` clauses, named arguments (`f(x: 1)`) and `: this(...)` use `:` in other positions and do not change.

**Methods** support expression bodies (`=>`), optional parameters with constant defaults (`port: int = 80`), named arguments, `params` arrays (`params xs: ReadOnlySpan<T>`) and overloading by parameter types. Parameter modes are in §10.3.

**Member names.** Within one type, fields (`const` and `static` ones too), getters, nested types and every other member that is not a method share one set of names, and a method may not take a name that one of them has. Methods may share a name only with other methods, as overloads. A second declaration of a name is error `MR0146`, reported there with the first one as a secondary label.

**Overload resolution.** A call chooses among the methods with the called name that are **applicable**: every argument matches a parameter, by position or by name, once omitted optional parameters take their defaults and a `params` parameter is either given an array or span or expanded into the remaining arguments; each argument converts implicitly to its parameter's type (§5.10); and each argument has the form its parameter's mode needs (§10.3). One applicable method is **better** than another when no argument's conversion to it is worse and at least one is better. A conversion to `P1` is better than a conversion to `P2` when it is the identity and the other is not, or when `P1` converts implicitly to `P2` and `P2` does not convert to `P1`. When neither is better, a non-generic method beats a generic one, then a method called without expanding `params` beats one that expands it, then a method that needs no default for an omitted argument beats one that does. The call is an error when no method is applicable, or when no applicable method is better than all the others; the error lists the candidates.

A call of a function with one declaration says directly what is wrong with it (D-136): an argument whose type does not convert to its parameter's type is error `MR0203` at the argument, with the notes and fixes of §5.10, and a missing argument for a parameter without a default, or an argument that no parameter takes, is error `MR0205`, naming the function's signature. With two or more overloads, a call that no overload is applicable to is error `MR0205`, and a call with no best overload is error `MR0206`, each listing the candidates:

```mora
Area(width: int, height: int): int => width * height;
Scale(x: int, factor: long): long => x * factor;
Scale(x: long, factor: int): long => x * factor;

let a = Area(3);        // MR0205: missing an argument for `height`
let b = Scale(2, 3);    // MR0206: both overloads fit equally well
let c = Scale(2, 3L);   // the first overload
```

**Named arguments** follow the positional ones. A positional argument after a named one and a name given twice are error `MR0207` in every call, whatever it calls. A name the function has no parameter for and a name for a parameter that a positional argument already gives are error `MR0207` too; with overloads, a name that an overload has no parameter for only makes that overload not applicable, and a name that every overload's parameters already take by position is `MR0207`. A call of a value whose type is known and is not a function type, as in `let n = 1; n();`, is error `MR0208`. Only the methods visible from the call are candidates (§4.5); a call of a value's method of which no overload is visible is error `MR0129`, as for any name that only invisible declarations have (§14.5).

For a literal, or an operation made only of literals, which takes any numeric type that holds its value (§3.5), the identity is the conversion to the type it has without a target, the first of `int`, `long` and `ulong` that holds an integer and `double` for a real. A conversion of that type under §5.10 comes next, and taking a type only as a literal comes last, so with overloads `F(x: int)` and `F(x: uint)`, `F(1)` calls `F(x: int)`, and with `G(x: long)` and `G(x: float)`, `G(1)` calls `G(x: long)`; between two types of the same kind, the conversion rule above decides.

Until type arguments are inferred (§8.1) and the arguments of a `params` parameter are collected into a span (M2.9), a call that a generic overload, one with a `params` parameter or an `async` function could take is not checked: an `async` function takes every argument by `move` and gives a future (§10.3, §12). An argument whose type is not known yet fits every parameter, and so does a parameter whose type is not known yet, such as an option, a class or a type parameter today. When either leaves more than one overload applicable, the call is not resolved and is not reported; it has a type only when all of those overloads return the same one (D-137). A static member named through a value, which is error `MR0155` (§6.10), is not checked before M3.13 either. A lambda or a `new` without a type passed to a call that is not checked takes its target type when the call is checked, and is not reported before; so is one passed to a method of a value whose methods are not known yet, such as a class's, to a constructor or to an indexer. A function of the standard library gives no target type while the library is not loaded, so `Console.WriteLine(new { X = 1 })` is error `MR0154`.

A lambda or a `new` without a type, or a conditional, `match`, tuple or collection with one in a branch, an arm or an element, takes its type from the parameter it is passed to, before the overloads are compared. With one function to call, that is its parameter of the argument's name or at its position. With overloads, it is the parameter type that every overload able to take the call's arguments has there, when they all have the same one. An overload is able to take them when it takes their count and their names, every argument fits its parameter's mode, and every other argument that takes no type from its parameter, typed first and without a target, fits its parameter's type, as for applicability. When they differ, such an argument is checked without a target type, and a `new` without a type is error `MR0154` at the argument, unless the call is already an error under `MR0205`, `MR0206` or `MR0207`, or another argument or a parameter of one of those overloads has a type not known yet, an argument that is already an error included: that call is not resolved and not reported (D-137, D-150).

**Fields and getters** (D-101, D-103). Two rules hold everywhere: reading a member without parentheses never does heavy work, and `x.F = v` written outside the type, wherever the field's write scope allows it, is always a plain store that runs no code (a field with a `@SoleWriter` method, §6.11, is assigned only through that method). Inside the type, writing an input of a `stored` getter also recalculates that getter; the inputs are writable only there, so no outside write ever does. State lives in fields; a value computed from them is a getter; a write that does work is a method.

- **Fields.** A field's visibility says who reads it (§4.5), and `mut` says who writes it. A bare `mut` field is writable wherever it is readable, which is its effective visibility: `public mut Step: int` in an unmarked type is writable in the folder, and in a `public` type everywhere.
- **Write scopes.** `mut(private)`, `mut(ns)` and `mut(internal)` narrow writes to the type, the folder (namespace) and the project: `public mut(private) Count: int` is read anywhere and written only inside its type. Outside its write scope a field is read-only, so passing it by `ref` there is `MR0321`. `ns` is a keyword only inside `mut(...)`, because an unmarked declaration already means the folder. A scope that is not narrower than the field's effective visibility changes nothing, and lint `redundant-mut-scope` (warn by default) offers a fix to bare `mut`. It is a lint and not an error because it repeats rather than misleads, and narrowing a type's visibility can make a scope redundant without the field's line changing.
- **Getters.** `Name: T => expr;` computes a value on each read. The compiler checks that a getter is cheap: its body reads fields, constants and literals, and calls only functions that the analysis of §6.14 finds do not allocate on the heap, loop, mutate state or fail. Looping includes recursion, but not a call to a standard-library function marked `@Cheap` (§6.11), which asserts amortized constant time such as a hash lookup; mutating is writing a field, an element or a `static mut`, or calling a `mut` method; failing is reaching `panic`, `Value`, `Expect`, `Unreachable` or `Todo` (§11.4), directly or through a call, while the checks of arithmetic, division and indexing do not count. A getter that breaks the rule is error `MR0251`, which names the call and how it breaks it: ``getter `Total` loops (through `Sum`, line 12); make it a method or `stored` ``. These four facts are computed together with effects but are not effects: `uses none` (§6.14) says nothing about them.
- **Stored getters.** `stored Name: T => expr;` keeps a hidden field that the compiler recalculates whenever one of its inputs is written, so a read is one load and the body may allocate or loop. Its inputs are the fields the body reads, and each MUST be read-only or a field of this object writable only inside its type (`mut(private)` or `private mut`), so every recalculation happens inside the type's own constructors and methods, memberwise creation and `with`. Each input's type MUST also be immutable all the way down: a plain type (§10.2), `string`, or a struct, tuple, union or option made only of these. State that can change without a write to an input, such as the elements of a `List` held in a read-only field, would leave the stored value stale, so `stored Size: int => Items.Count` is `MR0252`; such a value is a method. The body MUST use no capability, reach no `dyn` call and read no `static mut` (§6.14). A broken rule is error `MR0252`, naming the input or call. Callers cannot tell a `stored` getter from a plain one, so changing one into the other never breaks them.
- **No accessors on fields or getters.** A member is a field, a getter or a method: `{ get; }`, `{ get; set; }` and other accessor bodies are not Mora syntax (`MR0110`), and there are no setters. A write that validates, converts or notifies is a method, such as `SetEmail(value: string): Result<(), EmailError>`.
- **Indexers** keep their accessors, because D-65 gives indexing a write form: `dict[key]` reads a `TValue?`, `dict[key] = v` writes and `dict[key] = none` removes, through one indexer of type `TValue?`. Only the declaration syntax was open, and in the MVP it stays `this[index: int]: T { get => ...; set => ...; }`, with `value` naming the assigned value in `set`. A `set` accessor on a struct is a `mut` member. A `get` accessor is checked like a getter (`MR0251`). Declaring indexers as a getter and a write method may be revisited after the MVP.
- **Change notification** comes from the `@Observable` macro (§9.9), not from the language (D-104).
- **Not in the MVP:** a getter computed on first read and cached (`memo`), which needs a way to invalidate it when a collection it read changes.

**Constructors.** A constructor is written `public T(...) { ... }`, named after its type. A constructor may first run another constructor of the same type with `: this(...)`, as in `public Counter(name: string) : this(name, 1) { }`; the fields that constructor assigns count as assigned. A class has no parameter list after its name: every field's mutability and ownership is written in a field declaration (D-51). A struct may have one, which makes it a positional struct (§6.5) whose parameters are public read-only fields. Every field without an initializer MUST be assigned on every path before the constructor returns, and `this` MUST NOT be used except to assign fields until then (`MR0140`). Constructors cannot fail; use a static factory that returns `Result<T, E>` when construction can fail.

**Memberwise creation.** A struct, class or unique class that declares no constructor is created by naming its fields (D-57):

```mora
public class Options {
    public Host: string = "127.0.0.1";
    public Port: int;
    public mut Retries: int = 3;
}

let options = new Options { Port = 8080, Retries = 5 };
```

- `new T { F = v, ... }` MUST assign every field that has no initializer (`MR0143`). It may also assign a field that has one, and the value given replaces the initializer. A getter, plain or `stored`, is never assigned.
- Each name in `new T { ... }` MUST be an instance field of `T`, one that every value holds, and MUST appear once. A name that no member of `T` has, a getter, a `static` field and a constant are error `MR0147`, which suggests the closest field the expression does not assign yet, with the distance rule of names (§14.5). A field assigned twice is error `MR0148`, with a fix that removes the repeated assignment (D-143).
- The written field values run left to right, then the initializers of the omitted fields run in declaration order (D-142).
- The initializer of an instance field runs before the value exists, so it has no `this`: it, and a lambda or local function inside it, MUST NOT write `this` and MUST NOT name an instance field, getter or method of the type. Each is error `MR0152`, without a fix. A `static` member and a constant stay allowed (D-145).
- Read-only fields may be assigned, because creating a value is not mutating it.
- `new() { F = v, ... }` does the same where the target type is known. On such a type, `new T()` and `new()` name no fields. A `new` without a type creates its target type, which MUST be a struct or a class. Without a target type, as in `var x = new { X = 1 };`, a `new()` statement, or an operand, a receiver or an interpolation hole, and with a target type of any other kind, as in `let n: int = new();`, it is error `MR0154`, without a fix, because only the author knows which type is meant. A branch of a conditional, an arm of a `match`, an element of a tuple or of a collection, and the result of a lambda have the target type of the value they are part of, or none when it has none; a value assigned to a field of a type that is an error or not checked yet is not reported, so one missing type is one error. A written type of another kind, as in `new int()`, `new T()` for a type parameter or `new Color()` for an enum, is error `MR0154` too (D-148).
- Memberwise creation is available only where every field without an initializer is visible. Where one is not, `new T { ... }` and `new T()` are error `MR0144`. A library type that keeps such a field private is created only through what the library provides.
- Declaring any constructor turns memberwise creation off. A type enforces its invariants with a constructor, or with a static factory that returns `Result<T, E>`. On such a type, and on a positional struct (§6.5), `new T { ... }` without parentheses is error `MR0153`, with a fix that adds `()` when the type declares a constructor visible there that takes no arguments, because it has no parameters or each has a default, and every name in the braces is a field visible there and declared `mut` without a write scope, so it stays writable after that constructor. The names in the braces are still checked as after a constructor call (D-147). On such a type, `new T(args) { M = v }` runs the constructor and then assigns the listed members as ordinary assignments, so it may name only fields writable at that point: a name that is not an instance field of `T` is error `MR0147`, which suggests the closest field visible there and declared `mut` without a write scope, whether assigned already or not, and a field not visible there is `MR0129`, while assigning a field twice is allowed. The `MR0129` fix that widens the field comes only where it makes the program valid: on the first assignment of a field declared `mut` without a write scope, and never for the braces alone of `MR0153`. On a type that declares no constructor and is not positional, `new T(args)` with arguments is error `MR0205`, because no constructor takes them (D-147).

A field without an initializer is required: every way of creating the value sets it. A read-only field can be set only when the value is created.

**Destructors.** `~T() { ... }` declares the destructor (D-17). A class, unique class or struct may have one. It takes no parameters and no modifiers. It runs exactly once, when the value dies (§10.8). Inside it, `this` is a borrow that MUST NOT escape (`MR0142`). A method called from the destructor could still store `this`; the runtime checks after every class destructor that the strong count is still zero, and aborts with `object of type T was resurrected by its destructor` otherwise (D-46). A struct with a destructor is move-only.

**Operators** are `public static` methods named `operator +` and so on, declared name-first like any method: `public static operator +(a: Vec, b: Vec): Vec`. Arithmetic, bitwise, shift and unary operators may be declared. The comparison operators may not (D-90): `==` and `!=` always call `IEquatable<T>.Equals`, and `<`, `>`, `<=` and `>=` always call `IComparable<T>.CompareTo` (§8.5), so equality and ordering can never disagree with the operators. Declaring `operator ==`, `!=`, `<`, `>`, `<=` or `>=` is error `MR0238`; the type implements the interface instead, by hand or with `@Equatable` and `@Comparable` (§9.9).

```mora
@Equatable @Comparable
public struct Money {
    public Cents: long;

    public static operator +(a: Money, b: Money): Money => new Money { Cents = a.Cents + b.Cents };
}
```

**Conversions** are `IFrom<S>` implementations (§5.10). A type declares no conversion operators.

**Extensions** add methods and getters to a type declared elsewhere, without changing its layout (D-98). An extension block names a receiver, declared like a parameter, and holds members that use it:

```ebnf
ExtensionDecl = Attribute* Modifier* "extension" TypeParams? "(" Param ")" Constraints* "{" Member* "}" ;
```

```mora
public extension<T>(list: List<T>) {
    public IsEmpty: bool => list.Count == 0;

    public Second(): T? {
        if (list.Count < 2) {
            return none;
        }
        return list[1];
    }
}

let names: List<string> = ["Ada", "Grace"];
let second = names.Second();
```

- A block contains methods and getters (`Name: T => expr`), which are called on a receiver as if the type declared them. It adds no fields, constructors, destructors or static members.
- The receiver takes a parameter mode like any parameter: `ref list: List<T>` lets members modify a struct in place.
- Extension blocks are declared at namespace level, and their members have ordinary visibility (§4.5). An extension member is in scope where its namespace is, through the enclosing namespaces or a `using` directive, and is looked up after the type's own members (§7.2).
- A block with a type parameter list is generic over the receiver: `extension<T>(xs: IEnumerable<T>) where T : INumber<T>` applies to every source whose elements are numbers.
- There is no other way to declare an extension: a method declares only its own parameters, and a first parameter cannot mark a receiver.

**Retroactive implementations** (post-MVP, OQ-5) will use the same block to declare that an existing type implements an interface: `public extension Point : IDisplay { ... }`. The `:` means "implements", as in a type declaration; the block adds no fields and creates no type. A program has at most one implementation of an interface for a type, which hashing, equality and ordering rely on, so the rule is fixed now (D-98):

- A library may declare `extension T : I` only when it declares `T` or `I`. Two libraries can then never declare the same implementation: the one that declares `T` would have to depend on the one that declares `I` to name it, and the other way round, and dependencies never form a cycle (§4.4).
- An app may declare any implementation, because no other project depends on it. If a library it depends on later declares the same one, the error is reported in the app, with a fix that deletes the app's copy.
- A library that needs an implementation for a type and an interface it does not declare wraps the type in a `newtype` (§6.12), which costs nothing at run time.

### 6.10 Constants and statics

As in §4.7. A type may declare `const` and `static` members, and `static class` declares a type with only static members.

A `static` member belongs to the type, not to a value of it, so it has no `this`. Its body, and the body of a local function or lambda inside it, MUST NOT write `this` and MUST NOT name an instance field, getter or method of the type, which would read `this` implicitly. Each is error `MR0149`, without a fix: the member either takes the value as a parameter or stops being `static`, and only the author knows which (D-144). A type nested in another is a type of its own, so a member of it, `static` or not, has no value of the types around it: its body MUST NOT name an instance field, getter or method of one of them, which is error `MR0149` too, without a fix, while the nested type's own members and its own `this` stay allowed (D-144).

A `static` member is named through its type, as in `P.S()` or `P.K`. Naming one through a value, as in `p.S()` for a static method or `p.K` for a static field or getter, is error `MR0155`, with a fix that writes the type's name in place of the value, `P.S()`: the value plays no part in the access, and reading it there suggests an instance member that does not exist (D-152).

### 6.11 Attributes

Attributes are compile-time only (D-10). An attribute is `@` and a name, with arguments in parentheses when it takes any: `@Json`, `@Cfg(Os = "linux")`. Each attribute has its own `@`, and several may precede one declaration or statement: `@Equatable @Comparable struct Money`. The `[` that starts a collection, a list pattern or a capture list never starts an attribute (D-94). An attribute names a macro (§9), is a helper attribute that a macro reads on the declaration it processes (§9.9), or is one of these built-in attributes:

| Attribute | On | Effect |
|---|---|---|
| `@Name`, `@Name(args)` naming a macro | as the macro allows | Runs an attribute macro (§9) |
| `@Cfg(Os = "...", Arch = "...", Debug = bool, Feature = "...")` | any declaration, any statement | Compiles the item only when every condition holds (§17.2) |
| `@Allow("MR2001")`, `@Warn(...)`, `@Deny(...)` | any declaration | Changes the level of a lint or warning |
| `@MustUse` | type, function | Discarding a value of that type, or that function's result, warns `MR2002` |
| `@NonExhaustive` | union, enum | §6.6 |
| `@Flags` | enum | §6.7 |
| `@Test`, `@Test(Ignore = "reason")`, `@Test(MaxAllocations = n)` | function in a `test` section, or at file level in `tests/` | Test function for `mora test` (§15.9) |
| `@Benchmark`, `@Benchmark(MaxAllocations = n)` | function in a `test` section, or at file level in `tests/` | Benchmark for `mora bench` (§15.9) |
| `@Import(Lib = "c", Name = "strlen")` | `extern` function | FFI import (§13.9) |
| `@Export(Name = "...")` | function | Exported with the C ABI under that name; it keeps its own address when identical functions are folded (§14.14) |
| `@Layout(C)` | struct | C field order and alignment, for FFI |
| `@Inline`, `@Inline(Never)` | function | Inlining hint |
| `@Obsolete("message")` | any declaration | Using it warns `MR2003` with the message |
| `@RequiresCapability("net.listen")` | function | Declares that calling it uses the capability (§6.14, §18.6) |
| `@Blocking` | function | Declares that calling it blocks the executor thread (§6.14) |
| `@Cheap` | function, getter, index `get` accessor; `Std` only | Asserts amortized constant time, such as a hash lookup. The getter check (§6.9) counts a call to it as not looping, whatever it calls; whether it allocates, mutates or fails is still inferred. Using it outside `Std` is error `MR0254` |
| `@SoleWriter("X")` | method | Makes the method the only code that assigns field `X` of its type, apart from the field's initializer, constructors and memberwise creation. Any other assignment, or passing `X` by `ref`, is error `MR0253`, with a fix that calls the method instead. Macros such as `@Observable` put it on the methods they generate (§9.9) |

### 6.12 Newtypes

`newtype Name : T;` declares a new value type with the same layout as `T`, its underlying type (D-62). It may carry a **refinement**, written with the patterns of §7.8 after `is`, that every value of the type satisfies:

```mora
public newtype UserId : long;
public newtype OrderId : long;
public newtype Port : int is >= 1 and <= 65535;
public newtype Percent : double is >= 0.0 and <= 100.0;
public newtype Slug : string is { Length: > 0 and <= 64 };
```

- **Layout.** The same as `T`, with no tag and no header. `Port?` is laid out as every option of a value type is (§5.6).
- **Into the underlying type.** A newtype converts implicitly to `T`, because every `Port` is an `int`: the compiler generates `int : IFrom<Port>` (§5.10).
- **From the underlying type.** A value of the underlying type never converts implicitly; only a constant does, as the next rule says. `UserId.From(value: long): UserId` wraps a value of the underlying type when the newtype has no refinement. With a refinement, `Port.From(value: int): Port?` checks it and returns `none` when it fails. `Port.Parse` parses a `T` and then checks the refinement, and its `ParseError` names the refinement.
- **Constants are checked at compile time.** A constant of the underlying type assigned to a newtype is checked by constant evaluation (§14.6): `let port: Port = 8080;` compiles, and `let port: Port = 70000;` is error `MR0260`, "70000 is not a valid `Port` (requires `>= 1 and <= 65535`)".
- **Distinct types.** A `UserId` is not an `OrderId`, though both are `long`: passing one where the other is expected is an ordinary type mismatch.
- **Derived members.** Equality, hashing, ordering, printing and `@Json` come from the underlying type, so a `UserId` is a JSON number. Deserializing runs the refinement and reports a `JsonError` that names it.
- **No derived arithmetic.** `UserId + UserId` does not compile. `@Arithmetic` opts in to same-type `+` and `-` and to `*` and `/` by a scalar of the underlying type. On a refined newtype, each result is checked against the refinement in every profile, and a result outside it panics: `Port` 65535 plus 1 is not a `Port`.
- **Members.** A newtype may have a body with members, as a struct does. Inside it, `value` names the underlying value and is read-only; a member that needs a different value builds one through `From`.
- **Default value.** A newtype implements `IDefault` (§8.5) when its underlying type does and that default satisfies the refinement, which constant evaluation checks. `Port`'s underlying default, `0`, fails `>= 1`, so `default(Port)`, `new Port[4]` and a `@Default` field of type `Port` are error `MR0190`, unless the body implements `IDefault` with a valid value.

The refinement is checked once, where a value is created (`From`, `Parse`, a constant, deserialization, a default value, an `@Arithmetic` result), so code that receives a `Port` never checks it again. A predicate refinement that calls a function, `newtype Email : string where EmailAddress.IsValid(value);`, comes after the MVP, once effect inference can prove the predicate pure (OQ-22).

### 6.13 Contract sections [post-MVP]

A type states the rules of its members in a `contract` section, linked to each member by name, so the rules never sit on the signature line (D-63). The MVP reserves `contract` at section position; the checks arrive in v0.3.

```mora
public class Account {
    public mut Balance: Cents;
    public mut IsFrozen: bool;

    public Withdraw(amount: Cents): Result<(), AccountError> {
        Balance -= amount;
        return Ok(());
    }

    contract {
        invariant Balance >= 0;

        Withdraw {
            fails with AccountError.Frozen when IsFrozen;
            fails with AccountError.Insufficient when amount > Balance;
            ensures Balance == old(Balance) - amount;
        }
    }
}
```

| Clause | Constrains | Checked | On violation |
|---|---|---|---|
| `invariant cond;` | The type's state | After constructors and after every public member that may assign fields | Panic that blames the member |
| `requires cond;` | The caller's input | Before the body, unless proven or turned off by the profile (below) | Panic that blames the caller |
| `fails with E when cond;` | A declared failure | Before the body, in every build, because it is program behaviour | Returns `Err(E)` and the body does not run |
| `ensures cond;` | The result and the new state | After the body, in `dev` and under `mora test`; trusted in `release` | Panic that blames the member |

- **Linked by name.** An entry names a member, and the member's parameters are in scope. Only when overloads share a name does the entry add parameter types: `Withdraw(Cents) { ... }`. An unknown or ambiguous name is an error with one fix per candidate.
- **Order.** `requires` clauses run first, then `fails` clauses in written order, then the body.
- **Expressions are effect-free.** `result` names the return value inside `ensures`. `old(e)` is valid only inside `ensures`, evaluates `e` on entry and requires a copyable type.
- **Visibility.** `requires` and `fails` refer only to members at least as visible as the member, because a caller must be able to check them.
- **Messages.** A violation prints the clause's source text and the values of its operands, as `#Assert` does (§9.9).
- **Placement.** A type may have several `contract` sections, so an entry can sit next to its member. Free functions use a `contract` section at file level.
- **Interfaces** carry contracts and their implementations inherit them. An implementation may only weaken `requires` (`requires else`, OR-ed) or strengthen `ensures` (`ensures also`, AND-ed).
- **Profiles.** `contracts = "all" | "public-requires" | "none"` in `[profile.*]`; `dev` and `mora test` default to `"all"`, `release` to `"public-requires"`. `fails` clauses are behaviour and run in every profile.
- **Readers.** Package metadata stores the sections. Hover, generated documentation, JSON Schema, `mora diff` and the property tester read them, and a stronger `requires`, a weaker `ensures` or a new `fails` case on a public member is a breaking change.

Single-value ranges belong in newtype refinements (§6.12), so a contract section holds the relational rules, the failures and the postconditions.

### 6.14 Effects

The compiler infers which capabilities each function can use, and a declaration may state an upper bound with `uses` (D-102). Effects are an analysis, not part of any type the programmer has to write.

```mora
public class PriceService {
    private _rates: RateClient;

    // No annotation: hover shows `uses (net.outbound, clock)`
    public async PriceCents(order: Order): Result<int, PricingError> {
        let rate = await _rates.Get(order.Currency, Clock.Now())?;
        return Pricing.Apply(order.TotalCents, rate);
    }
}

public static class Pricing {
    // A checked bound: reading Clock.Now() here is error MR0250
    public static Apply(cents: int, rate: Rate): int uses none {
        return cents * rate.Numerator / rate.Denominator;
    }
}

public interface IComparer<T> {
    Compare(a: T, b: T): int uses none;      // every implementation is checked
}

// A comparator passed here can never do I/O
public Sort<T>(items: List<T>, compare: (T, T) => int uses none) { ... }
```

- **Vocabulary.** The capabilities of §18.6: `fs.read`, `fs.write`, `net.outbound`, `net.listen`, `env`, `clock`, `random`, `process` and `ffi`; `fs` and `net` name both of their halves. The analysis also reports three facts that are not capabilities: `blocking`, a call that blocks the executor thread (§12.1), `dyn`, a call to code the compiler cannot see, and `secret`, a read of a secret's plaintext through `Secret<T>.Use` (§15.11).
- **Sources.** A function marked `@RequiresCapability("...")` uses that capability, a function marked `@Blocking` is blocking, an `extern` function uses `ffi` (§6.11), and a call of `Secret<T>.Use` is `secret` (§15.11). Nothing else introduces an effect; the standard library carries the marks.
- **Inference.** A function's effect set is the union of the effects of the code written inside it, including the lambdas it creates, and of the functions it calls, computed per strongly connected component of the call graph so that recursion needs no annotation. A call through a function-typed `in` parameter adds nothing to the callee: the closure passed there is a view known at the call site (§5.8, §10.9), so its effects are added to the caller. A generic function is analyzed per instantiation, so `list.Sort(compare)` uses exactly what `compare` uses.
- **Dynamic calls.** A call through an escaping function value or an `any I` value adds `dyn`, unless the function type or the interface method has a bound, which then stands for the call.
- **Attribution.** An effect is charged to the function, and so to the package, whose code performs it. A lambda written in an app and passed into a library carries the app's effects, not the library's. Authority travels with values and closures, never with the call stack, which lets §18.6 check grants per package without effect parameters.
- **Bounds.** `uses` follows the return type, or the parameter list when there is none, and comes before `where` and the body. It names one capability, several in parentheses (`uses (fs.read, clock)`), or `none`. An inferred capability outside the bound, or a `dyn` call, is error `MR0250`, which prints the call path that brings it in. A bound on an interface method is checked against every implementation, and a bound on a function type against every lambda or method converted to it. `blocking` and `secret` are not capabilities and are never bounded.
- **`uses none` means no capability.** It does not mean that the function allocates nothing, mutates nothing or never panics; those are separate facts (§6.9).
- **Seeing effects.** Hover shows a function's set, and an optional inlay hint shows it after the signature. `mora explain-effects <function>` prints each effect with the call path that introduces it: `clock <- Clock.Now at src/Price.mr:12`.
- **`blocking-in-async`** (lint, warn by default) reports an `async` function whose set contains `blocking`, with the path, however many calls down the blocking call is.
- The same analysis gives the macro sandbox (§9.7), the getter rules (§6.9) and the capability set of each package (§18.6).

## 7. Expressions and statements

Expressions are evaluated left to right: operands before the operator, the receiver before the arguments, and arguments in order, unless this section says otherwise. Their syntax is in the grammar (§14.4) and this section.

In a method call the receiver expression is evaluated before the arguments. A receiver the method takes by `in`, or by `ref` as a `mut` method does (§10.3), is borrowed after the arguments and the default values, so the method sees what the arguments did to it: in `p.Show(p.Bump())`, `Show` reads `p` as `Bump` left it. A receiver taken by `move` is a value, taken when it is evaluated, as a `move` argument is: in `p.Take(p.Bump())`, `Take` gets `p` as it was before `Bump` ran (D-146).

### 7.1 Operator precedence

From highest to lowest:

| Level | Operators | Associativity |
|---|---|---|
| Primary | `x.y`, `x?.y`, `f(x)`, `a[i]`, `a?[i]`, `x++`, `x--`, `new`, `nameof`, `default`, `sizeof`, `match (x) { ... }`, postfix `x?` (propagation) | left |
| Unary | `+x`, `-x`, `!x`, `~x`, `++x`, `--x`, `^x`, `(T)x`, `await x` | right |
| Range | `x..y`, `x..`, `..y`, `..` | none |
| `with` | `x with { ... }` | left |
| Multiplicative | `*`, `/`, `%`, `*%` | left |
| Additive | `+`, `-`, `+%`, `-%` | left |
| Shift | `<<`, `>>`, `>>>` | left |
| Relational, type | `<`, `>`, `<=`, `>=`, `is`, `as` | left |
| Equality | `==`, `!=` | left |
| Bitwise | `&`, then `^`, then `\|` | left |
| Logical | `&&`, then `\|\|` | left |
| Coalescing | `??` | right |
| Conditional | `c ? a : b` | right |
| Assignment, lambda | `=`, compound assignments, `??=`, `=>` | right |


### 7.2 Member access and calls

- `x.y` on a value of type `T?` is error `MR0162`, with fixes `x?.y` and `x.Value.y`. Flow narrowing (§5.6) removes the need in most code.
- `ref` arguments are marked at the call site, `F(ref x)`. `move` at the call site is optional (§10.3). There are no `out` parameters (D-93): a function that produces several values returns a tuple, an option or a `Result`.
- Method groups convert to function types. Extension members (§6.9) are looked up after the type's own members.

### 7.3 Object creation

`new T(args)`, `new T(args) { F = v }`, target-typed `new()` and `new(args)`, which create their target type and need one that is a struct or a class (`MR0154`, §6.9), and `new T[n]` for an array of `n` default values. Arrays with given elements come from collection expressions (§7.9). `new` on a struct creates a value; on a class, a heap object with a strong count of one; on a unique class, a heap object owned by the receiving place. Besides arrays, `new` creates nothing else (`MR0154`): a union value comes from one of its cases, as in `Json.Bool(true)` (§6.6).

### 7.4 Propagation: `?`

`e?` evaluates `e`, which MUST be a `Result<T, E>` or a `T?`:

- `Ok(v)`, or a `v` that is not `none`, produces `v`;
- `Err(err)` returns `Err(err')` from the enclosing function, where `err'` is `err` converted to the function's error type by identity or through `IFrom` (§11.3); with no such conversion, error `MR0201`;
- `none` returns `none` from the enclosing function, which must return an option type (`MR0202`).

The enclosing function is the innermost function, lambda or async block. Inside a lambda, `?` returns from the lambda.

**With `await`.** In `await e?` the `?` applies to the awaited value: it parses as `(await e)?`. A `Future` is never a valid operand of `?`, so the rule only removes the need for parentheses (D-19).

**With the conditional operator.** After a primary expression, `?` begins a conditional expression when a `:` at the same nesting depth follows before the end of the expression (`;`, `)`, `]`, `}` or `,` at depth zero); otherwise it is propagation. `x?.y` is always conditional access on an option, and so is `x?[i]` written without a space between `?` and `[`; with a space, `flag ? [1] : []` is a conditional with a collection expression. A `?` directly followed by `:` is propagation, so `c ? r? : d` propagates in the first arm. After propagation, member access needs parentheses: `(r?).Name`. Writing `r?.Name` on a `Result` is error `MR0161` with that fix.

### 7.5 Option operators

`a ?? b`, `a ??= b`, `a?.m`, `a?[i]` work on `T?`:

- `a ?? b` evaluates `a`, and gives its value when it holds one; otherwise it evaluates `b` and gives that. When `a` has type `T?`, `b` has type `T` or `T?`, and so does the result.
- `a ??= b` does nothing when `a` holds a value; when `a` is `none`, it evaluates `b` and assigns it to `a`. `a` is a mutable place of type `T?`, and `b` has type `T` or `T?`.
- `a?.m` and `a?[i]` give `none` when `a` is `none`, without evaluating the index or the arguments, and otherwise access the member or element of `a`'s value. The member accesses, calls and indexing that follow in the same chain are skipped too, so `a?.B.C()` is `none` when `a` is. `a?.m` has type `U?` when `m` has type `U`, and `U?` (not `U??`) when `m` already has type `U?`.

### 7.6 Lambdas

```mora
x => x * 2
(x: int, y: int) => x + y
async (req: Request) => Results.Ok()
static (x: int) => x + 1                 // captures nothing
[weak this] () => this?.Refresh()          // weak capture of this
[move conn] async () => await conn.Close() // move a unique value into the closure
```

Parameter and return types are inferred from the target function type. A parameter with a written type puts the name first, `(x: int)` (§6.9). Capture rules and escaping are in §10.9. A lambda's body may use `return`, `?` and, if it is `async`, `await`.

### 7.7 `match` expressions and statements

There is one `match`, written the same way as a statement and as an expression (D-89, D-129):

```mora
match (command) {
    "start" => Start(),
    "stop" when force => Kill(),
    "stop" => {
        Flush();
        Stop();
    }
    _ => Console.WriteLine($"unknown command {command}"),
}

let status = match (store.Find(id)) {
    User u => Results.Json(u),
    none => Results.NotFound(),
};
```

- `match (e) { arms }` evaluates the first arm whose pattern matches and whose guard is true. An arm is `pattern [when guard] => body`, where the body is an expression or a block. A comma separates arms; after a block it may be left out.
- At the start of a statement, a `match` is a statement and the arms' values are discarded. Anywhere else it is an expression of primary precedence: its result type is the best common type of the arms, or the target type. In an expression, a block arm has type `()` unless it always returns or panics, so a block that produces a value is written as a separate function.
- The arms MUST be exhaustive in both positions (`MR0170`, §7.8); `_ => ...` covers the rest. There is no fall-through, and no `case`, `default:` or `break` inside a `match`.
- `match` is a contextual keyword (§3.4). It starts a `match` only when a parenthesized scrutinee and then `{` follow it (§14.4). Anywhere else it is an identifier, so `let match = pattern.Match(text);`, a field named `match` and a call `match(x)` are valid.

### 7.8 Patterns

| Pattern | Example | Matches |
|---|---|---|
| Constant | `0`, `"GET"`, `Color.Red` | Equal value |
| `none`, `not none` | `none` | Option is `None` / `Some` |
| Type | `User u`, `Circle c` on an interface value | Option is `Some` and binds the value; or interface value holds that concrete type |
| Binding | `let x`, `var x` | Anything; binds immutable (`let`) or mutable (`var`) |
| Discard | `_` | Anything |
| Relational | `< 0`, `>= 'a'` | Comparison with a constant |
| Logical | `> 0 and < 10`, `none or ""`, `not 0` | Combination |
| Case | `Circle(let r)`, `Shape.Rect(_, > 0)`, `None` | Union case, with nested patterns for its fields |
| Positional | `(0, let y)` | Tuple, or positional struct by deconstruction (§6.5) |
| Member | `{ Name: "Ada", Age: > 18 }`, `{ } x` | Members match nested patterns; `{ }` matches any value other than `none` |
| List | `[1, 2, .. let rest]`, `[_, _]` | Arrays, lists and spans by length and elements |

`e is pattern` is a `bool` expression; bindings in it are definitely assigned where the expression is known to be true. A binding under `not` is therefore assigned where the expression is false, as in the `else` of `if (o is not User u)`, and a binding in one alternative of `or` is definitely assigned on neither path, because another alternative may be the one that matched. They are in scope from the pattern to the end of the statement that holds the expression: the rest of the condition and both branches of an `if`, the condition and body of a `while`, the rest of a `let` initializer. They are not in scope after that statement, so `if (o is not User u) { return; }` does not leave `u` usable below it (D-132). The bindings of a `match` arm are in scope in its guard and its body.

**Exhaustiveness.** A `match` over a type with finitely many shapes (`bool`, enums without `@Flags`, unions, options, results, tuples of those) MUST cover every value. The compiler checks this with the usefulness algorithm of Maranget (§25). Error `MR0170` lists one missing value, for example `missing case: Rect(_, _)`. An arm that can never match is warning `MR2010`. Matching on integers or strings requires a `_` arm unless the patterns cover every value (for example all 256 `byte` values).

### 7.9 Collection expressions

`[a, b, c]` and spreads `[..xs, d]` build a collection of the target type: `T[]`, `List<T>`, `HashSet<T>`, `Span<T>` (on the stack), `ReadOnlySpan<T>` (static data when every element is constant), or any type with a `static Create(ReadOnlySpan<T>)` method. Without a target type the collection is a `List<T>` (D-20). `[]` without a target type is error `MR0171`.

`[]` with a `Dictionary<TKey, TValue>` target is an empty dictionary (D-99), so `private _users: Dictionary<int, User> = [];` needs no `new()`. A dictionary with entries is built with `new()` and assignments or `Add`; `[]` holds no key-value pairs.

### 7.10 Locals

| Declaration | Meaning |
|---|---|
| `let x = e;` | Immutable, type inferred |
| `let x: T = e;` | Immutable, explicit type |
| `var x = e;` | Mutable, type inferred |
| `var x: T = e;` | Mutable, explicit type |
| `let x;` / `let x: T;` / `var x: T;` | Declared now, definitely assigned before use (`MR0181`); a `let` assigned exactly once |
| `Twice(x: int): int => x * 2;` | Local function, declared name-first like a method (§6.9) |

A local always starts with `let` or `var`. An explicit type goes after the name and a colon.

Reassigning a `let` local is error `MR0182`. Lint `prefer-let` (warn by default) flags a `var` that is never reassigned.

A local declared without a value is definitely assigned at a point when every path that reaches the point assigns it; the body of a `while`, `for` or `foreach` may run no times, so what it assigns counts after the loop only on the paths that leave it by `break`, while a `do` body runs at least once. A condition is followed into the paths where it holds and where it does not: after `c && (x = 1) > 0`, `x` is assigned where the condition holds, after `c || (x = 1) > 0` where it does not, `!` swaps the two, a binding of `e is pattern` is assigned only where the test holds, and a `match` arm whose pattern does not match, or whose guard does not hold, goes on to the next arm with what the pattern and the guard evaluated; past the last arm only a failed guard goes on after the `match`, since a value that no pattern matches is for exhaustiveness (§7.8). A `let` declared without a value is `MR0182` when it is assigned where it may be assigned already: after an assignment on some path, or in a loop whose earlier run assigned it and went back to the loop's start, by reaching the end of its body or by `continue`; a run that leaves the loop after the assignment, by `break`, `return` or a condition of `do` that is the literal `false`, does not count. A deconstructing assignment assigns each of its places, and is `MR0182` for each `let` place that may be assigned already. A lambda's capture list, as in `[move conn]`, reads each local it names where the lambda is declared, so capturing one that is not definitely assigned there is `MR0181`, and a capture assigns nothing. A lambda or a local function sees the locals assigned where it is declared: reading one that is not definitely assigned there is `MR0181`, and assigning a `let` local declared outside it is `MR0182`, because it may run more than once (D-138).

```mora
let total: int;
if (ready) { total = 1; } else { total = 2; }
let shown = total;      // assigned on both paths
let limit: int;
while (ready) { limit = 3; }   // MR0182: an earlier run may have assigned it
```

A local is in scope from its declaration to the end of the block that declares it; a local function is in scope in its own body too, so it can call itself. A parameter is in scope in its function's body. A local, a parameter of a lambda or local function, or a binding of a pattern MUST NOT take the name of a local or parameter that is in scope where it is declared (`MR0180`, D-131). This covers a second declaration in the same block, an inner block, a lambda inside the function, and two bindings of one pattern. Blocks side by side may use one name, because the first one's local is out of scope when the second declares its own:

```mora
if (ok) {
    let total = 1;
} else {
    let total = 2;   // allowed: the other `total` is out of scope
}
let total = 3;       // allowed for the same reason
let total = 4;       // MR0180: `total` is in scope
```

### 7.11 Statements

`if`/`else`, `while`, `do`/`while`, `for`, `foreach`, `break`, `continue`, `return`, blocks, local functions, expression statements (calls, assignments, increments, `await`) and `match`. An assignment (`=`, a compound `op=` or `??=`) stands only where nothing uses its value: as an expression statement, in the update of a `for`, as an arm of a `match` used as a statement, as the whole expression body of a function, constructor, destructor, setter, local function or lambda whose result is `()`, and in the branches of a conditional that is itself in one of those places. A lambda's result is known to be `()` only where a function type is expected, as for an argument of a call of one function (§6.9); a lambda with no expected type, or an argument of an overloaded or unchecked call, is not in such a place. Used as a value, as in `x = y = 5;`, `F(x = 3)`, `let z = (x += 1);` or an interpolation hole, an assignment is error `MR0185`, with a fix that writes `y = 5; x = y;` where a statement can be put before it. `x++` and `x--` are not assignments and can be used as values. The condition of `if`, `while`, `do`/`while` and `for`, like that of `c ? a : b`, is a `bool`, and there is no implicit conversion to `bool` (`MR0203`). The two branches of `c ? a : b` take the type expected of it, or else have one type that the other converts to. **Control flow.** `break` and `continue` act on the innermost `while`, `do`, `for` or `foreach` around them; outside any loop they are error `MR0183`, and a lambda or a local function does not see the loops around it. A function that returns a value, one whose return type is not `()`, ends every path through its block body in `return` with a value, a call of `panic`, `Unreachable` or `Todo` (§11.4), written alone or as `Std.panic`, a loop whose condition is the literal `true` and that no `break` leaves, or a `match` whose every arm ends the path. A path that reaches the end of the body, or `return;` without a value, is error `MR0184`. Code that no path reaches reports nothing about assignment (D-138).

`foreach (let x in xs)`, `foreach (var x in xs)`, `foreach (let x: T in xs)` and `foreach (ref var x in span)` iterate anything with a `GetEnumerator()` method whose result implements `IEnumerator<T>` (§15.5). Mutating a collection while a `foreach` runs over it panics (§10.10). `yield return` iterators are post-MVP (OQ-9). `using arena { }` is post-MVP (§10.12).

### 7.12 Arithmetic

- **Numeric promotion.** The binary operators `+`, `-`, `*`, `/`, `%`, `+%`, `-%`, `*%`, `&`, `|`, `^`, `==`, `!=`, `<`, `>`, `<=` and `>=` on two numeric operands pick one type to work in:
  1. An unsuffixed integer literal operand takes the other operand's type when that type is numeric and holds the literal's value exactly, and an unsuffixed real literal operand takes the other operand's type when it is `float` or `double` (§3.5), so `x * 0.5` with `x: float` works in `float`.
  2. If both operands have the same type and it is `int`, `uint`, `nint`, `nuint`, `long`, `ulong`, `float` or `double`, the operation works in that type.
  3. Otherwise it works in the first of `int`, `uint`, `long`, `ulong`, `float` and `double` that both operands convert to implicitly (§5.10), so `byte + byte` is `int`, `int + uint` is `long` and `int + float` is `double`. When there is none, as for `long` and `ulong`, the expression is error `MR0204` and an explicit cast `(T)x` on one operand is the fix. §5.10 has no implicit conversion to or from `nint` and `nuint`, so they mix with another integer type only through an explicit cast.

  The result has that type; comparisons give `bool`. Among numeric types, `&`, `|`, `^`, `+%`, `-%` and `*%` are defined only on integer types.
- **Unary operators.** Unary `+`, `-` and `~` widen `byte`, `sbyte`, `short` and `ushort` to `int`; `-x` on a `uint` gives a `long`, and on a `ulong` or an `nuint` is error `MR0204`. `~` is defined only on integer types, and `++` and `--` keep the numeric type of their operand.
- **Other operands.** `!`, `&&` and `||` take `bool` operands and give `bool`; `&`, `|` and `^` on two `bool` values give `bool` and evaluate both. `==` and `!=` compare two values of one type, and `<`, `>`, `<=` and `>=` order numbers, `char` values and `string` values (§8.5). `+` on two `string` values concatenates them (§5.3). An operator on operands it is not defined for, such as `string + int`, `!` on an `int` or `<` on two `bool` values, is error `MR0204`.
- **Compound assignment.** `x op= y`, with `x` of type `T`, means `x = (T)(x op y)` with `x` evaluated once: whenever `x op y` is defined, its result converts back to `T` as the cast would, so `b += 1` with `b: byte` compiles although `byte + byte` is an `int`. Overflow follows the rules above, checked or wrapping as for `op`, so `+%=`, `-%=` and `*%=` always wrap. When `x op y` is not defined, as for `s += 1` with `s: string`, it is error `MR0204`.
- **Overflow** in `+`, `-`, `*`, unary `-`, `++` and `--` panics in debug builds and wraps in release builds, unless the profile sets `overflow-checks = true` (§17.2). `+%`, `-%`, `*%` and their compound forms always wrap (D-12).
- Integer division and remainder by zero panic in every build. `int.MinValue / -1` panics.
- **Shifts.** The left operand of `<<`, `>>` and `>>>` widens from `byte`, `sbyte`, `short` and `ushort` to `int`, and the result has its type. The count converts implicitly to `int` and is masked to the width of the result type: `count & 31` for a 32-bit type and `count & 63` for a 64-bit one. `>>` fills with the sign bit for a signed type and with zeros for an unsigned one; `>>>` is the unsigned shift, which always fills with zeros.
- Floating point follows IEEE 754 with round-to-nearest-even and no traps.

### 7.13 Other expressions

- `default` and `default(T)` require `T : IDefault` (§8.5). A class has no default value, so `default(User)` is error `MR0190`.
- `nameof(x)` gives the identifier as a `string` constant.
- `sizeof(T)` gives the size in bytes of a value type at compile time.
- `x with { F = v }` on structs (§6.2).
- `^i` is an index from the end and `a..b` a range, for arrays, spans, lists and strings.
- String interpolation (§3.5) builds the type its target asks for when that type is an interpolation handler (§7.14). Otherwise it formats each hole through `IDisplay` (§8.6): when the target is a `ref Formatter` parameter the parts are written directly into it, and in every other position the expression allocates one `string`.

### 7.14 Interpolation handlers

An interpolated string builds whatever type its target asks for. A query, an HTML fragment or a log message is written as `$"..."`, its literal parts stay trusted text, and its holes reach the target as typed values rather than as text pasted in (D-105).

```mora
let page: Html = $"<p class=\"greeting\">Hello, {user.Name}</p>";      // the name is escaped as element text
let link: Url = $"https://api.example.com/users/{id}/orders?status={status}";  // each hole encoded for its component
Log.Info($"created user {user.Id} from {req.Path}");                     // fields user.Id and req.Path (§15.10)

let name = ReadName();
let bad: Html = name;            // error: `string` does not convert to `Html`
```

A type is an interpolation handler when it implements `IInterpolate`, whose members refer to the handler as `Self`, as `IFrom` does (§5.10). A hole reaches it typed, when the hole's type implements `IHole<T>` for that handler, or as a displayable value:

```mora
public interface IInterpolate {
    static abstract Begin(literalBytes: int, holeCount: int): Self;
    Enabled: bool => true;            // false skips every part that follows
    mut AppendLiteral(text: Literal);
    mut AppendHole<T>(value: T, format: Literal, source: Literal) where T : IHole<Self>, allows move;
    mut AppendDisplay<T>(value: T, format: Literal, source: Literal) where T : IDisplay, allows move;
}

public interface IHole<THandler> {
    AppendTo(ref handler: THandler, format: Literal);
}
```

- **Lowering.** When the target type `T` of `$"..."` implements `IInterpolate`, the compiler calls `T.Begin` with the total length of the literal parts and the number of holes, then, if `Enabled` is `true`, `AppendLiteral` for each literal part and one call for each hole, in source order. The calls are statically dispatched and inlined like any other generic code (§8.3). The handler value is the expression's value. When `Enabled` is `false` no part is appended and no hole expression is evaluated, which is how a disabled log level costs one branch.
- **Holes.** A hole whose type implements `IHole<T>` for the handler goes to `AppendHole`, so a handler can take some types specially, as `Html` takes a nested `Html` fragment. Any other hole whose type implements `IDisplay` goes to `AppendDisplay`, which receives the value itself, not its text, so the handler decides how to encode it. A hole with neither is an error that names both interfaces. A hole is borrowed for the call (§10.3), so a move-only value can be a hole and stays where it is. `format` holds the text after the hole's expression, alignment included (`,10:F2`), and is empty when there is none. `source` is the hole's expression as written (`user.Id`), which log handlers use as a field name.
- **`Literal`** is text the compiler knows (§15.2): the literal parts of an interpolated string, and a string literal or `const` string written where a `Literal` is expected. Only the compiler creates one, and no function turns a run-time `string` into a `Literal`. Since a handler's trusted text arrives only as `Literal`, text read at run time becomes markup, a query or a command line only through a named escape hatch such as `Html.Unchecked`, which is linted (below).
- **No conversion from `string`.** A handler type does not implement `IFrom<string>`, so `let bad: Html = name;` does not compile. A handler that needs an escape hatch offers a named function, such as `Html.Unchecked(text)`, and lint `unchecked-injection` (warn by default) flags every call.
- **Skipped holes.** Lint `skipped-hole-effects` (warn by default) flags a hole that mutates state or uses a capability (§6.14) when the handler's `Enabled` is not the default, because that hole may not run.
- With no handler as target, an interpolated string is a `string` as before, or writes into a `ref Formatter` (§7.13).

The MVP standard library has `Html` and `Url` (§15.2) and one log message handler per level (§15.10). Handlers for SQL and for process command lines follow the same protocol and come with the database and process libraries, which are post-MVP. A static check of the literal parts at compile time, so that a malformed template is a compile error, is post-MVP too.

## 8. Generics and interfaces

### 8.1 Generic declarations

Types, methods, local functions and interfaces may take type parameters, listed in angle brackets after the name (`class Cache<TKey, TValue>`, `Max<T>(a: T, b: T): T where T : IComparable<T>`). Lambdas are typed from their target.

**Type argument inference.** A call that leaves out a generic method's type arguments has them inferred from its arguments, in two phases. First, each argument whose type is known without a target, which excludes lambdas, `new()`, `none` and collection expressions, is matched against its parameter's type: the two types are compared structurally, and each type parameter met in the parameter's type gets the corresponding part of the argument's type as a candidate. A bare interface type in a parameter, such as `source: IEnumerable<T>`, is an implicit type parameter constrained by that interface (§8.3), so the argument is compared with the instance of the interface that its type implements: a `List<User>` argument gives `T` the candidate `User`. A type that implements the interface for two different type arguments gives no candidate from that argument. An unsuffixed literal gives its default type (§3.5: `int`, `long` or `ulong` for an integer, `double` for a real) as a weak candidate. A weak candidate is dropped when another candidate is a numeric type that holds the literal's value exactly, as for binary operators (§7.12), and otherwise yields to any other candidate it converts to implicitly, so `Max(x, 0)` with `x: float` infers `float`. Second, the compiler repeats two steps until every type parameter is fixed or neither step makes progress: it fixes each type parameter that has candidates, unless it appears in the return type of a parameter whose lambda argument is still waiting and in none of that parameter's parameter types, and it types each waiting lambda whose parameter types are now fixed, matching the lambda's return type against that return type in the same way. So `Select(source, x => x.Name)` fixes `T` from `source`, types the lambda, and then fixes `U` from the lambda's body, and `Fold(items, seed, (a, b) => a + b)` fixes `T` from `items` and `seed` before typing the lambda. A type parameter is fixed to the one candidate that every other candidate converts to implicitly (§5.10), counting a type as converting to itself: `Max(1, 2)` infers `int`, and `Max(1, 2.5)` infers `double`. When the steps stop with a type parameter unfixed, because it has no candidates, no single candidate qualifies, or a lambda can never be typed, the call is an error, and the fix writes the type arguments.

Generics are **monomorphized** (D-11): each distinct instantiation is compiled separately, and a generic type parameter is never boxed. Instantiation is driven from the entry point and from public functions of libraries (§14.7). Polymorphic recursion that would create instantiations without bound is error `MR0220` (depth limit 64).

### 8.2 Constraints

| Constraint | Meaning |
|---|---|
| `where T : IFoo, IBar<T>` | `T` implements the interfaces |
| `where T : struct` | `T` is a value type |
| `where T : class` | `T` is a shared class |
| `where T : unique` | `T` is a unique class |
| `where T : allows move` | `T` may be move-only. Without it, `T` must be copyable (D-23) |
| `where T : allows ref struct` | `T` may be a view type |

A constraint narrows the types a parameter accepts; `allows` widens them, so the two read differently. A type that can be created with no arguments is constrained with `where T : IDefault` (§8.5).

A type parameter is **copyable by default**, so generic code that copies a value (`var a = item; var b = item;`) compiles. Containers that must hold unique values declare `where T : allows move`; inside them, `T` follows the move rules of §10.5. Every standard collection declares `where T : allows move`.

### 8.3 Static dispatch

A call through a constrained type parameter (`item.CompareTo(other)` with `T : IComparable<T>`) is resolved at instantiation and compiled as a direct call. This is the default and has no run-time cost.

A bare interface type is never a box (D-67):

- **In a parameter**, `Print(value: IDisplay)` declares an implicit type parameter: it means `Print<T>(value: T) where T : IDisplay`. Each call is monomorphized and statically dispatched.
- **In a return type**, `Active(): IEnumerable<User>` declares an opaque type: the function returns one concrete type, which callers see only through the interface. Returning two different concrete types is error `MR0237`; the fix is to return `any IEnumerable<User>`.
- **Anywhere else** (a field, a type argument, a local with a written type, an array element), a bare interface type is error `MR0236`, because a value stored there must be boxed; write `any I` (§8.4). The one exception is a service class (§16.4): there a bare interface type in a field or a constructor parameter names the binding of the composition that builds the class (D-123).

```mora
public static Print(value: IDisplay) {
    Console.WriteLine(value);
}

public static TotalArea(shapes: List<any IShape>): double {
    return shapes.Sum(s => s.Area());
}
```

### 8.4 Dynamic interface values

`any I` (`s: any IShape`, `List<any IShape>`) is a **dynamic interface value**: a pair of a data pointer and a vtable pointer (§13.6). `any` is the only way to write one, so every box is visible in the source (D-67).

- Converting a shared class to an interface value retains it; copying the value retains; it is copyable.
- Converting a copyable struct or union **boxes** it: the value moves into a heap cell with a reference count. Boxing is the only implicit allocation in the language. The LSP shows an inlay hint at every boxing site, and lint `boxing` (allow by default) reports them.
- Converting a move-only value to an interface value is error `MR0230` in the MVP (OQ-10).
- `any I` is allowed only if `I` has no static abstract members and no generic methods (`MR0231`). An associated type constrained to an interface `J` is erased to `any J`: through an `any IEnumerable<T>` value, `GetEnumerator()` returns a boxed `IEnumerator<T>`. An opaque return (§8.3), `GetUsers(): IEnumerable<User>`, keeps the concrete iterator and has no box.
- `is C c`, `as C` and `(C)i` test the concrete type through the type identifier in the vtable (§13.6). There is no other run-time type information.
- `any I` implements `I`: it satisfies a constraint `T : I`, and each call through it goes through the vtable.

### 8.5 Built-in interfaces

| Interface | Members | Used by |
|---|---|---|
| `IEquatable<T>` | `Equals(in other: T): bool` | `==`, `!=`, `Dictionary`, `HashSet`, `Contains` |
| `IComparable<T>` | `CompareTo(in other: T): int` | `<`, `>`, `<=`, `>=`, `OrderBy`, `Sort` |
| `IFrom<S>` | `static abstract From(value: S): Self` | Implicit conversions and `?` (§5.10, §11.3) |
| `IHashable` | `Hash(ref h: Hasher)` | `Dictionary`, `HashSet` |
| `IDisplay` | `Format(ref f: Formatter, spec: ReadOnlySpan<byte>)` and `ToString(): string`, each with a default written in terms of the other | Interpolation, `Console.WriteLine`, `ToString()` |
| `IDefault` | `static abstract Default: T;` | `default`, `new T[n]`, `@Default` |
| `IClone<T>` | `Clone(): T` | Explicit deep copies. Copying a class reference never clones |
| `IEnumerable<T>` | `type Enumerator : IEnumerator<T>; GetEnumerator(): Enumerator` | `foreach`, sequence operators |
| `IEnumerator<T>` | `mut Next(): T?`; default `SizeHint(): (Lower: int, Upper: int?)`, which returns `(0, none)` | `foreach`, sequence operators |
| `IFuture<T>` | `mut Poll(ref waker: Waker): Poll<T>` | `await`, `Task.*` (§12.2) |
| `IParse<T>` | `static abstract Parse(text: ReadOnlySpan<byte>): Result<T, ParseError>` | `int.Parse`, `RouteValue<T>`, query values |
| `IInterpolate` | `static abstract Begin(literalBytes: int, holeCount: int): Self`; default `Enabled: bool`, which is `true`; `mut AppendLiteral(text: Literal)`; `mut AppendHole<T>(value: T, format: Literal, source: Literal) where T : IHole<Self>, allows move`; `mut AppendDisplay<T>(value: T, format: Literal, source: Literal) where T : IDisplay, allows move` | Interpolated strings with a handler target (§7.14) |
| `IHole<THandler>` | `AppendTo(ref handler: THandler, format: Literal)` | Holes of an interpolated string with a handler target (§7.14) |
| `INumber<T>` | Arithmetic and comparison operators, `Zero`, `One`, `MinValue`, `MaxValue` as static abstract members | Generic math over primitives |

A type that implements `IDisplay` MUST override at least one of `Format` and `ToString` (`MR0232`); most types override `ToString()`. Because there is no `object`, a type has no `ToString()`, `Equals` or `GetHashCode` unless it implements these interfaces, generates them with a macro or is a type that gets them generated, such as a struct (§8.6). Using `==` on a type without `IEquatable<T>` is error `MR0233`, whose hint suggests `@Equatable` or `ReferenceEquals(a, b)` for identity of shared classes.

### 8.6 Generated implementations

`==`, hashing, ordering, printing, defaults, cloning and JSON are generated by standard-library macros such as `@Equatable` and `@Json` (§9.9). Structs, tuples, enums and unions get `Equatable`, `Hashable` and `Display` automatically (§9.6). Classes do not, because they have identity (§6.1).

### 8.7 Associated types

An interface may declare an associated type, `type Enumerator : IEnumerator<T>;`, and an implementing type binds it with `type Enumerator = ListEnumerator<T>;`. Generic code refers to it as `TSource.Enumerator`. The standard library uses associated types so that sequence operator chains compile to concrete iterator structs (§15.5); application code rarely needs them.

### 8.8 Variance

Generic variance is post-MVP (OQ-3). When it lands it is spelled with `out` and `in` on the type parameter, `interface IProducer<out T>` and `interface IConsumer<in T>`: variance annotations appear only in type parameter lists and parameter modes only in parameter lists, so the two uses of `in` and `out` never meet (D-25). In the MVP, `in` or `out` on a type parameter is error `MR0234`.

## 9. Macros

Macros generate code at compile time (D-48), and they are meant to be quick to write: an author writes no registration boilerplate and never parses a token stream. A Mora macro is a plain Mora function that receives a typed model of the code it is attached to and returns quoted Mora code. Writing one should take minutes, and reading one should need no knowledge beyond this section.

### 9.1 A complete example

```mora
public macro Equatable(t: Type) adds IEquatable<$t> {
    let checks = t.Fields.Where(f => !f.IsWeak).Select(f => code(this.$f == other.$f));
    return code {
        public Equals(in other: $t): bool => $(checks.JoinWith("&&", whenEmpty: code(true)));
    };
}
```

```mora
// expect: True
@Equatable
public class Point {
    public X: int;
    public Y: int;
    public Point(x: int, y: int) { X = x; Y = y; }
}

Console.WriteLine(new Point(1, 2) == new Point(1, 2));
```

### 9.2 Declaring macros

```ebnf
MacroDecl = Attribute* Modifier* "macro" Identifier "(" MacroParams ")" AddsClause? Block ;
AddsClause = "adds" Type ( "," Type )* ;
```

There are two kinds, told apart by the first parameter:

| Kind | First parameter | Used as | Returns |
|---|---|---|---|
| **Attribute macro** | `t: Type` (a struct, class, union or enum) or `m: Member` (a field, getter or method) | `@Name` or `@Name(args)` on the declaration | `code { ... }`: members added to the type |
| **Expression macro** | anything else | `#Name(args)` in an expression | `code(...)`: one expression that replaces the call |

- Further parameters of an attribute macro are its attribute arguments, which must be compile-time constants and may be named and optional: `@Json(NameCase = NameCase.Snake)` calls `macro Json(t: Type, nameCase: NameCase = NameCase.Camel)`.
- Parameters of an expression macro are either constants (`string`, numbers, enums, which the call site must pass as constants) or `Expr`, a typed handle to the argument expression.
- `adds` lists the interfaces an attribute macro makes its type implement. The compiler reads this clause **before** running any macro, so other macros see the interfaces through `Implements` (§9.6). A macro that returns members implementing an interface not listed in `adds`, or lists an interface it does not implement, is error `MR0610`.
- Macros are declared like functions, in any namespace, with the usual visibility. They are found by name through `using` directives, like types. `macro` is a reserved keyword.

### 9.3 Quoting: `code`

`code` builds the output and is the only way to produce code:

| Form | Produces | Example |
|---|---|---|
| `code { members }` | Member declarations | `code { public Count: int => $n; }` |
| `code(expression)` | One expression | `code(this.$f == other.$f)` |
| `code(type)` | A type, when the parenthesized text is a type | `code(List<$t>)` |

The result has type `Code`. Quoted code is ordinary Mora: it is parsed when the macro itself is compiled, so a syntax error inside `code { }` is an error in the macro's project, reported at the macro, and never surfaces in the projects that use it.

**Splices** insert values into quoted code:

| Splice | Value spliced | Inserts |
|---|---|---|
| `$x` or `$(expr)` | `Code` | The code fragment |
| | `Type` | A reference to the type |
| | `Field`, `Getter`, `Method`, `Case` | Its name, as an identifier in member position (`this.$f`) |
| | `Ident` (from `Ident.Of("Get" + f.Name)`) | An identifier |
| | `string`, number, `bool`, enum | A literal |
| `$..xs` | `IEnumerable<Code>` and the like | Each element in sequence: members one after another, or arguments separated by commas |

`JoinWith(separator, whenEmpty:)` on a sequence of `Code` joins expressions with an operator such as `&&` or `+`. A splice whose value does not fit its position (a `Type` where an expression is needed) is error `MR0611`, reported at the splice in the macro source.

### 9.4 Hygiene

- **One scope per expansion.** Every `code` fragment built during one invocation of a macro shares one hygiene scope. When fragments are spliced into each other, the combined code is resolved as one piece, so a name introduced by one fragment is visible in the fragments spliced inside it. In §9.1, `other` inside `code(this.$f == other.$f)` refers to the parameter that the outer `code { }` declares.
- Locals, parameters and labels introduced inside `code` are renamed so that they never collide with the user's names or with another expansion's names.
- Names written literally inside `code` that the expansion itself does not declare resolve where the **macro** is declared. In a macro from `Std.Json`, `JsonWriter` means `Std.Json.JsonWriter` whatever the user's `using` directives say.
- Names inserted with `$` resolve where the macro is **used**.
- Member names the macro declares (`public Equals`) are the type's API and are not renamed. A generated member that clashes with an existing one is error `MR0612`, pointing at both.

### 9.5 The typed model: `Std.Meta`

Macros read a read-only view of the declarations they are attached to. They never see tokens or syntax trees.

| Type | Members |
|---|---|
| `Type` | `Name`, `FullName`, `Kind` (`Struct`, `Class`, `UniqueClass`, `Union`, `Enum`), `IsPositional`, `TypeParameters`, `Fields`, `Getters`, `Methods`, `Cases`, `Attributes`, `Implements(Type)`, `IsCopyable`, `IsPlain`, `IsOptional`, `Underlying` (for `T?`, `T[]`, `List<T>`, `Dictionary<K, V>`), `TypeArguments`, `Location`, `Doc` |
| `Field` | `Name`, `Type`, `IsMut`, `WriteScope`, `IsWeak`, `IsPublic`, `HasInitializer`, `Attributes`, `Location`, `Doc` |
| `Getter` | `Name`, `Type`, `IsStored`, `Inputs` (the fields a `stored` getter reads), `IsPublic`, `Attributes`, `Location`, `Doc` |
| `Method` | `Name`, `Parameters` (`Name`, `Type`, `Mode`, `HasDefault`), `ReturnType`, `IsAsync`, `IsStatic`, `Attributes`, `Location` |
| `Case` | `Name`, `Fields`, `Index` |
| `Attribute` | `Name`, `Arguments` (constants by name) |
| `Expr` | `Type`, `Text` (source text, for messages), `IsConstant`, `Constant`, `Location` |

`Std.Meta` also provides `Fail(location, message)`, which stops the macro and reports a compile error at a location in the user's code, and `Warn(location, message)`. For example, `@Json` on a type with a field of type `TcpStream` reports ``Json cannot serialize `Conn`: `TcpStream` has no @Json and is not a supported type`` on that field.

### 9.6 What a macro sees

Every attribute macro on a type sees the type **as written in source**, plus the interfaces that every macro on any type `adds` (§9.2). It does not see the members other macros generate. This makes expansion independent of order, cacheable per macro and type, and free of cycles: the compiler can run all macros of a build in any order or in parallel and get the same result.

Structs get their `Equatable`, `Hashable` and `Display` automatically (§6.2): the compiler applies the standard-library macros of §9.9 to every struct, tuple, enum and union that does not already declare those members, when every field meets the macro's requirement. Classes are left alone; a class gets these members only from macros written on it.

### 9.7 Running macros

- Macros are compiled with the JIT (§14.16) and run inside the compiler during a build, after declarations and signatures are resolved and before function bodies are type-checked (§14.1).
- **Sandbox.** A macro's effect set (§6.14) MUST contain no capability and no `dyn` call: no files, network, environment, clock or randomness (`MR0613`). Its output therefore depends only on its inputs, which is what makes caching sound.
- **Limits.** Each invocation gets 5 seconds and 256 MiB. A macro that exceeds them, or panics, fails the build with an error at the use site that includes the macro's panic message and location.
- **Caching.** An expansion is cached under a hash of the toolchain version, the model it read, and the MIR of the macro and of every function the macro calls, directly or transitively. Editing a helper that a macro calls therefore reruns that macro; editing an ordinary function body reruns no macro, and editing a field reruns only the macros on that type.
- **Same project.** A macro may be used in the project that declares it, as long as the macro and the functions it calls do not themselves depend on types that use the macro (`MR0614`, which shows the cycle).

### 9.8 Seeing and debugging expansions

- `mora expand [--type UsersApi.User] [file.mr]` prints the source with every expansion inserted, formatted by `mora fmt`.
- The LSP offers "Show expansion" on every macro use and shows the macro's doc comment on hover.
- An error inside generated code has its primary span at the macro use and a secondary span at the line of the `code` block that produced it: ``note: generated by `Json` at std/Json/Macros.mr:42``.
- `Console.WriteLine` inside a macro prints to the build output with the prefix `[macro Json]`, which is the quickest way to debug one.
- `Std.Testing.MacroTest` runs a macro on a source snippet and compares the expansion with an expected snippet, so macro authors can unit-test the output.

### 9.9 Standard-library macros

These macros replace a fixed derive list; the compiler has no special knowledge of them beyond §9.6:

| Macro | Adds | Requirement on fields |
|---|---|---|
| `@Equatable` | `IEquatable<T>` comparing fields in order; for unions, the case and then its fields. `==` and `!=` follow from it (§6.9) | `IEquatable` |
| `@Hashable` | `IHashable`, consistent with `Equatable` | `IHashable` |
| `@Comparable` | `IComparable<T>`, lexicographic by field order; unions order by case declaration order first. The four relational operators follow from it (§6.9) | `IComparable` |
| `@Display` | `IDisplay` in the struct format `Point { X = 1, Y = 2 }`; unions print `Circle(1.5)` | `IDisplay` |
| `@Default` | `IDefault` from field initializers or field defaults; for unions, the case marked `@Default` | `IDefault` |
| `@Clone` | `IClone<T>`, cloning fields that implement `IClone` and copying the rest | copyable or `IClone` |
| `@Json` | `IJsonSerialize` and `IJsonDeserialize` (§16.3) | `IJsonSerialize` / `IJsonDeserialize` |
| `@Error` | On a union or struct: `IError`, with `Code`, `PublicMessage` and `HttpStatus` from `@Code`, `@Public` and `@Status` on each case (§11.2), and for each case with a single field marked `@From`, `IFrom<F>` for that field's type `F`, wrapping the value in the case; never on an `indirect` case (§11.3) | `IDisplay` on the type |
| `@Arithmetic` | On a newtype (§6.12): same-type `+` and `-`, and `*` and `/` by a value of the underlying type; on a refined newtype each result is checked against the refinement | The underlying type has those operators |
| `@Observable` | On a class: for each `mut` field `X` of type `F`, a method `SetX(value: F)`, marked `@SoleWriter("X")`, that assigns the field and, when the value changed, emits on a generated field `XChanged: Signal<F>` (§15.8); a `stored` getter whose inputs include `X` gets its own signal, which `SetX` emits after the recalculation. Because `SetX` is the field's sole writer, every other assignment to `X` is `MR0253` (§6.11), so no change goes unnotified. Other macros never see the generated signals (§9.6), so `@Equatable`, `@Display` or `@Json` on the same class ignore them | `IEquatable` on `mut` fields |
| `#Assert(condition)` | An assertion that, on failure, prints the condition's source text and the values of its operands: `assertion failed: total > 0 (total = -3)` | |

**Helper attributes.** A macro may read attributes on the declaration it processes and on that declaration's members and cases: `@Json` reads `@JsonName`, `@JsonIgnore`, `@JsonStrict`, `@JsonTag` and `@JsonSecret`, `@Error` reads `@From`, `@Code`, `@Public` and `@Status`, and `@Default` reads `@Default` on a union case. A helper attribute does nothing by itself, and one placed anywhere else is error `MR0615`.

A `weak` field is skipped by `Equatable`, `Hashable` and `Comparable`, and printed as `<weak>` by `Display`. These macros read a type's `Fields`; getters are computed from fields, so they are not compared, hashed, printed or serialized.

### 9.10 Not in the MVP

Macros that add top-level declarations next to their type, macros on namespaces or whole files, macros that read files (an `#Embed` with a declared input), and statement macros. Each would extend §9.2 without changing what exists.

## 10. Memory model

Values are the default, classes are reference counted, unique classes have one owner, and the compiler removes most reference-count traffic statically (D-3). Borrowing uses `in` and `ref`. A borrow never outlives the call that created it, except for views, whose lifetime the compiler checks inside one function body. No lifetime syntax exists.

This section is normative and is the part of the spec the test suite checks most closely (§20.3).

**Enforcement.** Every ownership, move, borrow, view and escape rule in this section is checked at compile time, and a violation is an **error** that fails the build, never a warning (D-49). No flag, attribute or `@Allow` turns one off. Only the following are checked at run time, because they depend on values or on code paths the compiler cannot see, and each one panics or aborts instead of continuing with corrupted memory: a structural change to a collection while a span of it is live or while it is being enumerated (§10.10), a destructor that resurrects its object (§6.9), and reference-count overflow (§13.3). An unsound program can be built only through FFI (§13.9) or the standard library's private `unsafe` dialect (§13.11).

### 10.1 Ownership

Every value has exactly one **owner**, which is a place: a local, a parameter, a field, an element of a collection or array, a closure environment slot, or a temporary.

- For a value type, the owner holds the value inline.
- For a shared class, the owner holds one **strong reference**. The object itself is owned jointly by all strong references and dies when the last one is released.
- For a unique class, the owner holds the only reference. The object dies with its owner.

When an owner dies (§10.8) it drops the value it holds: for a shared reference, a release; for a unique reference or value type, the value's drop.

### 10.2 Copyable and move-only types

| Category | Types | `b = a` does |
|---|---|---|
| Copyable, plain | Primitives, enums, and structs, tuples and unions whose fields are all plain | Bitwise copy |
| Copyable, counted | `string`, arrays, shared classes, `weak T`, dynamic interface values, escaping closures, and structs, tuples and unions that contain any of these and no move-only field | Copy, retaining every reference in it |
| Move-only | Unique classes, `Future<T>`, every struct or union that declares a destructor or contains a move-only field | Move: `a` is no longer usable |
| View | `Span<T>`, `ReadOnlySpan<T>`, `ref struct`, non-escaping closures | Bitwise copy, subject to §10.6 |

A type is **plain** when it contains no reference of any kind. Plain types matter in §10.4, because overwriting a plain value can never free memory.

When a copyable value is used for the last time, the compiler moves it instead of copying it. For types whose drop runs no user code this is visible only through weak references, which may read `none` once the last strong use has passed (§10.8). For types whose drop runs a destructor, §10.8 fixes when it runs.

### 10.3 Parameter modes

| Mode | Declared | At the call site | The callee may | Representation |
|---|---|---|---|---|
| `in` (default) | `x: T` or `in x: T` | `F(a)` | Read; call non-`mut` methods; pass it on as `in`; store it only if `T` is copyable, which copies or retains | Pointer, or the value itself for plain types up to two machine words. No retain |
| `ref` | `ref x: T` | `F(ref a)`, `a` a mutable place | Read, assign, call `mut` methods; store only by copy | Pointer to the caller's place (or a temporary, §10.4) |
| `move` | `move x: T` | `F(a)` or `F(move a)` | Own it: store it, move it, drop it | By value, or a pointer for large values. Ownership transfers |

- Storing an `in` or `ref` parameter of a move-only type is error `MR0310`. The message says: ``cannot store `conn` because `Connection` is unique and the parameter only borrows it; declare the parameter `move conn: Connection` ``.
- A `move` parameter of a copyable type copies the argument unless the argument is a local that is not used afterwards, or the caller writes `move a`. After `move a`, `a` is moved-from even if its type is copyable.
- For a shared class, an `in` parameter borrows the reference: the callee may mutate the object's `mut` fields, since that is what a class is, but cannot make the caller's place point elsewhere.
- **`this`.** In shared class methods, `this` is an `in` borrow of the reference. In unique class, struct and union methods it is an `in` borrow, or a `ref` borrow for `mut` methods. A method marked `move` takes `this` by `move`, consuming the receiver.
- **Async functions** take every parameter as if it were `move` (§12.3). `ref` and view parameters on an async function are error `MR0312`. For async instance methods, `this` follows the same rule: a shared class's `this` is retained into the future, which is why I/O handles are shared classes (§6.4). An async instance method on a unique class, struct or union must be marked `move`, consuming the receiver (`MR0313`). Async I/O takes buffers as `Memory<byte>` and `ReadOnlyMemory<byte>` (§5.9), which own a reference to their storage and so can live across `await`.
- Return values are always owned by the caller.
- **No default on `ref`.** A `ref` argument is a mutable place and a default is a fixed value, so a `ref` parameter with a default value, `F(ref x: int = 1)`, is error `MR0209` in a method and in a local function, with a fix that removes the default and one that removes `ref`.
- **Markers.** An argument is written with the marker its parameter's mode needs: `ref` exactly for a `ref` parameter, and `in` or `move` only for a parameter of that mode. Otherwise the function is not applicable to the call (§6.9); for a function with one declaration it is error `MR0205`, with a fix that adds, changes or removes the marker (D-136). The fix that adds `ref` is offered only before a place of exactly the parameter's type.
- **`ref` arguments have exactly the parameter's type.** The function reads and writes the caller's place itself, so a conversion would change the type of that place: `Bump(ref n)` with `n: int` and `Bump(ref x: long)` is error `MR0203`. An `in` or `move` argument converts to its parameter's type as any argument does (§5.10); a converted `in` argument is a temporary, as when an aliased place is borrowed (§10.4). Whether a `ref` argument is a mutable place is a rule of the memory model, checked with it.

### 10.4 Borrowing from places

A borrow is cheap when the compiler can prove the borrowed place does not change during the call, and costs a copy otherwise. The rule depends on how the place is reached.

**Exclusive places** are locals, parameters, their struct, tuple and union fields, and anything reached through a unique reference held in one of those. Nothing but the current function can reach them during a call, except through closures the function itself created. Borrowing an exclusive place passes a pointer, with no copy and no reference-count change. The compiler checks inside the function that one call does not borrow the same exclusive place twice in conflicting ways (`ref a` together with `in a` or another `ref a`): `MR0320`.

An `in` borrow of a unique object is read-only all the way down: through it, the object's `mut` fields cannot be assigned (§6.4). Two `in` borrows of the same unique object in one call are therefore harmless.

**Aliased places** are fields of shared class objects, array elements, closure environment slots, elements reached through a span or `Memory<T>`, and `static mut` variables. Other code can write them during a call through another reference. Collection elements are not places: `List<T>` and `Dictionary` indexers are methods whose getter returns a copy, so `F(list[i])` borrows a temporary and `ref list[i]` is error `MR0323`. `const` values and initialized `static lazy` variables never change and are borrowed like read-only fields. Borrowing an aliased place follows this table:

| Place | `in` borrow | `ref` borrow |
|---|---|---|
| Plain type | Pointer | Pointer |
| Read-only field (not `mut`), any type | Pointer; the object that holds the field is kept alive for the call | Not allowed, the field is read-only (`MR0321`) |
| `mut` field, element or slot of a copyable non-plain type | Snapshot: a copy, retaining what it contains, released after the call | Copy-in/copy-out: the callee gets a temporary, written back when it returns |
| `mut` field, element or slot of a move-only type | Not allowed (`MR0322`). Use `Take()` or `Std.Mem.Replace` to move it out, then put it back | Same |

The **root** of a borrow through an aliased place is the shared object (or array) that holds the place. The caller keeps the root alive for the duration of the call: it retains the root, unless the root is itself held by an exclusive place that the call cannot change, which is the common case.

Copy-in/copy-out means that code which reads the field through another reference during the call sees the old value until the call returns.

A non-escaping closure that captures a local mutably makes that local aliased for the call it is passed to. If the same call also borrows the local, the borrow is a snapshot.

These rules are what make the following safe. The assignment inside `Print` releases the only other reference to the string that `s` refers to:

```mora
// expect: OLD
class Box { public mut Value: string = "old".ToUpper(); }
static Print(s: string, b: Box) { b.Value = "new"; Console.WriteLine(s); }
let b = new Box();
Print(b.Value, b);   // `b.Value` is a `mut` field of a class: passed as a snapshot
```

### 10.5 Moves

The compiler tracks, for every local and parameter of a move-only type, whether it is initialized, moved or maybe-moved at each point (a flow-sensitive analysis over the IR, §14.8).

- Using a moved or maybe-moved value is error `MR0300`. The diagnostic points at the move and at the use.
- Assigning a new value to a moved-from `var` makes it usable again.
- Moving part of a value out of a local (one field of a struct) is error `MR0301`. Move the whole value, or use `Take()` on a `T?` field, or `Std.Mem.Replace(ref place, newValue)`, which returns the old value.
- Moving a value out of an aliased place is error `MR0302`, with the same hints.
- Moving a value declared outside a loop inside the loop body is error `MR0303` unless the loop assigns it again before the next iteration.
- When a local is maybe-moved at the end of its scope, the compiler keeps a hidden drop flag and drops the value only if the flag says it is still initialized.

### 10.6 Views

A view (§5.9) refers to memory it does not own, so the compiler keeps it inside the function that created it, using the rules below. Each view has a **scope**: the function body, or a narrower block if it refers to a block's local or `stackalloc` memory.

- A view type may appear only as a local, a parameter, a return value or a field of another view type. It may not be a field of a non-view type, a collection element, a type argument without `where T : allows ref struct`, a capture of an escaping closure, or live across an `await` (`MR0340`).
- **Static data.** A `ReadOnlySpan<T>` over immortal read-only data, which the compiler builds for a constant sequence (§4.7), never outlives its source, because that source never dies. It may be the type of a `const`, and it is the only view type that may; copied out of the constant, it follows the rules above like any other view.
- **Returning views.** A function that returns a view returns one that refers to its arguments. At the call site, the result's scope is the narrowest scope among the arguments that are views or borrowed places. `Head(s: ReadOnlySpan<byte>): ReadOnlySpan<byte> => s[..4];` compiles; returning a span of a local array is error `MR0343`.
- **Sources stay put.** While a view derived from a local is live (from its creation to its last use), that local may not be assigned, moved or `ref`-borrowed (`MR0341`). The diagnostic reads ``cannot change `buffer` while `span` (created on line 12) is still used on line 15``.
- **Temporaries.** A view of a temporary that is used after the statement ends is error `MR0342`, with the hint to store the temporary in a local first.
- **Shared sources.** When a view is taken from a shared object (`order.Items.AsSpan()`), the compiler keeps a hidden strong reference to the object for as long as the view is live, so the object cannot die under it. When the source is a snapshot of a `mut` field (§10.4), the snapshot also lives until the view's last use, so `let s = box.Data.AsSpan();` compiles.
- **Hidden owners stay local.** A view that depends on a hidden reference, a snapshot or a collection guard (§10.10) cannot be returned from the function that created it (`MR0344`), because those hidden values die with the function. Views that only depend on the function's arguments (`s[..4]` of a span parameter, `text.AsSpan()` of a `string` parameter, `array.AsSpan()` of an array parameter) can be returned: the argument stays in the caller's place while the view is live, and the caller keeps it alive. A `string` passed `in` is passed by pointer to the caller's place (§10.3), and an inline string's bytes live in that place (§13.4), so a view of an inline string is a borrow of the place that holds the string, not of a heap object, and these rules keep that place in place while the view lives. A `move` parameter or a temporary is held by the callee's own frame, so returning a view of a string received that way is error `MR0343`, as for a local. The hint for `MR0344` suggests taking a `Span<T>` parameter or returning `Memory<T>`. This keeps spans two words wide (§13.5).

Views of growable collections need one more rule, §10.10.

### 10.7 Weak references

- `weak T` is allowed for shared classes only (`MR0350` otherwise): as a field, a local or a capture (`[weak this]`).
- Reading a weak reference gives `T?`: a new strong reference if the object is alive, `none` if it has died. Assigning a `T` stores a weak reference; assigning `none` clears it.
- An object's destructor runs when its strong count reaches zero, even with weak references outstanding. Because a value is released at its last use (§10.8), a weak reference can read `none` before the end of the scope that held the last strong reference. Its memory is freed when the weak count also reaches zero (§13.3).
- **Lint `strong-cycle`** (warn by default, `MR2020`) reports pairs of distinct class types that hold strong references to each other through fields, options or standard collections. The message names the two fields and suggests making the back-reference `weak`. A type referring to itself (a tree of `Node`) is not reported.

### 10.8 Drops and destructors

- Locals are dropped at the end of their scope, in reverse order of declaration, on every exit path: falling off the end, `return`, `break`, `continue` and `?`. A moved-from local is not dropped.
- Temporaries are dropped at the end of the full expression statement that created them, except that a temporary bound by `let` or `var` lives as long as that local.
- A value whose drop runs user code (it has a destructor, or contains something that does) is dropped exactly at those points. A value whose drop only releases memory MUST be released right after its last use, including the last use of any view or hidden owner that depends on it (§10.6), in every build profile (D-70). The early release is visible through weak references: a `weak` reference to the object reads `none` once the last strong use has passed, even inside the scope. Code that needs the object alive until a later point uses it there, for example with `Drop(x)`. Releasing at last use lowers memory use and lets `with` and string `+=` reuse storage (§14.19).
- **Order inside a value.** When a class's strong count reaches zero: its destructor body runs, then its fields are dropped in declaration order, then its memory is freed if no weak references remain. Struct and union values drop the same way (destructor, then fields; for a union, the fields of its current case). Collections and arrays drop their elements in index order.
- Releasing a long chain of objects (a linked list of a million nodes) does not recurse: the runtime releases chains with an explicit work list, so dropping deep structures cannot overflow the stack (§13.3).
- A destructor cannot be called directly (`MR0360`). `Drop(x)`, from the prelude, takes `move x` and ends a value's life early.
- A panic inside a destructor aborts the process (§11.4).

### 10.9 Closures

A lambda becomes one of three things, decided by where it is written (D-43):

| Lambda passed to | Becomes | Captures | Allocation |
|---|---|---|---|
| An `in` parameter whose type is a function type (`f: (T) => ()`) | A non-escaping closure: a view | By reference | None |
| A parameter whose type is a generic type parameter (`pred: TPredicate` with `where TPredicate : (T) => bool`) | A closure struct: an anonymous value type with one field per capture | By value, following the escaping rules below | None, unless a capture is shared |
| Anything else: a `move` parameter, a field, a local, a return value, a collection | An escaping closure: a shared heap object | By value, following the escaping rules below | One environment object |

Inside a callee, an `in` parameter of function type is itself a view: it can be called and passed on as `in`, but not stored (`MR0333`, with the hint to declare it `move`). APIs that keep callbacks, such as `Signal<T>.Subscribe` and `WebApp.MapGet`, take `move` function parameters. Sequence operators take closure structs, which is how §15.5's iterator chains store their predicates with no heap allocation.

**Non-escaping closures.** Their environment lives on the caller's stack and they capture locals by reference. They can read and assign captured variables. This covers `List<T>.ForEach`, `Sort` comparers and callbacks run during the call.

**Escaping closures and closure structs.** Captures follow these rules:

- A captured variable that is never assigned after capture (`let` locals, parameters, `var` locals that are not reassigned) is captured by copy, retaining what it holds.
- A captured `var` that is assigned after capture, by the closure or by the enclosing function, is **shared**: the compiler moves it into a heap cell owned jointly by the function and every closure that captures it. The LSP marks such variables with an inlay hint.
- A move-only variable must be listed as `[move x]` and is moved into the closure (`MR0330` otherwise). The closure owns it and later code in the function cannot use it.
- `[weak x]` and `[weak this]` capture a weak reference; inside the closure the variable has type `T?`.
- `ref` and view-typed variables cannot be captured by an escaping closure (`MR0331`).
- `static` lambdas capture nothing (`MR0332` if they try).

### 10.10 Growable collections and interior views

`List<T>`, `Dictionary<TKey, TValue>`, `HashSet<T>` and `StringBuilder` can reallocate their storage, which would leave a view of the old storage dangling. Each of them keeps a **borrow count**:

- Creating a view of the collection's storage (`list.AsSpan()`) increments the count. The compiler inserts a hidden guard value that decrements it, and the guard lives exactly as long as the span is live (§10.6).
- An operation that changes the collection's structure (`Add`, `Insert`, `Remove`, `Clear`, `EnsureCapacity`, `TrimExcess`, assigning a new key in a dictionary) checks the count first and panics if it is not zero: `collection was modified while a view of it was in use`.
- **Enumerators do not use the count** (D-42). A collection keeps a version number that every structural change increments. An enumerator holds a strong reference to the collection, the version it started with and a position; each `Next()` compares versions and panics with `collection was modified during enumeration` when they differ, and bounds-checks the position. Enumerators are therefore copyable and have no destructor, so they can be boxed (§8.4) and stored inside the iterator structs of sequence operators.
- Writing an element (`list[i] = x`) is not a structural change and is allowed while a view is live; §10.4 already makes it safe.

### 10.11 Leak and misuse detection

Debug builds link the debug runtime (§13.8), which:

- poisons freed memory and traps on a release of a dead object or a strong count underflow (a compiler or runtime bug, reported as `internal error: refcount underflow` with the type name);
- keeps every live shared object on an intrusive list, and at process exit reports objects still alive, grouped by type, with counts. `mora run --leak-check` and `mora test` turn this report into a failure. The report finds strong cycles among the leaked objects (trial deletion, as in Bacon and Rajan, §25) and names the fields that form them;
- does not change release builds, which have neither the list nor the report.

### 10.12 Arenas [post-MVP]

`using arena { ... }` makes every allocation inside the block (including in functions it calls) come from a bump allocator, and frees the arena in one step. Arena objects are still reference counted and still run destructors. The arena counts its live allocations; when the block ends and the count is zero, its memory is freed at once. If some object escaped the block, the arena stays until the last of its objects dies, and the debug runtime warns that the arena outlived its block. This keeps arenas safe without an escape check. Allocator-parameterized collections (`List<T, Arena>`) come with this milestone too (OQ-13).

### 10.13 What the optimizer guarantees

Programmers can rely on these cases having no reference-count operations, or none beyond the release that must happen, in optimized builds (`--release`), and the test suite checks each with IR snapshots (§20.3):

1. Passing a reference as an `in` argument from an exclusive place.
2. The last use of a local, which moves instead of copying.
3. A local that is only ever borrowed, never stored.
4. Returning a newly created object.
5. Reading a read-only field and borrowing it.
6. Calling a method on `this`.
7. A non-escaping closure capturing a reference.
8. Passing a `mut` field of a shared object as an `in` argument to a function that cannot write that field makes no snapshot (§10.4).
9. A retain and release pair around a call to a function that cannot release objects of that type is removed.
10. A value whose drop runs no user code is released immediately after its last use (§10.8).

Debug builds keep these guarantees too.

Allocation and layout guarantees hold in every build profile, so a test that counts allocations (§15.9) gives the same answer in `dev` and `release`. The test suite checks each with MIR snapshots or allocation counts:

1. **Stack promotion.** A class or unique object of at most 1 KiB, or an array of constant length within that size, never touches the heap when its reference is used only in locals of the creating function, as an `in` argument to a parameter that does not escape, as the receiver of a method whose `this` does not escape, or by a non-escaping closure, when no weak reference to it is taken, and when it does not outlive its loop iteration. A unique object qualifies when it is never moved out of its local. A function's promoted objects total at most 16 KiB (§14.18, OQ-23).
2. **Futures.** `await` on a call that is not part of a cycle in the async call graph, direct or mutual (§12.3), allocates nothing for the future's state; allocations the function's body makes count separately. `Task.SpawnDetached` and `scope.Spawn` allocate once, `Future.Box` allocates once, and an `await` in such a cycle allocates once per call (§12.3).
3. **Sequence operators and loops.** A chain of the standard streaming operators (all except `OrderBy`, `OrderByDescending`, `ThenBy`, `ThenByDescending`, `GroupBy`, `Reverse` and `Distinct`, which buffer, §15.5) over an array, span or `List<T>` compiles to one loop with no allocation and no calls into the operators' iterator types. `foreach` over an array or span, and `for (var i = 0; i < a.Length; i++)` over an array or span local or parameter that the loop does not assign, have no per-element bounds check (§14.17).
4. **Strings.** Creating, copying or slicing to a string of at most 15 bytes never allocates; `AsMemory()` on one does (§13.4). `a + b + ...` allocates at most once, and so does `$"..."` when every hole is a primitive, a `string` or a type whose `IDisplay` implementation does not allocate. Allocations made inside a user's `Format` or `ToString` are counted separately (§13.4).
5. **In-place updates.** `with` on a struct stored in a heap cell, at its last use, with one strong reference and no weak reference, updates it in place. `+=` on a local string at its last use appends in place when its allocation has room and no borrow or view of it is live. A loop of `+=` on an unaliased local string runs in amortized linear time (§14.19).
6. **Options.** `T?` is the same size as `T` for every type §13.5 lists as having a niche.
7. **Copy elision.** A value returned by `return new S(...)`, by `return local` when every return path returns that local, as an aggregate literal, or by a call assigned to a fresh local or field is constructed in its final place (D-79).
8. **Sized collections.** `ToList`, `ToArray` and `ToDictionary` on a source whose exact count is known make exactly one allocation for their backing storage, or none when the count is zero (§15.3); the `List<T>` or `Dictionary` object itself is one more unless it is promoted (item 1, §15.5).

The compiler shows what it decided: hover on `new` says `stack` or `heap` and, for `heap`, the reason and its line, and `mora explain-cost` lists every remaining allocation, box, retain in a loop and bounds check (§17.1, D-76).

Where a struct is stored is not observable, because a struct has no identity (§6.1). The rules the compiler follows to choose will be published in this section (OQ-21).

### 10.14 Soundness summary

The rules above keep these invariants, which §20.3's test suite checks:

1. A strong reference always points to a live object.
2. A borrowed reference or pointer always points to a live value for as long as the borrow lasts: §10.3 (borrows end with the call), §10.4 (aliased non-plain places are copied or kept alive), §10.6 (views stay inside their scope and keep their sources in place), §10.10 (growable storage does not move under a view).
3. A move-only value has at most one owner, and a moved-from place is never read: §10.5.
4. Every value is dropped exactly once: §10.5 drop flags, §10.8.
5. In the MVP every program is single-threaded (§12.1), so non-atomic reference counts are race-free. Post-MVP threads keep this by the `shared` rules of §12.6.

## 11. Errors and panics

### 11.1 Two kinds of failure

- **Recoverable errors** (a file is missing, input does not parse, a peer closed the connection) are values of type `Result<T, E>`, returned and handled like any other value.
- **Bugs** (an index out of range, `Value` on `none`, a broken invariant) are **panics**, which end the process.

There are no exceptions (D-8).

### 11.2 Error types

Any type can be an error type. The standard library's errors are unions or structs that implement `IError`:

```mora
public interface IError : IDisplay {
    Source(): (any IError)? => none;   // the underlying error, if any; boxed, since its type varies (§8.4)
    Code: string;                      // stable and dotted: "checkout.card_declined"
    PublicMessage: string? => none;    // text safe to show a client; none means internal
    HttpStatus: int? => none;          // the status an HTTP response for it uses (§16.2)
}
```

One error serves three readers (D-108). `Code` is for programs: alerting, dashboards and clients branch on it, never on message text, so it stays the same when the message changes. `PublicMessage` is the only text that may reach an end user, and an error without one is internal. `ToString()` and the chain of `Source()` are for developers and go to the log (§15.10). `Source()` is a method rather than a getter because boxing an error allocates, which a getter may not do (§6.9).

The MVP standard library defines `IoError`, `ParseError`, `FormatError`, `JsonError`, `HttpError`, `TimeoutError` and `TimeError` (§12.4, §15, §16), whose codes start with `io.`, `parse.`, `format.`, `json.`, `http.`, `task.` and `time.`. An application usually declares one union per layer, with the standard-library macro `@Error` (§9.9):

```mora
@Error
public union AppError {
    @Status(404) @Public("Not found.")
    NotFound(What: string),
    Io(@From Inner: IoError),
    Json(@From Inner: JsonError);

    public ToString(): string => match (this) {
        NotFound(let w) => $"{w} not found",
        Io(let e) => $"I/O error: {e}",
        Json(let e) => $"bad JSON: {e}",
    };
}
```

`@Error` gives each case its `Code`, `PublicMessage` and `HttpStatus` from the attributes `@Code("...")`, `@Public("...")` and `@Status(n)` on the case. A case without `@Code` gets the type's name and the case's name in snake case, joined by a dot: `app_error.not_found`. On a struct the attributes go on the type, and without `@Code` the code is the type's name in snake case. A code is one or more lowercase segments of letters, digits and `_`, joined by dots, and a status is an error status from 400 to 599; `@Error` reports anything else as a compile error at the attribute. Cases without `@Public` are internal, so `Io` and `Json` above never show their text to a client. For a case whose field is marked `@From`, `Source()` returns that field, boxed, so the chain to the original error is kept. Such a field is always copyable: `IFrom<S>.From` borrows its argument (§10.3), so a generated `From` can only store a copyable value, and `@Error` reports a `@From` field of a move-only type at the attribute.

### 11.3 Conversions in `?`

`?` converts an error to the function's error type by identity or through `IFrom` (§5.10): `Err(e)` of type `E` becomes `Err(F.From(e))` when the function's error type `F` implements `IFrom<E>`. It does not search for conversion chains: `A` to `C` through `B` requires `C : IFrom<A>`.

`@Error` on a union adds `IError`, and for every case with a single field marked `@From` it adds the `IFrom` implementation that wraps that field in the case. With the declaration in §11.2, `File.ReadAllText(path)?` works inside a function returning `Result<T, AppError>`, because `AppError : IFrom<IoError>`. A hand-written `IFrom` works the same way, for a conversion that needs more than wrapping.

`@From` MUST NOT mark a field of an `indirect` case, or of any case of an `indirect union` (`MR0151`, D-100). Wrapping a value in such a case allocates its heap cell (§6.6), and a conversion used implicitly does not allocate (§5.10). The case is still built explicitly before propagating: `File.ReadAllText(path).MapError(e => AppError.Io(e))?`.

`Result<T, E>` implements `IFrom<T>` in the standard library, which is what lets `return user;` and `return Results.NotFound();` type-check in functions returning a `Result`.

### 11.4 Panics

A panic happens on:

| Cause | Message |
|---|---|
| `panic("...")` | the message |
| `Value` on `none` or on `Err` | `called Value on none: User?` / `called Value on Err: <error>` |
| `Expect("msg")` failing | `msg: <error>` |
| Index or slice out of range | `index 5 out of range for length 3` |
| Integer overflow (debug builds) | `arithmetic overflow: 2147483647 + 1` |
| Division by zero | `division by zero` |
| Structural change during a view or enumeration | §10.10 |
| Failed checked cast `(C)i` or enum cast | `cannot cast IShape holding Rect to Circle` |
| A hole where an `Html` or `Url` handler cannot encode it (§15.2) | `Html hole inside <script> is not supported` |
| `Debug.Assert(cond)` (debug builds) | `assertion failed: <condition source text>` |
| `Unreachable()`, `Todo()` | `entered unreachable code` / `not yet implemented` |
| Lazy static initialization cycle | §4.7 |
| Stack overflow | `stack overflow` (detected through a guard page) |
| Out of memory | `out of memory allocating N bytes` |

A panic prints `panic at src/Store.mr:42:17: <message>` to stderr and, in debug builds or when `MORA_BACKTRACE=1`, a symbolized backtrace. It then **aborts** the process with exit code 101 (D-26). Nothing unwinds: destructors do not run, and buffered stdout is flushed by the runtime before the abort. A panic inside a destructor, or a second panic while printing the first, aborts at once.

`panic`, `Unreachable` and `Todo` are expressions of the never type, which converts to any type, so `let x = cond ? 1 : panic("no");` compiles. The never type cannot be named in source.

Whether a panic in one task of a server should end only that task is OQ-1.

### 11.5 Reports

A library returns typed errors, so that callers can match on the cases. An application that only needs to report a failure, with what it was doing when it failed, returns `Report` instead (D-108):

```mora
LoadConfig(path: string): Result<Config, Report> {
    let text = File.ReadAllText(path).Context(() => $"reading config {path}")?;
    let config = Json.Deserialize<Config>(text).Context("parsing the config")?;
    return config;
}
```

- `Report` holds the error as `any IError` and a list of context lines, and implements `IError`. Its `Source()` is the wrapped error, and its `Code`, `PublicMessage` and `HttpStatus` are the wrapped error's. `ToString()` prints the newest context line first, then the older ones, then the error and the chain of its `Source()`.
- `Context(message)` on a `Result<T, E>` with `E : IError` turns an `Err` into a `Report` with that line, and on a `Result<T, Report>` it adds a line. `Report()` converts without a line. Both do nothing to an `Ok`. `E` is copyable, as a type parameter is by default (§8.2), because a move-only value cannot be boxed (§8.4); a move-only error is converted by hand. A message that is a string literal costs nothing, and a message with holes is passed as a lambda, `() => $"..."`, so that it is built only when there is an error.
- `?` never converts to `Report` by itself. Wrapping an error boxes it, and a conversion used implicitly does not allocate (§5.10), so `Report` does not implement `IFrom<E>`. A bare `?` on a `Result<T, IoError>` in a function that returns `Result<U, Report>` is error `MR0201`, with fixes that add `.Report()` or `.Context("...")`. The allocation is then written in the code, and it happens only on the error path.

## 12. Async and concurrency

### 12.1 Execution model in the MVP

A Mora program in the MVP runs all Mora code on **one OS thread**. A built-in executor (D-4) runs tasks cooperatively on that thread and waits for I/O through the OS readiness or completion API: epoll on Linux, kqueue on macOS, IOCP on Windows, all through `mio` in the runtime (§13.1).

Single-threaded execution is what lets reference counts be non-atomic in the MVP (§10.14). A blocking call (synchronous file I/O, a long computation) blocks every task; the documentation says so in the API reference for each blocking function. Worker threads, `Task.Run` and `shared` classes come after the MVP (§12.6).

The runtime and the standard library are **thread-ready, not threaded** (D-60), so that threads arrive as an addition rather than a rewrite:

- Every reference-count update goes through one lowering in the compiler and one slow path in the runtime, each with a plain and an atomic form (§13.3). The MVP emits only the plain form; the atomic form is built and tested with it.
- The executor's state (task queue, wakers, timers, reactor registration) lives in a per-thread context, never in globals, so a second thread can run its own executor.
- `Std` keeps no global mutable state: caches, buffers and counters live in values the program owns or in that per-thread context.

**The World** (D-109). Everything a program can observe from outside goes through one runtime seam, the *World*: the wall clock, monotonic time, timers, random numbers, sockets, files, environment variables, the console, and the executor's choice of which ready task to poll next. The World lives in the per-thread context above. A `release` build calls the real World directly, so the seam costs nothing there; `dev` builds and `mora test` call it through a pointer in that context, one load and an indirect call next to a system call. In the MVP the World is internal to the runtime and `Std`: no public type names it, and a program cannot install its own.

- **Scheduling.** Outside tests the executor polls ready tasks first in, first out. Under `mora test` it picks among the ready tasks with a seeded generator, so one seed gives one interleaving (§17.5).
- **Randomness.** `Random.Shared` draws from the World, and `mora test` seeds it from the same test seed.
- **Determinism.** With the seed fixed, a single-threaded test sees the same task order, random numbers, identity numbers (D-61), dictionary order (D-29) and hash keys (§15.3) on every run, as long as its I/O completes in the same order and its clock readings do not change what it does. Real I/O, the clock and `ffi` calls are outside the seed: foreign code reaches the operating system without passing through the World. Effect inference (§6.14) reports which tests use them.
- **Post-MVP.** A public `World` and a simulated one in `Std.Testing.Sim` (a virtual clock that jumps to the next timer, an in-memory network and file system with injected faults, several simulated hosts in one process) build on this seam. Static state (§4.7) is shared between simulated hosts in one process, so that design must give each host its own statics.

### 12.2 Async functions and futures

- `async F(...): T` declares an async function returning `T`. Calling it returns a future whose result is `T` (D-27, described below). Inside it, `return v;` returns a `T`, and `await` may be used.
- An async function with nothing to return omits the return type, as any other function does.
- Writing `async F(): Future<T>` is error `MR0401` with a fix that removes the wrapper, because the declared type is the awaited result.
- Calling an async function returns a **future**: a value of a compiler-generated move-only type that implements `IFuture<T>`, as each lambda gets its own closure type (§10.9). A local takes that type (`let f = GetA();`), and the value is `@MustUse`. `Future<T>`, written in a type, is the boxed form: one unique block that holds a future of any type inline, with its `Poll` and drop functions (§13.3), for storing futures of different functions together (`List<Future<T>>`, a route table). Boxing is always explicit: `Future.Box(f)` moves a future `f` of any type implementing `IFuture<T>` into a new `Future<T>`, with one allocation (D-73, OQ-26). `Future<T>` does not implement `IFrom`, because an implicit conversion may not allocate (§5.10). It also implements `IFuture<T>` by forwarding `Poll` to the future it owns, so it can be awaited. The box moves the future in and erases its type inside `Std`, so it is not an `any` value and `MR0230` does not apply.
- A future is **cold**: calling an async function only builds the state; nothing runs until the future is awaited or spawned. As a consequence, in `var a = GetA(); var b = GetB(); await a; await b;` the two operations run one after the other. `await Task.WhenAll(GetA(), GetB())` runs them concurrently.
- `await e` requires `e` to implement `IFuture<T>` and is only allowed in an async function, an async lambda or top-level statements (`MR0400`).
- `IFuture<T>.Poll(ref waker: Waker): Poll<T>` advances a future. `Poll<T>` is the standard union `Ready(Value: T)` / `Pending`. `Ready` gives the result, and a future that has returned `Ready` is not polled again. `Pending` means the future cannot continue yet and has arranged for `waker` to be woken when it can. `Waker` is a move-only handle with `Wake()`, which schedules its task to be polled again, and `Clone()`; in the MVP it is the index of a task on the current executor (§12.1), not a counted object. Code outside `Std` rarely calls `Poll`, because `await` does.

### 12.3 Lowering

The compiler turns each async function into a state machine struct (§14.9):

- Parameters are moved into the state when the function is called: shared references are retained, copyable values copied, move-only values moved. This is why async functions cannot take `ref` or view parameters (`MR0312`).
- Locals that live across an `await` become fields of the state; other locals stay in the poll function's stack frame.
- No borrow or view lives across an `await` (`MR0340`). The state therefore never points into itself, so futures can move freely and never need to be pinned in place (D-28).
- `await` on a child future stores it in the parent's state and polls it in place, so calling and awaiting allocates nothing for the child's state. The exceptions are a recursive await (next item), and allocations that the child's body makes count separately.
- An `await` on a call that can reach the awaiting function again (a cycle in the async call graph) would give the state an infinite size. The compiler boxes the future at that call, one allocation per recursive call, and an inlay hint says so.
- Locals live at disjoint suspension points may share storage in the state. This is best-effort, and `mora explain-cost` reports state sizes.
- `Task.SpawnDetached(f)` and `scope.Spawn(f)` allocate one block that holds the task's bookkeeping, the future's state, built directly in it, and a slot for the result. `TaskHandle<T>` is a unique value that points into the block, not an allocation of its own.
  - A detached task's block has two owners: the executor while the task runs, and the returned handle until it is awaited or dropped. Dropping the handle leaves the task running, and the block is freed when the last owner lets go.
  - A scope child's block has a third owner, the scope, which keeps it until the scope completes. Dropping the child's handle therefore neither cancels the child nor hides its result: the scope still waits for it and still sees an `Err` it returns.

  The executor keeps free lists of task blocks by size class, and a waker is a task index, not a counted object.
- An object that §10.13 promotes inside an async function lives in its state.

### 12.4 Tasks

| API | Behaviour |
|---|---|
| `await Task.WhenAll(f1, f2, ...)` | Runs the futures concurrently on the current thread and returns a tuple of their results. Up to 8 futures, plus an overload taking `List<Future<T>>` that returns `List<T>` |
| `await Task.WhenAny(f1, f2)` | Returns the first result as a union `First(T1)` / `Second(T2)` and drops (cancels) the other |
| `await Task.Scope(async scope => { scope.Spawn(f); ... })` | Structured concurrency, below. Children run concurrently; the scope completes when its body and every child have completed. Returns the body's result; dropping the scope's future cancels the body and every child |
| `Task.SpawnDetached(f)` | Starts `f` as a detached task on the executor and returns a `TaskHandle<T>`. Awaiting the handle gives `T`; dropping it leaves the task running. Detached tasks still running when `Main` returns are cancelled. Lint `detached-task` (warn by default) reports a call outside `Main` |
| `await Task.Delay(TimeSpan.FromMilliseconds(100))` | Timer |
| `await Task.Yield()` | Lets other tasks run |
| `await Task.WithTimeout(duration, f)` | Runs `f` with a deadline `duration` from now; `Result<T, TimeoutError>`, with `Err` when the deadline passes first |
| `await Task.WithDeadline(at, f)` | The same with an absolute deadline `at: MonoTime` (§15.6) |
| `Task.Remaining` | `TimeSpan?`: the time left before the current task's deadline, or `none` when it has none |

`TimeoutError` is a struct with no fields that implements `IError`; its message is `deadline exceeded` and its code `task.timeout`. It is internal, with no `PublicMessage` or `HttpStatus` (§11.2), so a handler that returns it gets a 500 problem response; the 503 of an expired request deadline (§16.1) comes from the server, not from this error.

Futures given to `scope.Spawn` and `Task.SpawnDetached` own everything they use (§12.3), so a spawned task can never refer to a local of its spawner.

**Scopes fail fast** (D-110). A scope's error type `E` comes from its body: a body that returns `Result<T, E>` makes `Task.Scope` return `Result<T, E>`.

- A child whose result is `Result<U, E>` and that returns `Err` cancels the scope at once: the body and every other child are dropped (§12.5), and the scope returns that `Err`. This happens whether or not the body has awaited the child, so a failure is never lost in a child nobody awaits.
- A child whose result is not a `Result` cannot fail the scope. A child that should not stop its siblings handles its own error and returns a plain value, for example `none`.
- A child whose result is `Result<U, F>`, with `F` not the scope's error type, is error `MR0404`, with a fix that adds `.MapError(...)` to the child. A scope whose body does not return a `Result` has no error type, so every `Result` child is `MR0404` there, and the fix handles the error inside the child instead.
- When the body returns `Err` itself, the scope cancels the children that are still running and returns it. When everything succeeds, the scope waits for the remaining children and returns the body's `Ok`.

**Deadlines** (D-110). A deadline belongs to the running task and covers everything it awaits; it is one field in the executor's task record.

- `Task.WithTimeout` and `Task.WithDeadline` lower it while `f` runs and restore it afterwards. A nested call keeps the earlier of its own deadline and the inherited one, so an inner call can shorten a deadline but never extend it.
- When the deadline passes, the runtime drops `f` (§12.5) and the call returns `Err(TimeoutError)`. Timers and socket operations in `f` therefore end at the deadline without checking anything. A synchronous call such as `File.ReadAllText` blocks the thread (§12.1) and cannot be interrupted: `f` is dropped at its next `await` after the deadline, so the call returns late by the length of the blocking call. Lint `blocking-in-async` (§6.14) reports such calls in async code.
- If `f` completes and the deadline passes before the executor polls `f` again, the result wins: the executor checks deadlines only before it polls a task, so a future that has returned `Ready` is never turned into `Err(TimeoutError)`.
- `scope.Spawn` children inherit the deadline of the task that created the scope. `Task.SpawnDetached` tasks start without one.
- The HTTP server gives each request a deadline (§16.1), so a handler, everything it awaits and its `scope.Spawn` children stop at it with no deadline parameter in any signature. A `Task.SpawnDetached` task started by a handler has no deadline and can outlive the request.

```mora
app.MapGet("/dashboard/{id}", async (req: Request) => {
    let id = req.RouteValue<UserId>("id")?;
    return await Task.Scope(async scope => {
        // Load and Recent return Result<_, HttpError>, the scope's error type
        let profile = scope.Spawn(_profiles.Load(id));
        let orders = scope.Spawn(_orders.Recent(id));     // an Err here cancels the profile load
        let dashboard = new Dashboard { Profile = await profile?, Orders = await orders? };
        return Ok(Results.Json(dashboard));
    });
});
```

### 12.5 Cancellation

Dropping a future cancels it: the next time it would have resumed, it does not, and its state is dropped, which runs the destructors of the locals it held at that `await`. Cancellation is therefore deterministic and needs no token. `CancellationToken` does not exist; deadlines, `WhenAny` and scope cancellation all work by dropping futures.

### 12.6 Threads [post-MVP]

The MVP specifies the rules so that the language does not change when threads arrive (OQ-6):

- A value is **sendable** if it is plain, a `string` or array of sendable elements, a `shared class`, a unique class whose fields are all sendable, or a struct, union, tuple or option of sendable values.
- `shared class` (D-5) uses atomic reference counts. Its fields MUST be read-only and sendable, or of a synchronized type: `Atomic<T>`, `Mutex<T>`, `RwLock<T>`, `Channel<T>`. The compiler checks this in the MVP already (`MR0410`).
- `Thread.Spawn(...)`, `Task.Run(...)` and a multi-threaded executor accept only sendable captures and arguments (`MR0411`). Sending a non-`shared` class is an error with the fix ``mark `Cache` as `shared class` ``.
- Strings and arrays crossing a thread boundary use atomic reference counts. Whether strings are always atomic or switch on first send is OQ-7.

## 13. Runtime and ABI

### 13.1 The runtime library

The runtime is two Rust crates (D-50). `mora-rt-core` is linked into every Mora executable; on Linux, macOS and Windows it arrives inside `libmora_rt.a`, the static library built from `mora-rt`, which bundles its dependencies as Rust static libraries do. A bare-metal target links `libmora_rt_core.a` and its own platform layer instead. Both libraries ship in the toolchain archive (§19.2):

- **`mora-rt-core`** is `#![no_std]` and needs no operating system: reference counting and the release work list, weak references, panic formatting, number formatting and parsing, UTF-8, hashing and the executor's task queue. It reaches the platform only through the platform interface of §19.4.
- **`mora-rt`** is the hosted platform layer for Linux, macOS and Windows. It implements the platform interface with the Rust standard library and adds the hosted services: process startup, files, sockets, the `mio` reactor, backtraces and HTTP parsing.

Together they provide:

| Area | Contents |
|---|---|
| Startup | The process entry symbol, `main`, called by the platform's C runtime: sets up the panic and stack-overflow handlers, collects arguments and environment, calls `int32_t __mora_main(void)`, which the compiler emits to run the program's `Main` and map its result to an exit code (§4.6), flushes stdout and returns that code |
| Memory | Allocation through the bundled allocator (below), the release slow path (destructor call, field drops, the iterative release work list), weak reference bookkeeping, debug-mode poisoning and leak tracking (§10.11) |
| Panics | Message formatting, backtraces through the `backtrace` crate, abort |
| Executor | Task queue, wakers, timers, `Task.*` primitives, held in a per-thread context (§12.1) |
| I/O | `mio` reactor; TCP sockets; synchronous files, directories and environment; stdin, stdout and stderr with buffering |
| Text and numbers | UTF-8 validation, float formatting (shortest round-trip, as `ryu` does) and parsing, integer parsing, case mapping for ASCII and the Unicode simple case folding table. The byte scans (UTF-8 validation, `IndexOf`, equality, JSON whitespace and quote scanning, hash-table group probes) use SIMD instructions, and `Std` calls them for inputs of about 32 bytes or more (D-83) |
| Hashing | A keyed fast hash with per-process random keys for `Dictionary` and `HashSet`, and SipHash-1-3 as `Hasher.Strong` (§15.3) |
| HTTP | Request-head parsing with `httparse` (§16.1) |

The standard library is written in Mora (§15) and calls the runtime through `extern` functions marked `@Import(Lib = "mora_rt", Name = "...")`. These imports are private to `Std`; user code cannot name them. Reference-count increments and decrements are not calls: the compiler emits them inline (§13.3) and calls the runtime only on the slow path.

**Allocator.** On every hosted target, `mora-rt` links mimalloc statically, and Mora code allocates only through it; `malloc` stays for C code (D-68). The allocator behaves the same on Linux, macOS and Windows, which the platform allocators do not. The runtime's allocation entry points are `mora_rt_alloc(size, align)`, which counts the allocation and calls `mora_pal_alloc`; `mora_rt_free_sized(ptr, size, align)`, which calls `mora_pal_free`; and `mora_rt_good_size(size, align)`, which calls `mora_pal_good_size` and returns the usable size of the block that request would get. The compiler knows the size of every object it frees, so it always frees with the size and alignment. Every object's size is a function of its type and the length or capacity it stores, so a free recomputes exactly the size that was allocated (§13.3, §13.4). A growable buffer (a `List<T>` buffer, a hash-table block, a heap string) asks `mora_rt_good_size` for the usable size of what it needs, takes the largest capacity whose size fits in it (a buffer of zero-sized elements is never allocated, §13.4), and allocates exactly the size of that capacity, so the size passed to `mora_rt_free_sized` is always the size passed to `mora_rt_alloc` and the buffer never uses bytes it did not ask for. Both runtimes keep a per-thread allocation counter and a byte counter: one increment per call to `mora_rt_alloc`, and the `size` it was passed, without the debug prefix, so both runtimes count the same bytes. `Std.Testing` and `mora bench` read them (§15.9). mimalloc is the hosted platform layer's implementation of these `mora_pal_` functions (§19.4), so a bare-metal target supplies its own.

### 13.2 Two builds of the runtime

The toolchain ships `libmora_rt.a` (release) and `libmora_rt_debug.a` (debug: poisoning, leak tracking, refcount underflow traps, extra assertions) per target. Both are optimized builds; the debug runtime is the same crates with their `debug` feature on. `cargo xtask runtime [--test] [target...]` builds both into `target/runtime/<target>/`, and with `--test` links a C program against each and checks that the process exits with the code its `__mora_main` returns. CI builds and tests both for every hosted target of §19.1, each on a runner of its own architecture except `x86_64-macos`, which runs under Rosetta 2 on the arm64 macOS runner. `mora build` links the debug runtime and `mora build --release` the release runtime, unless the profile overrides it (§17.2).

### 13.3 Shared objects

Every shared class object, array, heap string, box and escaping closure environment starts with an 8-byte header (D-72):

| Offset | Field | Meaning |
|---|---|---|
| 0 | `strong: u32` | Strong count. Bit 31 set means **immortal**: never retained, released or freed |
| 4 | `weak: u32` | Weak count, plus one while `strong > 0` |

There is no type pointer. After monomorphization every retain, release and drop site knows the concrete type, and a dynamic interface value carries its drop function, size and alignment in its vtable (§13.6). Each concrete type gets a generated drop function, called directly, so it can be inlined. The debug runtime keeps a `meta` word in a prefix before the header, so field offsets are the same in both builds. It points to the type's `TypeMeta` record (§13.6), which the leak report groups by and the cycle finder walks (§10.11). The identity number of D-61 lives in a side table keyed by the object, so it needs no header space.

- **Retain** is `if strong & IMMORTAL == 0 { strong += 1 }`, emitted inline. A count that would reach 2^31 aborts with `reference count overflow`.
- **Release** is `if strong & IMMORTAL == 0 { strong -= 1; if strong == 0 { mora_rt_drop(obj, T_drop, size, align) } }`. The release site knows the concrete type, so it passes that type's generated drop function, the allocation size and the alignment; for an array it comes from the length, and for a heap string from the capacity stored in the object, never from the length. Releasing a dynamic interface value takes the drop function, size and alignment from its vtable (§13.6).
- `mora_rt_drop(obj, drop, size, align)` calls `drop`: the destructor, then the fields. Each shared child whose count falls to zero is pushed onto a thread-local work list as an `(object, drop function, size, alignment)` entry instead of being dropped recursively. It then decrements `weak` and, if `weak` reaches zero, frees the memory with `mora_rt_free_sized(obj, size, align)`, which passes both to the platform's `free` (§19.4). `size` and `align` describe the object from its header on; in the debug runtime, `mora_rt_alloc(size, align)` allocates a block of `p + size`, where the prefix `p` is the `meta` word rounded up to `max(align, word size)`, aligned to `max(align, word size)`, and returns the address `p` bytes in. `mora_rt_free_sized(obj, size, align)` computes the same `p`, frees from `obj - p` with the same total size and alignment, and so callers pass the same values in both builds.
- **Weak load** returns a null pointer (`none`) if `strong == 0`, otherwise retains and returns the object.
- **Weak retain** increments `weak`; a weak count that would reach 2^32 − 1 aborts with `reference count overflow`, as a strong count does, so `weak` never wraps to zero while weak references remain. **Weak release** decrements it and, if it reaches zero, calls `mora_rt_free_sized(obj, size, align)`. Only shared class objects can have weak references (§10.7), never arrays, strings, boxes or interface values, and `T` in `weak T` is a concrete class, so the site knows its size and alignment statically. The implicit `+1` that `weak` holds while `strong > 0` is dropped by `mora_rt_drop`, so `weak == 1` on a live object means no weak reference exists (§14.18, §14.19).
- An **immortal** object (bit 31 of `strong` set) is never retained, released or freed, and its `weak` field is never changed: a weak reference to it is a plain pointer, and a weak load always succeeds.
- Immortal objects in read-only data are: string literals longer than 15 bytes, constant strings longer than 15 bytes and the heap strings nested in constants (§4.7), and the empty array. Each is laid out like any other object of its type behind an immortal header. Shorter string literals and constant strings are inline values (§13.4), and every heap string the program creates at run time is counted like any other object.
- In `shared` classes (post-MVP), `strong` and `weak` are updated with atomic operations: relaxed increments, release decrements and an acquire fence before the drop. A weak load increments `strong` with a compare-and-swap loop that succeeds only while the count is nonzero, with acquire ordering, so it never revives an object that a concurrent release is dropping.
- The compiler emits retain and release from one lowering that has both forms, and `mora_rt_drop` and the weak-count paths exist in both forms. The MVP runtime test suite covers the atomic form, including a weak load racing the last release, though only the plain form is emitted (D-60).

**Unique objects** have no header: a unique class reference points straight at its fields, because its type is always known statically and nothing counts it. When its owner drops it, the compiler calls its drop function and then `mora_rt_free_sized` with the type's static size and alignment, unless it was promoted (§14.18). The boxed future `Future<T>` is one unique block: a pointer to a read-only table with the future type's `Poll` and drop functions and the state's size and alignment, then padding, then the future's state inline. The state starts at `round_up(word size, state align)`; the block's size is that offset plus the state's size, and its alignment is `max(word size, state align)`. Dropping it drops the state through the table and frees the block with the size and alignment computed from the table (§12.2).

These are all the paths that free memory: a strong release that brings both counts to zero, a weak release that brings `weak` to zero, and the drop of a unique object, a `Future<T>` included, and the release of a task block (§12.3). Each passes the size and alignment the object was allocated with. Immortal objects and promoted objects never reach any of them.

### 13.4 Strings and arrays

- `string`: 16 bytes (D-75). An **inline** string holds up to 15 bytes, with its length and a tag in the last byte. A **heap** string holds a pointer to `{ header, capacity: u32, bytes[capacity] }` and its length; the object is allocated at exactly `8 + 4 + capacity` bytes and freed with that size. `capacity` can exceed the length after an in-place append (§14.19), and it never exceeds `int.MaxValue`: a capacity that would is clamped to it, and a string that would grow past `int.MaxValue` bytes panics with `string too long`. Copying a heap string retains it and copying an inline one does not; equality compares lengths first. `string?` uses a spare tag value for `none`, so it is also 16 bytes. A literal of at most 15 bytes is an inline value; a longer one is an immortal heap object in read-only data, so retaining or releasing either does nothing. The bytes are valid UTF-8 and never change while another reference can see them. Maximum length is `int.MaxValue` bytes. `AsSpan()` on an inline string points into the 16-byte value, so the span borrows the place that holds it (§10.6). `AsMemory()` on an inline string first moves its bytes to a heap string, one allocation, because a `Memory<T>` may outlive the place that holds the string.
- Concatenation allocates at most once: `a + b + c` lowers to one `Concat` that computes the total length first, and `$"..."` formats into a stack buffer and allocates the result once at the end; allocations inside a hole's own `Format` or `ToString` come on top. A result of at most 15 bytes allocates nothing.
- `T[]`: one pointer to `{ header, length: u32, padding to align(T), elements[length] }`. When `sizeof(T)` is zero, as for `()` or an empty struct, the elements take no space and the array is the header and length alone. An array is allocated at exactly that size, `8 + 4 + padding + length × sizeof(T)` rounded up to the alignment, and freed with the same size computed from `length`, which never changes.
- `List<T>`: a shared class holding `{ items, capacity: int, count: int, borrows: int, version: int }`, where `items` points to a raw buffer of `capacity` slots with no header. Only the first `count` slots hold values; the rest are uninitialized, so element types need no default value. The buffer is allocated at exactly `capacity × sizeof(T)` with the alignment of `T` and freed with that size. Dropping the list drops the first `count` elements and then frees the buffer. When `sizeof(T)` is zero, the list never allocates a buffer: `items` is a non-null pointer aligned to `T`, `capacity` is `int.MaxValue`, and only `count` changes. Hash tables follow the same rule for their entries: zero-sized keys or values take no space in the block, which still holds the index table.

### 13.5 Value layouts

- **Structs and tuples** order fields by decreasing alignment, then by declaration order, unless the struct has `@Layout(C)`, which keeps declaration order with C alignment rules. Size is rounded up to the alignment.
- **Unions** are a tag (`u8`, or `u16` beyond 256 cases) followed by the largest case payload. A union whose other cases carry no data puts its tag in a niche of the largest case's payload when that payload has a spare bit pattern for each of those cases; otherwise it keeps a separate tag.
- **Niches** (D-78). These types have invalid bit patterns that a `T?` or a union tag uses, so `sizeof(T?) == sizeof(T)`: pointers (shared and unique classes, heap strings, arrays, escaping closures; null is `None`); dynamic interface values (a null data pointer); `string` (a spare tag value); `bool` (254 spare values); `char` (surrogates and values above U+10FFFF); enums without `@Flags`, when the backing type has a value no member declares (an enum that declares all 256 values of a `byte` has no niche, so its option is a tag byte plus the enum); unions with a spare tag value; and an option of any of these when its payload still has an unused pattern after `None` takes one. `bool??` keeps a niche; `User??`, whose payload's only spare pattern (null) is taken by the inner `None`, uses a tag. Several `bool` fields are never packed into one byte, because each field must stay addressable for `ref` borrows.
- **`indirect` cases** store a pointer to an immortal-capable shared box holding the payload.
- **Interface values** are two words: data pointer, vtable pointer.
- **Closure structs** (§10.9) are laid out like structs, with one field per capture; calling one is a direct call.
- **Function values**: escaping closures are one pointer to an environment object whose first field after the header is the code pointer. A function value passed to an `in` parameter is two words, code pointer and environment pointer, which lets the same parameter accept non-escaping closures (environment on the caller's stack), escaping closures (environment is the heap object, not retained) and plain functions (null environment).

### 13.6 Vtables and type ids

For each pair of concrete type and interface used as a dynamic value, the compiler emits one read-only vtable: `[type meta pointer, drop function, allocation size and alignment, method pointers in interface declaration order, default methods included]`. The **type id** in the type meta is the first 128 bits of BLAKE3 over the canonical name of the concrete type, including type arguments and the root namespace of the library that declares it. `is C c` compares type ids.

**`TypeMeta`** is a read-only record the compiler emits once per concrete type: `{ size: usize, align: usize, drop: fn, type_id: u128, name: (ptr, len), counted_fields: (ptr, len), trace: fn }`, where `name` is the canonical type name in UTF-8, `counted_fields` lists the offsets of fields that hold counted references, and `trace` visits every counted reference the object holds directly: its counted fields, the elements of an array (using its length), and the live slots of a `List<T>` buffer or hash-table block (using their count), including counted references inside struct elements. Release builds emit it only for types used as dynamic values, and only vtables point to it. Debug builds emit it for every heap-allocated type too, and the `meta` prefix word of each object points to it (§13.3), so the leak report can group live objects by type name and the cycle finder can follow every counted reference through `trace` (§10.11).

### 13.7 Calling convention and symbols

- Mora functions use the target's C calling convention, which its target description names (§19.4): System V AMD64 on x86_64 Linux and macOS, AAPCS64 on aarch64 Linux, Apple's arm64 variant of AAPCS64 on aarch64 macOS, Microsoft x64 on x86_64 Windows, Microsoft's arm64 convention on aarch64 Windows, and LP64D on riscv64 Linux. Cranelift selects the matching `CallConv` from the target description (Windows arm64 support in Cranelift is [verify]). Debuggers and profilers therefore work without special support. Values larger than two words are passed by pointer; larger results are returned through a caller-provided pointer.
- Symbol names are mangled as `_M` + length-prefixed path segments + a hash of the generic arguments: `_M7UsersApi9UserStore3AddE`. `mora demangle` reverses it, and the backtrace printer demangles automatically.
- Cranelift emits DWARF line tables and unwind information on Linux and macOS, and CodeView/PDB data is post-MVP on Windows, where backtraces fall back to symbol names from the export table [verify].

### 13.8 Startup and process exit

The runtime's entry point (`main`, called by the C runtime start files) installs handlers, runs `Main` on the executor if it is async or directly if not, and returns the exit code. `Environment.Exit(code)` flushes stdout and stderr and exits without running destructors.

### 13.9 Foreign functions

```mora
@Import(Lib = "c", Name = "getpid")
extern GetPid(): int;
```

An `extern` function has `;` instead of a body and MUST carry `@Import` (`MR0451`). In the MVP, an `extern` function's parameter and return types MUST be plain types, `nint`, or `ReadOnlySpan<byte>` / `Span<byte>` (passed as pointer plus `nint` length) (`MR0450`). The import is resolved at link time against the named library. Calling an imported function is the only way a Mora program can break the guarantees of §10.14, and post-MVP packages that use `@Import` require the `ffi` capability (§18.6). `@Export` makes a Mora function callable from C under a fixed name.

### 13.10 Stacks

The main thread uses the OS default stack size (8 MiB on Linux and macOS, 1 MiB on Windows, raised to 8 MiB by the linker flags the toolchain passes). Tasks are stackless state machines and do not have their own stacks. The runtime detects stack overflow through the guard page with an alternate signal stack on Unix and a vectored exception handler on Windows, prints `stack overflow` and aborts.

### 13.11 Standard library internals

`Std` needs operations that safe Mora cannot express: partly initialized buffers for `List<T>`, moving elements in and out of raw storage, calling the runtime. Files in `std/` may use a restricted `unsafe` dialect that the compiler accepts only when compiling `Std` (`MR0107` elsewhere in the MVP, D-44):

- `unsafe { ... }` blocks;
- `RawPtr<T>` with `Read`, `Write` (moves a value in or out without dropping), `Offset`, `Add`, and `RawPtr<T>.Null`;
- `Uninit<T>`, a value slot the compiler does not drop, with `Write` and `AssumeInit`;
- intrinsics in `Std.Intrinsics`: `Alloc`, `Realloc`, `Free`, `SizeOf<T>`, `AlignOf<T>`, `IsPlain<T>`, `NeedsDrop<T>`, `DropInPlace<T>(RawPtr<T>)`, `Retain`, `Release`, `Abort`.

MIR represents these with a `RawDeref` place projection and intrinsic calls; every MIR check skips places based on `RawDeref`. The dialect is the seed of the post-MVP user `unsafe` feature (OQ-11) and is documented in `std/README.md`, not in user documentation.

## 14. Compiler architecture

### 14.1 Pipeline

One Rust workspace builds the `mora` executable. The compiler, LSP, formatter and REPL share one incremental analysis database (§14.3). Everything after type checking runs on one typed IR, MIR (§14.7), so the native backend, the JIT and the post-MVP wasm backend cannot drift apart.

```mermaid
flowchart LR
  src[".mr files"] --> lex[lex] --> parse[parse: lossless CST]
  parse --> items[item tree, name resolution]
  items --> sigs[signatures]
  sigs --> macros[macro expansion: JIT-run macros]
  macros --> infer[type inference and checking]
  infer --> hirchk[HIR checks: definite assignment, exhaustiveness, mutability, lints]
  hirchk --> lower[lower to generic MIR]
  lower --> mirchk[MIR checks: moves, drops, borrows, views, escapes]
  mirchk --> async[async lowering]
  async -.post-MVP.-> ser["mora.ir section"]
  async --> mono[monomorphization]
  mono --> inl[inlining, SROA, constant folding]
  inl --> arc[ARC insertion and optimization]
  arc --> place[stack promotion, reuse]
  place --> opt[CFG simplification, dead code]
  opt --> clif[Cranelift: object file or JIT]
  clif --> link[link with mora-rt]
  opt -.post-MVP.-> wasm[wasm backend]
```

The optimizer runs in that order because each stage needs the one before (D-74): inlining and scalar replacement turn iterator chains, accessors and closure structs into straight-line code; ARC optimization then sees the real data flow and removes the retains that inlining made redundant; stack promotion and reuse work on the counts that remain. Every budget in the optimizer is counted in MIR statements, never in time, so the same input always gives the same output.

### 14.2 Crates

| Crate | Responsibility | Main dependencies |
|---|---|---|
| `mora-span` | File ids, spans, line index, the database trait | `salsa` |
| `mora-diag` | One diagnostic type for every crate, the error code registry and its explanations (`docs/errors/MRnnnn.md`), terminal and JSON rendering | `annotate-snippets`, `serde`, `serde_json` |
| `mora-syntax` | Lexer, parser, lossless syntax tree, typed AST views | `logos`, `rowan`, `unicode-ident`, `unicode-normalization` |
| `mora-hir` | Item tree, name resolution, types, inference, HIR bodies; defines the macro-runner interface | `salsa`, `la-arena` |
| `mora-macro` | Compiles macros with the JIT, runs them in the sandbox, builds the `Std.Meta` model, parses `code` output, caches expansions | `cranelift-jit` through `mora-codegen` |
| `mora-check` | HIR checks and lints: definite assignment, exhaustiveness, mutability, naming, `strong-cycle` | `salsa` |
| `mora-mir` | MIR, lowering from HIR, move and drop elaboration, borrow and view checks, async lowering, monomorphization, ARC passes, optimizations, const evaluation | none beyond workspace crates |
| `mora-codegen` | Layouts from the target description, lowering MIR to Cranelift IR, object files, JIT, debug info | `cranelift-codegen`, `cranelift-frontend`, `cranelift-module`, `cranelift-object`, `cranelift-jit`, `gimli` |
| `mora-link` | Linker driver: finds the bundled or system linker, start files and runtime library for a target | `object` |
| `mora-target` | Target descriptions (§19.4): triple, pointer width, endianness, alignments, object format, linker flavor, platform capabilities, `@Cfg` values | `serde`, `toml` |
| `mora-rt-core` | Portable runtime core, `no_std` (§13.1) | none |
| `mora-rt` | Hosted platform layer for Linux, macOS and Windows (§13.1) | `mio`, `httparse`, `backtrace` |
| `mora-driver` | Projects, profiles, targets, the build graph, standard library loading | `toml` |
| `mora-pkg` | Manifest and lockfile; local path dependencies in the MVP, fetching post-MVP | `toml`, `sha2` |
| `mora-fmt` | Formatter on the syntax tree | |
| `mora-lsp` | Language server on the shared database | `lsp-server`, `lsp-types` |
| `mora-repl` | Interactive loop on the JIT | `reedline` |
| `mora-cli` | The `mora` executable | `clap` |
| `mora-testkit` | UI, run, spec-example and leak test harnesses (§20) | |

`std/` holds the standard library's Mora source. The toolchain embeds it and compiles it with the program (§14.15). `xtask` holds repository tooling: the crate layering check (`cargo xtask layers`, as in the predecessor project), the spec-example extractor, the error-code registry check, the syntax code generator (§14.4) and `cargo xtask bench-compare`, which runs the benchmarks on `main` and on a branch and reports the change of each (§20.6).

**Layers.** A crate may depend only on crates in lower layers: (1) `mora-span`, `mora-target`; (2) `mora-diag`, since every diagnostic points at a span; (3) `mora-syntax`; (4) `mora-hir`; (5) `mora-check`, `mora-mir`; (6) `mora-codegen`, `mora-fmt`; (7) `mora-macro`, `mora-link`, `mora-pkg`, `mora-driver`; (8) `mora-lsp`, `mora-repl`; (9) `mora-cli`, `mora-testkit`. The runtime crates sit below all of them: `mora-rt-core` depends on no other workspace crate, and `mora-rt` only on `mora-rt-core`. Any crate may take `mora-testkit` as a dev-dependency. `xtask` is outside the layers, and nothing may depend on it.

Crate versions are pinned in the workspace `Cargo.toml`. At the time of writing they are `salsa` 0.28, `rowan` 0.18 (an alpha release, so the parser keeps its use behind `mora-syntax`'s own API), `logos` 0.16, Cranelift 0.137, `lsp-server` 0.10, `mio` 1.2 and `annotate-snippets` 0.12 [verify each API before use].

### 14.3 The analysis database

Inputs are file contents, the project graph and the target configuration. Every derived fact is a `salsa` query: `parse(file)`, `item_tree(file)`, `def_map(project)`, `signature(item)`, `infer(body)`, `mir(body)`, `checked_mir(body)`, `diagnostics(file)`. The LSP keeps one database for the session and updates inputs as files change. The CLI builds a database, runs the queries the build needs and drops it.

### 14.4 Lexer and parser

- The lexer is generated by `logos` for ordinary tokens and switches to a hand-written sub-lexer inside string literals, because interpolated and raw strings need a mode stack (holes can contain strings that contain holes). An interpolated string is a sequence of tokens: its start, pieces of text, holes, and its end. A hole is its opening braces, the ordinary tokens of its expression and alignment, an optional format, and its closing braces, so the parser and the LSP see the code in a hole as code.
- `>` is always its own token. The parser joins adjacent `>` tokens into `>>`, `>>>`, `>=`, `>>=` and `>>>=` in expressions, so `List<List<int>>` closes two type argument lists without splitting a token.
- The nodes of the tree and their children are written down in `crates/mora-syntax/mora.ungram`, in the notation of `ungrammar`. `cargo xtask codegen` generates `SyntaxKind` from it and from the keyword lists of §3.4, together with a typed view per node (`ast::StructDecl` with `name()`, `member_list()` and so on), and CI fails when the generated code is stale.
- The parser is hand-written recursive descent with Pratt parsing for expressions. It produces a `rowan` green tree that keeps every token and every piece of trivia, so the formatter and the LSP work on the same tree and `print(parse(text)) == text` for every input, valid or not. The compiler reads the green tree in place: a view of a node is the green node and its offset, which borrow the tree and are copied freely, so a view costs no allocation and a walk over a subtree allocates only its stack. A view has no link to its parent, because the compiler only walks down from the root.
- The parser enforces the nesting limit of §3.1 in two places. While it descends it counts levels and, at the limit, stops: the construct there goes into one `Error` node, which holds a bracketed group when the construct starts with an opening bracket and otherwise the tokens up to a `;`, a `,` or a closing bracket that is not its own. The lookahead that decides between a type and an expression skips a type past the limit without descending, so the type is parsed, and reported, as a type. A chain grows to the left without the parser descending, so the tree builder also counts the levels of the finished tree and turns a node past the limit, with everything in it, into an `Error` node that holds its tokens. Every later pass can then walk the tree recursively without running out of stack. The builder creates each distinct token once and does not intern nodes, so building a tree takes time proportional to its size at any depth.
- The parser never stops at the first error. It recovers at statement and member boundaries and marks missing nodes, so the LSP can analyze code that is being typed. A token the grammar does not allow where it appears, or a missing one, is error `MR0110`, whose message names what was expected. Code written in another language's syntax gets this error too, with no dedicated code or fix (D-59).
- Four ambiguities are settled by fixed rules. A generic argument list `<...>` is chosen when the token after the closing `>` is one of `( ) ] } : ; , . ? == != | ^ && || & [`. A parenthesized sequence of tokens followed by an operand is a cast `(T)x` when the tokens form a type and either they cannot form an expression, as in `(int)-x` or `(int?)x`, or the token after `)` is `~`, `!`, `(`, an identifier other than `with`, a literal (including `true`, `false` and `none`), or a keyword other than `as` and `is`; otherwise it is a parenthesized expression, so `(a) - b` subtracts. The word `match` followed by `(` starts a `match` (§7.7) when the parentheses, balanced, are followed by `{`; otherwise it is an identifier, so `match(x);` is a call, since a call is never followed by `{`. Where a statement can start, with or without attributes before it, this rule also wins over a function named `match` with a block body and no return type, so such a function is written with one: `match(x: int): () { ... }`. The postfix `?` is settled by §7.4.
- A statement that starts with `let` or `var` declares a local (§7.10). Any other statement starts with a keyword or is an expression statement. No declaration starts with a type, so the parser never has to guess whether a statement such as `a < b > c;` declares a variable or compares values.
- **Parsing never needs types** (D-115). Every construct parses from its tokens alone: the rules above and in §7.4 settle every choice the grammar leaves open, and no parse depends on what a name refers to. A language change that would need name or type information to parse is not accepted.
- **The ungrammar is the normative grammar** (D-115). `mora.ungram`, with the keyword lists of §3.4 that code generation also reads, defines the syntax. The EBNF fragments in this document summarize it, and where they differ the ungrammar wins and the fragment is corrected. `mora docs --llm` and the documentation site publish it (§17.10). Besides the fuzz target of §20.1, a second target generates programs from the ungrammar, adding parentheses wherever the rules above would read the text another way, parses each one, and checks that no error is reported, that the tree has the shape the program was generated from, and that printing the tree gives the text back.

### 14.5 Name resolution and types

A simple name, or the first segment of a path, is looked up from where it is used, in this order: locals and parameters; members of the enclosing types; the enclosing namespaces, from the innermost out; the global names, which are the first segment of the project's root namespace, the names of its dependencies (§4.4) and `Std`; the names that the file's `using` directives and the project's `global using` directives open (§4.3); and last the prelude. Each later segment is looked up in what the segments before it name. A level counts only the declarations visible from the use (§4.5), and the first level that has one decides. Two declarations at that level are ambiguous, error `MR0128`, unless all of them are functions, which form a set of overloads; two types with one name in one namespace are therefore reported where the name is used. Within one type a name means one member or one set of overloads (`MR0146`), so the members of an enclosing type are never ambiguous. A name that no level has is error `MR0127`, and a name that only declarations invisible from the use have is error `MR0129`. In a type position the value members of the enclosing types are not looked up, so a field may take the name of its type, as in `public Address: Address;` (D-133). A path in a type position that names a namespace, a function, a macro or a value is also error `MR0127`. When a name is not found, the error suggests the nearest visible name, or type keyword in a type position, with a fix that puts it in place. An edit inserts, deletes or substitutes one character or swaps two adjacent ones; a name of `n` characters gets a suggestion only within `max(1, n / 3)` edits, rounding down, and of two at the same distance the first in name order wins. The path of a `using` directive is looked up without the last two levels, so no directive depends on another.

Type checking is bidirectional: types flow inward from targets (lambdas, `new()`, collection expressions, `none`, union case names, integer literals) and outward from expressions. Type arguments of a generic call are inferred as §8.1 describes. There is no whole-function type inference: `let` and `var` locals take the type of their initializer.

Macro expansion (§9) sits between signature resolution and body checking. `mora-hir` asks for expansions through a `MacroRunner` interface that `mora-macro` implements, so the HIR layer does not depend on code generation. Expanded members become ordinary HIR items, so later phases do not know they were generated.

### 14.6 Constant evaluation

`const` values, default parameter values, constants converted to a newtype and the defaults of refined newtypes (§6.12) are evaluated by a MIR interpreter at compile time. In the MVP the interpreter supports arithmetic, comparisons, string literals and concatenation of constants, construction of plain structs, tuples and unions, read-only sequences of such values for `ReadOnlySpan<T>` constants (a constant is never an array, §4.7), and matching a value against a newtype's refinement pattern. Anything else is error `MR0460` with the hint to use `static lazy`.

### 14.7 MIR

MIR is a typed control-flow graph:

```text
Body       { params, return_ty, locals: [LocalDecl { ty, mutable, source }], blocks: [Block], drop_flags }
Block      { statements: [Statement], terminator: Terminator }
Place      { local, projections: [Field(n) | Deref | Index(local) | ConstIndex(n) | Case(n) | RawDeref] }
Operand    = Copy(Place) | Move(Place) | Const(Value)
Rvalue     = Use(Operand) | Borrow(Mode, Place) | Binary(Op, Operand, Operand) | Unary(Op, Operand)
           | Aggregate(Kind, [Operand]) | Cast(Kind, Operand, Type) | Len(Place) | Discriminant(Place)
           | MakeClosure(Fn, [Operand]) | ToInterface(Operand, Interface)
Statement  = Assign(Place, Rvalue) | StorageLive(local) | StorageDead(local) | SetDropFlag(local, bool)
           | Retain(Place) | Release(Place) | Snapshot(dest, Place) | WriteBack(Place, src) | Nop
Terminator = Goto(b) | Switch(Operand, [(value, b)], otherwise) | Return | Call { callee, args, dest, next }
           | Drop(Place, next) | Await(Operand, dest, resume) | Panic(PanicKind, args) | Unreachable
```

Before monomorphization, types may contain type parameters. `Retain` and `Release` appear only after ARC insertion (§14.11); `Await` disappears in async lowering (§14.9). Every MIR statement carries a span for diagnostics and debug line tables. A textual MIR dump (`mora build --emit mir`) is stable enough to be used in snapshot tests.

**Numbering.** `Field(n)` counts the instance fields of a struct by declaration order, from zero: a `static` field and a constant belong to the type, not to its values, and are not counted. An aggregate of a struct takes one operand per instance field, in that order.

**Arithmetic.** `Add`, `Sub`, `Mul` and unary `-` are checked by meaning: a result that does not fit its type panics. The wrapping forms `+%`, `-%` and `*%` wrap. `Div` and `Rem` include the panic for division by zero. Code generation applies the build profile (§17.2), so MIR does not depend on it.

**Conversions.** Every conversion that inference applies is a `Cast` of kind `Implicit`. A written cast, and the cast back that a compound assignment applies to its result (§7.12), is a `Cast` of kind `Numeric`.

**Parameter modes.** An `in` or `ref` argument, and `this`, is passed as a borrow: a `Borrow` rvalue stores it in a temporary and the call takes that temporary. A parameter of those modes holds a borrow, and its value is reached through `Deref`. A local function's captured locals are passed as `in` and `ref` parameters after its own parameters. A captured `this` comes before them all, and is `ref` when the local function writes through it and `in` otherwise. A method's receiver is the first argument. Its expression is evaluated before the other arguments; a receiver taken by `in` or `ref` is borrowed after them and after the default values, as a captured `this` is, and a `move` receiver is copied into a temporary before them when a later argument may change it (D-146).

**Callees.** A `Call` targets an `Item`, a function or method of the program; a `Local(n)`, a local function, by the position `n` of its body in its owner's MIR, which holds the owner's body first and then those of its local functions in the order of the text; an `Initializer(T, n)`, the initializer of the `n`-th instance field of the struct `T`, a body of its own that takes no argument and gives the field's value, called for a field that `new T { ... }` leaves out; or, as an M1 stand-in, an `External` function. **Call results.** The arguments are evaluated before the call, and the result is stored in `dest` after the callee returns. When `dest` overlaps a place that an argument borrows, as in `p = p.Scaled(2);` where the receiver borrows `p`, the callee reads the old value throughout, and code generation stores the result through a temporary.

**Diverging calls.** A `Call` with no `next` does not return: it calls a function that never returns (D-138). The block after the call is unreachable, so lowering emits none for it.

**M1 stand-ins.** Until the standard library is loaded (M1.15), a call whose target is a `Std` name the compiler cannot see has an `External` callee naming its path and no result, and an interpolated string is an `Interpolate` rvalue of text pieces and operands. The validator rejects both once `Std` is loaded.

**Validation.** A validator checks that every local has a concrete type, with no part that is an error or a type MIR does not hold yet, that every place and operand is typed and consistent and that every block ends in one terminator. A local of the never type `!` is allowed: `let x = Todo();` declares one, and nothing is ever stored in it. Debug builds of the compiler run it after lowering, and after every later pass.

### 14.8 MIR checks

Run on generic MIR, before async lowering, in this order:

1. **Move check.** Forward dataflow of maybe-initialized and maybe-moved state per local and field path (§10.5).
2. **Drop elaboration.** Inserts `Drop` terminators at scope exits, adds drop flags for maybe-moved locals, and removes drops of moved-from places.
3. **Borrow check.** For each call, classifies every borrowed argument place as exclusive or aliased (§10.4), reports conflicts (`MR0320`), and rewrites aliased non-plain borrows into `Snapshot` and `WriteBack`. Inserts root keep-alive copies where the root is not provably held by an exclusive place.
4. **View check.** Liveness of view locals; reports changes to a view's source while the view is live (`MR0341`), views escaping their scope (`MR0340`, `MR0342`, `MR0343`). Inserts the collection borrow guards of §10.10.
5. **Escape check.** Reports storing a borrowed parameter of a move-only type (`MR0310`) and captures that §10.9 forbids.
6. **Effect check.** Computes each function's effect set and getter facts per strongly connected component of the call graph (§6.14, §6.9), then checks `uses` bounds (`MR0250`), getters (`MR0251`) and `stored` getters (`MR0252`), and runs `blocking-in-async`. Calls that a generic function makes through its type parameters are resolved per instantiation, so a bound on generic code is checked for every instantiation the program uses. Results are cached per function in the analysis database (§14.3).

Every diagnostic from these checks is written for the audience of §1.4: it names the variables and lines involved, says in one sentence what would go wrong, and offers at least one fix that the LSP can apply.

### 14.9 Async lowering

Each async function or lambda becomes a state struct and a `Poll(ref state: State, ref waker: Waker): Poll<T>` function. The pass computes which locals are live across each `Await` (liveness analysis) and stores those in the state, numbers the suspension points, and emits a drop function that drops the right fields for each suspension point. Because §12.3 forbids borrows and views across an `await`, the state never contains a pointer into itself.

### 14.10 Monomorphization

Starting from the roots (the entry point, `@Test` functions, `@Export` functions and, for libraries, every public non-generic function), the pass walks MIR call graphs and instantiates each generic body once per distinct list of type arguments. Interface vtables for dynamic values are instantiated as they are needed. The output is a set of MIR bodies with no type parameters.

### 14.11 ARC insertion

After monomorphization every type is concrete, so the pass knows which places hold counted references. It inserts `Retain` for each `Copy` of a counted value and `Release` for each drop of one, using the conventions of §10.3: `in` arguments at +0 (borrowed), `move` arguments and return values at +1 (owned).

### 14.12 ARC optimization

1. **Last-use moves.** A `Copy` of a local that is dead afterwards becomes a `Move`.
2. **Pair cancellation.** A `Retain` and a later `Release` of the same reference cancel when nothing between them can release that reference or observe its count. A call counts as a possible release unless its summaries (below) show that it cannot release an object of that type.
3. **Borrowed forwarding.** An owned value that is only borrowed until it is dropped is never retained.
4. **Immortals.** Retains and releases of immortal constants are removed.

5. **Snapshot and keep-alive elision.** A snapshot of a `mut` field passed to a callee that cannot write that field is removed, and so is a root keep-alive retain when the callee's summaries show that it neither releases an object of the root's type nor observes that object's count, which a weak load of it would.
6. **Release at last use.** A value whose drop runs no user code is released right after its last use (§10.8).

Two summaries per monomorphized function feed these passes, computed as incremental queries with a fixed point over each cycle of the call graph (D-70):

- **Ownership:** for each parameter, whether the function only borrows it, may retain a copy, or consumes it.
- **Modification and release:** which fields, by type and field, the function may write, and which types it may release, directly or through drops.

The standard library's `unsafe` functions carry hand-written summaries, which the debug runtime's tests check. These passes MUST deliver the guarantees of §10.13; snapshot tests of MIR after this phase check each one. Reuse of a unique value's storage is limited in the MVP to `with` and string `+=` (§14.19); general reuse is post-MVP (§25).

### 14.13 Cranelift lowering

Code generation sits behind a `Backend` trait in `mora-codegen` that takes monomorphized, ARC-optimized MIR and a target description and produces code in the target's object format (§19.4): an ELF, Mach-O or COFF object file, or a WebAssembly module for the wasm backend. Running code in-process is a separate, optional capability (below). Cranelift is the MVP's only implementation; the wasm backend (§18.1) and the LLVM backend (M13) implement the same trait, so nothing before code generation changes when a backend is added.

Two rules keep a second backend from repeating or contradicting the first:

- **Layout is computed once.** Sizes, alignments, field offsets and union tags (§13.3 to §13.5) come from backend-independent code in `mora-codegen` that reads the target description, and every backend uses that result. No backend computes a layout of its own, so two backends cannot disagree about where a field lives.
- **The JIT is optional.** The `Backend` trait requires that output only, and in-process execution is a separate capability. The REPL, the test harness and macros run on the host, which is x86_64 or aarch64, so they use Cranelift's JIT. Cranelift supports both instruction sets; whether it supports Microsoft's arm64 calling convention is the open question of §13.7, and an aarch64 Windows host depends on its answer. A backend added for other targets, such as LLVM for 32-bit embedded chips (M13), only has to produce that output.

One Cranelift function per MIR body. Scalar locals whose address is never taken become SSA variables through `cranelift-frontend`; aggregates and address-taken locals live in stack slots. Overflow-checked arithmetic uses Cranelift's overflow-flag instructions and branches to a cold panic call. `Switch` lowers to Cranelift's jump tables or branch trees. Panics are calls to `noreturn` runtime functions. Line tables are emitted from MIR spans through `gimli`. Debug builds use Cranelift's `none` optimization level and release builds `speed`.

### 14.14 Linking

`mora-link` turns the object file into an executable:

| Host and target | Linker | C runtime | Result |
|---|---|---|---|
| Linux (x86_64, aarch64) | `ld.lld` shipped in the toolchain | musl start files and `libc.a` shipped in the toolchain | Fully static executable |
| macOS (aarch64, x86_64) | The system `ld` through `xcrun` (Xcode Command Line Tools) | `libSystem` (dynamic, the only supported way on macOS) | Executable depending only on `libSystem` |
| Windows (x86_64, aarch64) | `lld-link` shipped in the toolchain | MinGW-w64 start files and import libraries from llvm-mingw, shipped in the toolchain; runtime built for the `gnullvm` targets | Executable depending only on system DLLs |

On Linux and Windows the linkers merge identical functions, so monomorphized copies that compile to the same code are stored once (D-82): `ld.lld` runs with `--icf=all` and `lld-link` with `/opt:icf`. macOS links with the system `ld`, which gets no flag; folding there is post-MVP [verify whether the system `ld` folds identical code, or whether to ship `ld64.lld`]. Folding is not observable from Mora: function types have no equality (§5.8), so `==` on two function values is error `MR0233`, and Mora code cannot take a function's address. A function's address leaves Mora only through `@Export`, since an `extern` function cannot take a function in the MVP (§13.9), so exported functions keep their own addresses: on ELF the compiler passes `--keep-unique=<symbol>` for each exported symbol, and on COFF it leaves exported functions out of folding [verify the `lld-link` mechanism]. A backtrace may then name a merged twin, `List<User>.Add` for `List<Order>.Add`. Merging identical MIR bodies before code generation is post-MVP.

No step requires Visual Studio or a C compiler on the user's machine except the Xcode Command Line Tools on macOS. When a required tool is missing, `mora build` stops with the exact command that installs it (`xcode-select --install`). Each row is [verify] until the cross-platform CI job of milestone M1 passes.

### 14.15 Standard library compilation

The MVP compiles the standard library from source together with the program, and the build caches the checked, generic MIR of `Std` on disk under `target/mora/cache/std-<toolchain hash>`. A program only pays for monomorphizing and generating code for what it uses. If hello world takes longer than the budget in §20.6 to build, a per-project on-disk cache of user code moves into the MVP.

### 14.16 JIT and the REPL

`mora repl` and the test harness use `cranelift-jit` on the same MIR. The REPL keeps a session project: each input is parsed as declarations or as top-level statements; `let` and `var` locals from earlier inputs become session globals; each input is compiled and run immediately. `:type expr`, `:mir fn`, `:load file.mr` and `:reset` are built in.

### 14.17 Inlining, scalar replacement and bounds checks

Cranelift optimizes one function at a time, so the inlining that sequence operator chains and closure structs need happens in MIR (D-74):

- **Inlining policy.** Always inline `@Inline` functions, closure-struct calls whose closure has one call site, the `Next()` of standard iterator types, trivial accessors, and bodies under a small budget of MIR statements. Never inline `@Inline(Never)` functions, members of a recursive cycle, or cold paths such as panic branches. Debug builds inline only the cases that affect allocation or reference counts (closure structs, the `Next()` of standard iterator types, and accessors), so both profiles allocate the same and the sequence operator guarantee of §10.13 holds in both.
- **Scalar replacement** turns iterator structs and other small aggregates into separate locals that Cranelift keeps in registers.
- **Bounds checks.** `foreach` over an array or span checks no index, because the enumerator knows the length. `for (var i = 0; i < a.Length; i++)` over an array or span local or parameter that the loop does not assign checks no index either, because arrays and spans never change length. The same loop over a `List<T>` keeps its check unless the loop body's summaries show that it changes no `List<T>`, and the enumerator's version check (§10.10) is hoisted out of the loop under the same condition; both are best-effort.

### 14.18 Stack promotion

For each parameter of a counted type, and for `this`, a summary says whether the reference can **escape**: be stored in a field, array element, collection, static, escaping closure or spawned future; be returned; be converted to `any I`, captured by `[weak x]` or stored in a `weak T` (an object that can have a weak reference is never promoted); or be passed to a parameter that escapes, or through `any I` or a function value. The summary is computed over the whole program, with a fixed point over each cycle of the call graph that starts from "does not escape", so it does not depend on inlining (D-69).

A `new` that meets the rule of §10.13 is placed in the creating function's frame, or in its state if it is async. A promoted shared object keeps its header, so any callee can retain and release it, but its count is back to one when its last use passes. The compiler drops it where a heap object would be released: at its last use if its drop runs no user code (§10.8, D-70), otherwise at the end of its scope, and no second drop runs at the end of the scope. That drop calls the type's drop function directly (destructor, then fields) and never `mora_rt_drop` or `mora_rt_free_sized`: the object and its header are part of the frame or state and go away with it. The debug runtime checks `strong == 1 && weak == 1` (no weak reference besides the implicit one of §13.3) right before that drop and aborts with `internal error: stack object escaped (type T)` otherwise. A unique object is promoted when it is never moved out of the local that holds it: it has no header (§13.3) and no weak references (§10.7), so it is a plain value in the frame or state, dropped at the point §10.8 gives. A promoted `List<T>` also gets its first buffer, up to 256 bytes, in the frame, and moves it to the heap when it grows past that; this part is best-effort.

Hover shows a parameter that does not escape as `scoped`. The word is not a keyword in the MVP (OQ-25). Because the summary looks into callees, editing a callee can change where a caller's object lives; `mora explain-cost --diff` reports the change.

### 14.19 Reuse when unique

Two idioms reuse a value's storage when nothing else refers to it (D-71):

- **`with`.** `x with { F = v }`, where `x` is at its last use, checks `strong == 1 && weak == 1` at run time: one strong reference and no weak reference besides the implicit one of §13.3. When it holds, it releases the old `F`, stores `v` and returns `x`; otherwise it copies and then sets. Only a struct can be the subject: a class has no `with` (§6.3). The counted case is a struct the compiler stores in a heap cell, because it is large or recursive (§6.1, §10.13). A struct stored inline has nothing to reuse, since `with` on it is already a copy that scalar replacement removes.
- **String append.** `s += t` and `s = s + t`, where `s` is a local at its last use, build the result in place. A result of at most 15 bytes stays an inline string and allocates nothing. A longer result is appended in place when `s` is a unique heap string (`strong == 1`, which an immortal string, with bit 31 set, never is) whose allocation has room (§13.1) and no `in` borrow or view of `s` is live, which the compiler checks statically (§10.6). Otherwise the string moves to a new allocation that grows by half again, so repeated appends run in amortized linear time. No other reference sees the change, so strings stay immutable to every observer.

Reuse needs `weak == 1`, that is no weak reference besides the implicit one, because a weak reference could otherwise load the new value instead of `none`. Rebuilding an `indirect` union in place and reusing any freed cell for a new allocation of the same size are post-MVP.

## 15. Standard library (MVP surface)

The standard library gives familiar names to familiar things (`List<T>`, `Dictionary<TKey, TValue>`, `Console.WriteLine`), and leaves out what depends on reflection, exceptions or a collector. The API reference generated from `std/` doc comments is the full contract; a name listed here and missing there is a bug in one of the two.

### 15.1 Prelude (`Std`)

Visible in every file: primitives and their static members (`int.Parse`, `int.MaxValue`, `double.IsNaN`, ...), `string`, `Option<T>`, `Result<T, E>`, `Ok`, `Err`, `List<T>`, `Dictionary<TKey, TValue>`, `HashSet<T>`, `Span<T>`, `ReadOnlySpan<T>`, `Func`, `Action`, `Console`, `Math`, `MathF`, `Environment`, `Task`, `Future<T>`, `TimeSpan`, `Clock`, `Instant`, `MonoTime`, `Drop`, `ReferenceEquals`, `panic`, `Debug`, `Unreachable`, `Todo`, the interfaces of §8.5, `IError`, `Report`, `Literal`, `Html`, `Url`, `Log`, `Trace`, `Secret<T>`, and the sequence operators of §15.5.

Numeric parsing returns results: `int.Parse(s)` is `Result<int, ParseError>`, so `if (int.Parse(text) is Ok(let n)) { ... }` parses and tests in one step.

### 15.2 Strings and text

| Type | MVP members |
|---|---|
| `string` | `Length`, `IsEmpty`, `Chars()`, `AsBytes()`, `AsSpan()`, `Contains`, `StartsWith`, `EndsWith`, `IndexOf` (byte offset, `int?`), `Substring(start, length)` (byte offsets), `Split(char)`, `Split(string)`, `Trim`, `TrimStart`, `TrimEnd`, `ToUpper`, `ToLower` (ASCII and Unicode simple mappings), `Replace`, `PadLeft`, `PadRight`, `Repeat`, `Equals(other, StringComparison)`, `CompareTo`, static `Join`, `Concat`, `IsNullOrEmpty` (on `string?`), `FromUtf8(ReadOnlySpan<byte>)` returning `Result<string, Utf8Error>` |
| `StringBuilder` | `Append(...)` for every `IDisplay`, `AppendLine`, `Length`, `Clear`, `ToString()` |
| `Formatter` | The sink `IDisplay.Format` writes to: `Write(ReadOnlySpan<byte>)`, `Write(char)`, `Write(value: IDisplay)`, padding and alignment helpers |
| `char` | `IsDigit`, `IsLetter`, `IsWhiteSpace`, `IsUpper`, `IsLower`, `ToUpper`, `ToLower`, `ToString()`, `Utf8Length` |
| `Literal` | Text the compiler knows (§7.14): `Length`, `AsSpan()`, `ToString()`, `IDisplay`. Only the compiler creates one |
| `Html` | An interpolation handler (§7.14). Literal parts are trusted markup. A hole is escaped for where it stands, element text or a quoted attribute value, and a hole anywhere else (inside `<script>` or `<style>`, in an unquoted attribute, or in a tag or attribute name) panics with the context it found. Some quoted attributes need more than escaping: in an attribute that takes a URL (`href`, `src`, `action`, `formaction` and the others the HTML standard lists), a hole is allowed only after literal text that starts with `http://`, `https://` or `mailto:` (in any case) or with `/` not followed by another `/`, `./`, `../`, `?` or `#` (`href="/users/{id}"`), and is then percent-encoded as `Url` encodes a hole; a `Url` hole may also be the whole value, since its own scheme is literal text too (`Url` implements `IHole<Html>` for this); a hole in an event handler attribute (`on...`), in `style` or in `srcdoc` panics. Holes: any `IDisplay` value, whose text is escaped, and `Html`, which implements `IHole<Html>` and is inserted as it is in element text; in an attribute value an `Html` hole is escaped like any text. `Html.Unchecked(text: string)`, `ToString()` |
| `Url` | An interpolation handler (§7.14). Literal parts are the URL's structure. A hole is percent-encoded for its component: a path segment, a query name or value, or the fragment. Every byte of its UTF-8 text except the unreserved characters of RFC 3986 (letters, digits, `-`, `.`, `_`, `~`) is encoded, so a hole can never add a `/`, `?`, `&`, `=` or `#`, and a hole that would make a whole path segment `.` or `..` has its dots encoded too. A hole before the end of the host, that is in the scheme or in the text up to the first `/`, `?` or `#` after `//`, panics. Holes: any `IDisplay` value. `Url.Unchecked(text: string)`, `ToString()` |

`Split` returns a lazy iterator struct, as the sequence operators do, so `foreach (let field in csv.Split(','))` allocates only the pieces longer than 15 bytes (§13.4); `ToList()` on it builds a list.

**Format specifiers** for interpolation holes and `ToString(spec)` on numbers: `D[n]`, `X[n]` / `x[n]`, `F[n]`, `N[n]` (thousands separators), `E[n]`, `G`, `P[n]`, and `R` for round-trip doubles. There is no culture support in the MVP: everything formats and parses with one culture-independent convention (OQ-14). The decimal separator is `.`, the thousands separator is `,` in groups of three digits, the negative sign is a leading `-`, `P` multiplies by 100 and appends ` %`, and the non-finite doubles are written `NaN`, `Infinity` and `-Infinity`.

### 15.3 Collections

| Type | MVP members |
|---|---|
| `List<T>` | `new()`, `new(capacity)`, `Count`, `Capacity`, `this[int]`, `Add`, `AddRange`, `Insert`, `RemoveAt`, `Remove`, `Clear`, `Contains`, `IndexOf`, `Sort()`, `Sort(compare: (T, T) => int)`, `Reverse`, `AsSpan`, `ToArray`, `ForEach`, `GetEnumerator` |
| `Dictionary<TKey, TValue>` | `Count`, `this[key]` (get returns `TValue?`; set inserts or replaces, and setting `none` removes the key), `Add` (returns `false` and changes nothing when the key is present), `Remove`, `ContainsKey`, `Keys`, `Values`, `Clear`, iteration as `(Key: TKey, Value: TValue)` tuples in insertion order. `Count` and the `this[key]` get are `@Cheap` (§6.11), so getters may use them |
| `HashSet<T>` | `Add` (returns `bool`), `Remove`, `Contains`, `Count`, `UnionWith`, `IntersectWith`, iteration. `Contains` and `Count` are `@Cheap` (§6.11) |
| `Queue<T>`, `Stack<T>` | `Enqueue`/`Dequeue` and `Push`/`Pop` returning `T?`, `Peek`, `Count` |
| `T[]` | `Length`, indexing, `AsSpan`, `Fill`, `Array.Sort`, `Array.IndexOf` |
| `Span<T>`, `ReadOnlySpan<T>` | `Length`, indexing, slicing with ranges, `CopyTo`, `SequenceEqual`, `IndexOf`, `ToArray` |
| `Memory<T>`, `ReadOnlyMemory<T>` | `Length`, `Span`, `Slice`, `ToArray`; created with `array.AsMemory()` and `text.AsMemory()` |

`Dictionary` and `HashSet` iterate in insertion order (D-29). This makes program output reproducible, because no iteration order depends on the per-process hash keys.

**Layout and hashing** (D-77). `Dictionary` and `HashSet` keep their entries in a dense array in insertion order, with an index table of entry numbers probed 16 slots at a time and one control byte per slot, at a load factor of 7/8. The entries, the index table and the control bytes share one allocation, which growth replaces as a whole, so a table's backing storage is always one block. The block records its slot count, and its size, recomputed from that count, is the size it was allocated with (§13.1). Iteration walks the dense array. A removal leaves a tombstone, and the array compacts when more than half of it is tombstones. Keys are hashed with a per-process random key, which is fixed under `mora test` and `mora bench` so that their counts repeat from run to run (§20.6). `Hasher` is the state that `IHashable.Hash` writes to (§8.5), and its static members `Hasher.Fast` and `Hasher.Strong` name the two algorithms: a fast keyed hash, the default, and SipHash-1-3, for code that hashes keys an attacker controls (OQ-24). `Dictionary` and `HashSet` take the algorithm as an optional constructor argument: `new Dictionary<string, User>(hasher: Hasher.Strong)`.

**Sizing** (D-81). `List<T>` doubles its capacity and rounds the new buffer up to the allocator's usable size (§13.1), so space the allocator would waste becomes usable capacity. The capacity never exceeds `int.MaxValue`, and growing past it panics. `new List<T>()`, `new Dictionary<TKey, TValue>()` and `new HashSet<T>()` allocate no buffer until the first insertion. An empty result therefore has no backing buffer: `ToList`, `ToDictionary` and `ToHashSet` of an empty source, `Where(...).ToList()` that keeps nothing included, allocate only the collection object (§10.13), and `ToArray` of an empty source returns the immortal empty array and allocates nothing.

### 15.4 Lookups never panic

A lookup that can miss returns an option, and a mutation that can conflict returns `bool` (D-65). A panic aborts the process (§11.4), so a missing key in one request must not stop a server.

- `dict[key]` returns `TValue?`. `dict[key] = none` removes the key.
- `First`, `Last`, `Single` and `ElementAt` return `T?`: `none` for an empty sequence, and for `Single`, also for more than one element. `Find` on a list returns `T?`.
- `Dictionary.Add` and `HashSet.Add` return `false` when the item is already there. `Queue.Dequeue` and `Stack.Pop` return `T?`.

Code that knows a value is there says so with `.Expect("message")`, which panics with that message on `none`:

```mora
if (_users[id] is User user) {
    Console.WriteLine(user.Name);
}
let first = orders.First().Expect("orders is never empty here");
if (!_users.Add(user.Id, user)) {
    Console.WriteLine($"user {user.Id} already exists");
}
```

### 15.5 Sequence operators

The operators are members of extension blocks over `IEnumerable<T>` sources (§6.9). Each returns a concrete iterator struct (`WhereIterator<TSource, TPredicate>`), so a chain like `orders.Where(o => o.Paid).Select(o => o.Total).Sum()` compiles to one loop with no allocation and no virtual calls (D-11).

MVP operators: `Where`, `Select`, `SelectMany`, `Take`, `Skip`, `TakeWhile`, `SkipWhile`, `Concat`, `Zip`, `Distinct`, `OrderBy`, `OrderByDescending`, `ThenBy`, `ThenByDescending`, `Reverse`, `GroupBy`, `ToList`, `ToArray`, `ToDictionary`, `ToHashSet`, `First`, `Last`, `Single`, `Any`, `All`, `Count`, `Sum`, `Min`, `Max`, `Average`, `Aggregate`, `Contains`, `SequenceEqual`, `ElementAt`. `First`, `Last`, `Single` and `ElementAt` return options (§15.4). Query syntax (`from x in xs where ... select ...`) is post-MVP.

Operators that need the whole sequence (`OrderBy`, `GroupBy`, `Reverse`, `Distinct`) buffer into a `List<T>` once when enumeration starts.

`IEnumerator<T>.SizeHint()` (§8.5) returns how many items remain: at least `Lower`, and at most `Upper` when that is known. `Lower == Upper` means the count is exact. Arrays, spans and collections report an exact count. The operators pass it on: `Select` keeps it, and `Where` keeps only the upper bound, as `(0, Upper)`. `ToList`, `ToArray` and `ToDictionary` on a source with an exact count make exactly one allocation for their backing storage, or none when the count is zero; a `List<T>` or `Dictionary` object is one more unless it is promoted (§10.13). `OrderBy(...).ToList()` sorts its buffer in place and hands it to the result instead of copying it (D-81). A chain of streaming operators compiles to one loop with no allocation; the buffering operators above allocate their buffer (§10.13, §14.17).

### 15.6 Console, environment and time

| Type | MVP members |
|---|---|
| `Console` | `Write`, `WriteLine` (any `IDisplay`, or a format string with holes), `ReadLine()` returning `string?`, `Error.Write`, `Error.WriteLine`, `Out.Flush()` |
| `Environment` | `GetEnvironmentVariable(name)` returning `string?`, `GetCommandLineArgs()`, `Exit(code)`, `NewLine`, `ProcessId`, `CurrentDirectory` |
| `Random` | `Shared.Next(min, max)`, `NextDouble()`, `Fill(Span<byte>)`, seeded `new Random(seed)`. `Shared` draws from the World and is seeded under `mora test` (§12.1) |
| `Math`, `MathF` | `Abs`, `Min`, `Max`, `Clamp`, `Sqrt`, `Pow`, `Floor`, `Ceiling`, `Round`, trigonometry, `PI`, `E` |

**Time** (D-111). There is one type per concept, so an instant, a duration and a calendar date can never be confused. `Clock`, `Instant`, `MonoTime` and `TimeSpan` are in the prelude (§15.1); the other types below, and `TimeError`, are in `Std.Time` and need `using Std.Time;`.

| Type | Meaning and MVP members |
|---|---|
| `Instant` | A point on the UTC timeline, in nanoseconds. `Clock.Now()` returns one (`uses clock`). `Instant + TimeSpan` gives an `Instant` and `Instant - Instant` a `TimeSpan`; comparison, `ToString()` in RFC 3339 (`2026-10-26T20:15:03.120Z`), `Instant.Parse` returning `Result<Instant, TimeError>` |
| `MonoTime` | A monotonic reading for measuring durations and setting deadlines. `MonoTime.Now()` (`uses clock`), `MonoTime - MonoTime` gives a `TimeSpan` and `MonoTime + TimeSpan` a `MonoTime`. It never serializes |
| `TimeSpan` | An exact duration: `FromMilliseconds`, `FromSeconds`, `FromMinutes`, `FromHours`, `TotalMilliseconds`, arithmetic, comparison |
| `Stopwatch` | `StartNew()`, `Elapsed`, `ElapsedMilliseconds`, `Restart()`, built on `MonoTime`. Each of them reads `MonoTime.Now()`, so each uses `clock` (§6.14); `Elapsed` and `ElapsedMilliseconds` are plain getters, which may use a capability (§6.9) |
| `LocalDate`, `LocalTime`, `LocalDateTime` | Calendar values with no time zone: a birthday, "09:00 every day". `LocalDate.Of(year, month, day)` and the matching `Of` on the others return `Result<_, TimeError>`, so an invalid date is an error value. Components: `Year`, `Month`, `Day` and `DayOfWeek` on dates, `Hour`, `Minute`, `Second` and `Nanosecond` on times, all of them on `LocalDateTime`. Comparison, ISO 8601 `ToString()`, and `Parse` returning `Result<_, TimeError>` |
| `Period` | Calendar arithmetic: `Period.Days(n)`, `Months(n)`, `Years(n)`. `LocalDate + Period.Months(1)` moves by calendar months and clamps the day (January 31 plus one month is February 28 or 29); a `TimeSpan` is always exact |
| `Clock` | `Now()`, the one wall-clock reading in `Std`, routed through the World. `MonoTime.Now()` also goes through the World. Behind the real World, the wall clock is a hosted service of `mora-rt` (§13.1) and monotonic time comes from `mora_pal_now` (§19.4) |
| `TimeError` | A union implementing `IError`: `OutOfRange(Field: string, Value: long)` for a component outside its calendar range (code `time.out_of_range`), and `Format(Input: string)` for text `Parse` rejects (code `time.format`). Both cases are internal, with no `PublicMessage` or `HttpStatus` (§11.2) |

- **Durations use `MonoTime`.** The wall clock can jump (NTP, manual changes), so lint `wall-clock-duration` (warn by default) reports `Instant - Instant` where both readings come from `Clock.Now()` in the same function, with a fix to `Stopwatch` or `MonoTime`.
- **Post-MVP:** `TimeZone` from the IANA time-zone database, `ZonedDateTime` (a `LocalDateTime` with a zone and an offset), explicit resolution of times in a daylight-saving gap or overlap, and RFC 9557 zone suffixes in JSON.

### 15.7 Files and networking

| Type | MVP members |
|---|---|
| `File` | `ReadAllText`, `ReadAllBytes`, `WriteAllText`, `WriteAllBytes`, `AppendAllText`, `Exists`, `Delete`, all returning `Result<_, IoError>`. Synchronous: they block the thread (§12.1) |
| `Directory` | `Exists`, `CreateDirectory`, `GetFiles`, `Delete` |
| `Path` | `Combine`, `GetFileName`, `GetExtension`, `GetDirectoryName` |
| `TcpListener` | `static async Bind(address: string)`, `async AcceptAsync()`, `LocalEndpoint`. Shared class; the destructor closes the socket |
| `TcpStream` | `static async ConnectAsync(address: string)`, `async ReadAsync(Memory<byte>)`, `async WriteAsync(ReadOnlyMemory<byte>)`, `WriteAllAsync`, `ShutdownAsync`, `SetNoDelay`. Shared class; the destructor closes the socket |
| `IoError` | A union of the OS error kinds (`NotFound`, `PermissionDenied`, `AddrInUse`, `ConnectionReset`, `TimedOut`, `Other(Code: int, Message: string)`, ...) with `IDisplay` |

### 15.8 Events

`Signal<T>` is the event type. `Subscribe(move handler: (T) => ())` returns a `Subscription` (unique): dropping it unsubscribes. `SubscribeWeak(target, handler)` holds the target weakly and drops the subscription when the target dies, so a subscription the program forgets to drop does not keep its target alive. `Emit(value: T)` calls each live handler in subscription order. The `@Observable` macro (§9.9) generates one signal per `mut` field for change notification.

### 15.9 Testing

Unit tests live next to the code they test, in a `test` section (D-64):

```mora
public struct Order {
    public Lines: List<Line>;

    public Total(): Cents {
        return Lines.Sum(l => l.Price * l.Quantity);
    }
}

test {
    @Test
    EmptyOrderTotalsZero() {
        let order = new Order { Lines = [] };
        let zero: Cents = 0;
        Assert.Equal(zero, order.Total());
    }

    @Test
    TotalMultipliesPriceByQuantity() {
        let order = new Order { Lines = [OneLine(price: 250, quantity: 2)] };
        let expected: Cents = 500;
        Assert.Equal(expected, order.Total());
    }

    OneLine(price: Cents, quantity: int): Line {  // a helper: no @Test, still test-only
        return new Line { Price = price, Quantity = quantity };
    }
}
```

- A `test { }` section sits at file level or inside a type, and a file may have several.
- Inside a section, `@Test` marks a test and anything else is a helper. `@Test` outside a test section is error `MR0145`, with a fix that moves the function into one. Files in `tests/` are exempt: each is an integration test program, and its `@Test` functions sit at file level (§17.5).
- Sections, their tests, benchmarks and helpers are compiled only under `mora test` and `mora bench`, and so are the files in `tests/`. They never reach a `build` or `run` binary and raise no dead-code warnings. `mora bench` builds the same code with the `release` profile and runs the `@Benchmark` functions instead of the `@Test` ones (§17.1).
- A section at file level sees what other code in its file sees, `internal` and `file` items included. A section inside a type also sees the type's `private` members.
- `Order.tests.mr` next to `Order.mr` is a test section in its entirety, for tests that outgrow the file.
- Integration tests stay in `tests/*.mr` and see only `public` items (§17.5).

A `@Test` function returns `()`, `Result<(), E>` or, if async, either one. In the MVP it takes no parameters. Parameters with generated inputs, shrinking and contract fuzzing come in v0.3 (§6.13), and the runner is designed for them. `Assert.Equal(expected, actual)`, `Assert.NotEqual`, `Assert.True`, `Assert.False`, `Assert.None`, `Assert.NotNone`, `Assert.Contains` and `Assert.Panics(() => ...)` are in `Std.Testing`. A failed assertion panics with a message that shows both values. `Assert.Panics` runs the closure in a child process (§17.5), since panics abort.

**Snapshots** (D-114). `Assert.Snapshot(value)` takes a `T : IDisplay` and compares its display text with the file `tests/snapshots/<test>.snap`, where `<test>` is the test's qualified name (§17.5). `Assert.SnapshotJson(value)` takes a `T : IJsonSerialize` and compares its indented JSON (§16.3) instead, so a diff shows one field per line. When a test takes several snapshots, the n-th one from the second on uses `<test>.<n>.snap`. A missing or different file fails the test with a diff, and a `\r\n` line ending compares equal to `\n`. `mora test --bless` writes the files of the tests it runs instead of comparing them. Snapshot files are committed with the code.

**Examples in doc comments** (D-114). A `mora` code block in a doc comment (§3.2) whose first line is `// expect: ...`, `// error: MRnnnn` or `// warning: MRnnnn` is a test:

```mora
public struct Order {
    public Lines: List<Line>;

    /// Adds up price times quantity over the lines.
    ///
    /// ```mora
    /// // expect: 500
    /// let order = new Order { Lines = [new Line { Price = 250, Quantity = 2 }] };
    /// Console.WriteLine(order.Total());
    /// ```
    public Total(): Cents {
        return Lines.Sum(l => l.Price * l.Quantity);
    }
}
```

`mora test` compiles each example on its own as the body of a test function in the documented declaration's file, so it sees what a test section there sees, and checks it by the rules of §20.4: an `// expect:` example must print those lines, and an `// error:` or `// warning:` example must report that code. A code block with any other first line is only documentation. An example is named after the declaration it documents, then `#` and its position among that declaration's examples, such as `Shop.Order.Total#1`, and `[filter]` selects examples as it selects tests. The generated API reference shows the same examples, so every published example has passed.

A `@Benchmark` function takes no parameters and may return a value, which the runner consumes so that the work is not removed as dead code. `mora bench` runs each one after a warm-up, repeating calls until the timing is stable, and reports per call the mean time and its spread, allocations, bytes allocated (the sum of the sizes passed to `mora_rt_alloc`, §13.1), retains executed (from a run under the debug runtime) and instructions retired where the runner reports them. The counts come from the same allocation counter as the assertions below, so they match what a test would see. `@Benchmark(MaxAllocations = n)` makes `mora bench` fail when a call allocates more than `n` times.

`@Test(MaxAllocations = 0)` fails a test that allocates more than the given number of times, and `Assert.AllocatesAtMost(3, () => Handle(request))` checks one call. Both read the runtime's allocation counter (§13.1), and the count is the same in every build profile (§10.13).

### 15.10 Logging and tracing

`Log.Trace`, `Log.Debug`, `Log.Info`, `Log.Warn` and `Log.Error` take an interpolated string whose target is a log message handler (§7.14), so an ordinary message is also a structured event (D-107). Each of the five takes its own handler type, which knows its level, so its `Begin` compares that level with the configured one and the handler is `Enabled` only when the event will be written:

```mora
Log.Info($"created user {user.Id} for {req.Path}");
```

- **The event** records the template, `created user {user.Id} for {req.Path}`, one field per hole named after the hole's source text (`user.Id = 7`, `req.Path = "/users"`), the level, the time and the current span. The field names come from the string itself, so they cannot drift away from the values. When the same source text appears twice in one message, the second field is named `name#2`, the third `name#3`, and so on.
- **Levels.** `MORA_LOG` sets the lowest level written: `trace`, `debug`, `info` (the default), `warn` or `error`. A call below it costs one branch: the handler is not `Enabled`, so no hole is evaluated (§7.14).
- **Output** goes to stderr, one line per event: `2026-10-26T20:15:03.120Z INFO created user 7 for /users`. In both formats the template, the field names and the field values are written as escaped JSON string contents, so a line break, quote or control character, whether it comes from a value or from the message's own literal text, cannot start a new record, break a JSON key or reach the terminal. With `MORA_LOG_FORMAT=json` each event is one JSON object per line, with the template, the fields, the level, the time and the span.
- **No capability.** Logging is ambient: any code, libraries included, logs without a grant, and a log call or span adds nothing to the caller's effect set (§6.14), even though it reads the clock and writes to stderr, so a `uses none` function may log. A `Secret<T>` hole prints redacted (§15.11).
- **Spans.** `Trace.Span($"...")` starts a span with the same template and fields and returns a move-only value. The span ends when that value is dropped, which happens on every exit path, `?` included (§10.8), and its end records the duration. Events and spans started while it is live belong to it, and a task inherits the span that is current where it is spawned (§12.4). Moving the value into a spawned task hands the span over on purpose.

```mora
LoadUser(id: int): Result<User, Report> {
    let span = Trace.Span($"load user {id}");     // lives to the end of the function
    let text = File.ReadAllText($"users/{id}.json").Report()?;  // an early return ends the span too
    return Json.Deserialize<User>(text).Report();
}
```

A span local is never reported as unused, since keeping it alive is its purpose. The request log of §16.1 is one `Info` event per request. Export to OpenTelemetry is post-MVP.

### 15.11 Secrets

`Secret<T>` holds a value that must not leak: a password, a token, a connection string (D-106).

```mora
@Json
public struct Settings {
    public DatabaseUrl: Secret<string>;
    public Port: int;
}

let settings = Json.Deserialize<Settings>(text)?;
Log.Info($"loaded {settings.DatabaseUrl}");          // writes Secret<string>(***)
let token = settings.DatabaseUrl.Use(bytes => Sign(bytes, payload));
```

- **Printing.** `IDisplay` writes `Secret<string>(***)`, so interpolation, logging, `Console` and error messages never show the value.
- **Reading.** `Use<R>(f: (ReadOnlySpan<byte>) => R): R` is the only way to the plaintext. The span is a view (§10.6), so it cannot outlive the call. Copying the bytes out inside `f` is possible but visible, and lint `secret-copied` (warn by default) flags it. Each call of `Use` adds the fact `secret` to the calling function (§6.14), so `mora explain-effects` shows every place that reads a secret.
- **Erasing.** `Secret<T>` is a shared class that keeps its own copy of the bytes. Copying a secret copies a counted reference (§10.2), so the plaintext exists once however often the secret is passed around, and when the last reference dies the destructor overwrites the bytes with zeros before the memory is freed (§10.8). `Clone()` makes a second copy of the plaintext explicitly. `new Secret<string>(value)` copies `value` in and does not erase it, so secrets are best read straight into a `Secret<T>`, as JSON deserialization does.
- **Serialization.** `Secret<T>` implements `IJsonDeserialize` but not `IJsonSerialize`, so no serializer, generated or written by hand, writes one except through `Use`. `@Json` (§16.3) reads a `Secret<T>` field, decoding the JSON string straight into the secret's own buffer (the input text itself is the caller's and is not erased), but never writes one unasked: `@Json` always generates both interfaces, as its `adds` clause says (§9.2), and its serializer leaves a `Secret<T>` field out of the output. Reading that output back then fails loudly with `missing property`, rather than storing a placeholder as the secret. `@JsonSecret` on the field makes the generated serializer write the plaintext through `Use`.
- In the MVP, `T` is `string` or `byte[]`.

### 15.12 What is not in the MVP

Threads and synchronization (`Thread`, `Mutex`, `Atomic`, `Channel` across threads), HTTP client, TLS, async file I/O, regular expressions, `decimal`, time zones, cultures, compression, cryptography beyond hashing for collections, processes (`Process.Start`), and serialization formats other than JSON. Each is a candidate for post-MVP milestones (§22).

## 16. HTTP and JSON

### 16.1 HTTP server (`Std.Net.Http`)

The server maps each route to a handler with one call, so the program in §2.2 reads familiar to anyone who has written a web API.

```mora
let app = WebApp.Create(args);                    // reads the --urls argument and MORA_URLS
app.MapGet("/health", async (req: Request) => Results.Ok());
await app.RunAsync("127.0.0.1:8080")?;            // address argument overrides configuration
```

**Handlers.** A handler is a function from `Request` to a future of `Result<Response, E>`, for any copyable error type `E : IError` (D-108; type parameters are copyable by default, §8.2, as `Result`'s are), written as an async lambda and taken as a `move` parameter, so it becomes an escaping closure (§10.9). `HttpError` is the error type of the built-in failures, and an application returns its own error types directly, with no conversion to `HttpError`. `MapGet`, `MapPost`, `MapPut`, `MapPatch`, `MapDelete` and `Map(method, pattern, handler)` register them. They are generic over `E`, so each registration compiles a wrapper that turns `Err(e)` into the response §16.2 describes, reading `e`'s members without boxing it. The route table stores these wrappers together, so when a request arrives the router boxes the wrapper's future with `Future.Box`: one allocation per request, counted in the §20.6 budget.

A handler is a root of the app's composition (§16.4). The map functions take it as `move handler: F` with `F : IRoot<S, R>`, where `S` is the app's composition, so its parameters other than the `Request` are services injected for each request, and a handler that takes only the `Request` builds nothing. Each registration's wrapper runs the handler through `RunScoped(handler, request)`, and the router still boxes the wrapper's future, so a request costs that one allocation plus the scoped and transient services its handler takes.

**Routing.** Patterns are literal segments and `{name}` parameters, plus an optional trailing `{*rest}`. Routes are matched by segment, literal segments before parameters, and the first registration wins among equal routes. A path that matches no route returns 404; a path that matches only with another method returns 405 with an `Allow` header. Duplicate registrations panic at startup.

**`Request`.** `Method`, `Path`, `Query` (a `QueryString` with `Get(name)` returning `string?`), `Headers` (case-insensitive `Get`), `Route` (the matched `{name}` values), `RouteValue<T>(name)` returning `Result<T, HttpError>` for `T : IParse`, `Body` (`ReadOnlySpan<byte>` over the buffered body), `ReadTextAsync()`, `ReadJsonAsync<T>()` returning `Result<T, HttpError>`.

**`Results`.** `Ok()`, `Ok(body)`, `Json<T>(value, status = 200)`, `Text(string, status = 200)`, `Created<T>(location, value)`, `NoContent()`, `BadRequest(message)`, `NotFound()`, `Conflict(message)`, `Problem(message, status)`, `StatusCode(int)`. Error results with a message have the body `{"error":"<message>"}` and `Content-Type: application/json`. Responses can be built directly with `new Response(status) { Headers = ..., Body = ... }`.

**Protocol.** HTTP/1.1 with persistent connections, `Content-Length` request bodies and chunked request bodies, `Content-Length` responses, `Date` and `Server: mora` headers, `Connection: close` handling, and `Expect: 100-continue`. HTTP/2, TLS and WebSockets are post-MVP; production deployments are expected to sit behind a reverse proxy that terminates TLS, which the documentation shows.

**Limits**, configurable through `WebAppOptions`: request head 16 KiB, body 1 MiB (413 beyond), 100 headers, header read timeout 10 s, idle keep-alive timeout 60 s, 10,000 concurrent connections, and a request deadline (`WebAppOptions.RequestTimeout`, a `TimeSpan?` field, 30 s by default, `none` for no deadline, §12.4): when it passes, the handler's future is dropped and the client gets 503. Exceeding the head limit returns 431.

**Body memory budget.** Buffered request bodies across all connections share one budget, 64 MiB by default (`WebAppOptions.MaxBufferedBodyBytes`). A request reserves its `Content-Length` before the server reads the body; a chunked body reserves in 64 KiB steps as it arrives. While the budget is exhausted the server stops reading request bodies, which applies TCP backpressure to clients. A request that waits more than 5 s for a reservation gets 503 with `Retry-After: 1` and its connection is closed. A reservation is released when the handler's future completes. The worst case for buffered bodies is therefore the budget, not connections times body limit, and head buffers add at most 16 KiB per connection.

**Concurrency.** Each accepted connection runs as a task in the app's task scope on the single executor thread (§12.1). A connection task handles its own errors and returns no `Result`, so one failed connection never cancels the others (§12.4). Requests on one connection are handled in order. `RunAsync` completes on SIGINT or SIGTERM (Ctrl+C or Ctrl+Break on Windows): it stops accepting, waits up to 5 s for in-flight requests and then cancels them.

**Allocation** (D-80). Each connection task takes a read buffer and a write buffer from a pool. A `Request` keeps offsets into the request-head buffer, and `Headers.Get(name)` builds its `string` only when called, usually inline (§13.4). `Request` and `Response` live in the connection task's state (§12.3). `Results.Json(value)` keeps the value and serializes it straight into the write buffer when the response is written, with no intermediate `string`. A warm `GET /users/1` in §2.2 makes at most 3 heap allocations (§20.6).

**Logging.** One `Info` event per request (§15.10), with the fields `method`, `path`, `status` and `elapsed`, written as `2026-10-26T20:15:03.120Z INFO GET /users/1 200 0.4ms`, unless `WebAppOptions.RequestLog` is `false`. Each request runs in its own span, whose id is the request's trace id.

### 16.2 `HttpError`

```mora
public union HttpError : IError, IFrom<JsonError>, IFrom<ParseError> {
    BadRequest(Message: string),
    NotFound,
    PayloadTooLarge,
    UnsupportedMediaType,
    Status(Code: int, Message: string);

    public static From(e: JsonError): HttpError => BadRequest($"invalid JSON {e}");
    public static From(e: ParseError): HttpError => BadRequest(e.ToString());
}
```

`HttpError` implements the members of `IError` (§11.2) by hand, in its declaration in `Std`: its codes are `http.bad_request`, `http.not_found`, `http.payload_too_large`, `http.unsupported_media_type` and `http.status`, its `HttpStatus` is the case's status, and every case is public: `BadRequest` and `Status` show their `Message`, and the other cases the reason phrase of their status.

**Responses for errors.** A handler's `Err(e)` becomes a response through `e`'s `IError` members, the same way for `HttpError` and for an application's errors (D-108):

- An error with a `PublicMessage` gets the status `HttpStatus`, or 500 when that is `none` or outside 400 to 599 (the server then also logs a `Warn` event naming the error's code), and the body `{"error":"<PublicMessage>"}` with `Content-Type: application/json`, the message written as an escaped JSON string.
- An internal error, with no `PublicMessage`, gets status 500 and an `application/problem+json` body (RFC 9457) with the standard `type`, `title` and `status` members and, beyond them, only the code and the request's trace id: `{"type":"about:blank","title":"Internal Server Error","status":500,"code":"app_error.io","traceId":"4bf92f3577b34da6"}`. Its text never reaches the client.
- An internal error is also logged as one `Error` event with the code, `ToString()` and the chain of `Source()`, in the request's span, so the trace id in the response finds the full story in the log.

`RouteValue<int>("id")` on `abc` returns `BadRequest("route value 'id' is not a valid int")`. `ReadJsonAsync<T>()` returns `UnsupportedMediaType` when the request has a `Content-Type` other than `application/json` (a missing header is accepted), and `BadRequest` for malformed or mistyped JSON.

### 16.3 JSON (`Std.Json`)

**Values.** `JsonValue` is the union shown in §6.6 (named `JsonValue` in the library). `Json.Parse(string)` and `Json.Parse(ReadOnlySpan<byte>)` return `Result<JsonValue, JsonError>`; `JsonValue.ToString()` writes compact JSON and `ToString("indented")` indented JSON.

**Typed serialization.** The `@Json` macro (§9.9) generates `IJsonSerialize` and `IJsonDeserialize` for structs, classes, unions and enums. `Json.Serialize<T>(in value: T)` returns a `string`; `Json.Deserialize<T>(text)` returns `Result<T, JsonError>`. The generated code writes and reads tokens directly, with no intermediate `JsonValue` and no reflection. Serializers write property names as precomputed, already escaped byte literals, and numbers straight into the output buffer. Deserializers pick a field by the key's length and then compare it eight bytes at a time: each complete 8-byte chunk with one load, and the remaining bytes one by one, so no load reads past the key.

| Mora | JSON |
|---|---|
| Field `FirstName` | `"firstName"`: camelCase by default |
| `@JsonName("x")` on a field | `"x"` |
| `@JsonIgnore` | skipped |
| `T?` | `null` when `None`; on input, a missing property also reads as `null` |
| Non-optional property missing on input | `JsonError` naming the property: `missing property "name"` |
| Unknown property on input | ignored, unless the type has `@JsonStrict` |
| Integers | numbers; out-of-range values are errors |
| `float`, `double` | numbers; NaN and infinities are errors on output |
| `string`, `char` | strings |
| Enums | strings with the member name in camelCase |
| `List<T>`, `T[]`, `HashSet<T>` | arrays |
| `Dictionary<string, T>` | objects |
| Unions | `{"type": "circle", "radius": 1.5}`: a `type` discriminator with the camelCase case name, then the case fields. `@JsonTag("kind")` renames the discriminator |
| `Instant` | RFC 3339 string in UTC (`"2026-10-26T20:15:03.120Z"`) |
| `LocalDate`, `LocalTime`, `LocalDateTime` | ISO 8601 strings (`"2026-10-26"`, `"09:00:00"`, `"2026-10-26T09:00:00"`) |
| `TimeSpan` | number of milliseconds |
| `Secret<T>` | read as its value; left out of the output unless the field has `@JsonSecret` (§15.11) |
| `@JsonSecret` on a `Secret<T>` field | written as its plaintext; on any other field it is a compile error from `@Json` |

`JsonError` carries the line, column and a message. Its `ToString()` is `at line 1, column 2: expected string`, which is how §2.2's expected output is produced.

**Limits.** Nesting depth 128 (configurable). Input must be valid UTF-8. Duplicate property names on input: the last one wins.

### 16.4 Dependency injection (`Std.Inject`)

A program declares its services in `services` blocks, and the compiler wires them (D-122). A block is a type, a *composition*, and each of its bindings says which implementation stands for a service and how long it lives. The compiler checks a composition's whole graph when it checks the program and compiles the constructor calls a person would write by hand. Nothing of it exists at run time: there is no container, no way to ask for a service by type, no property injection, and no registration in ordinary code.

```mora
// src/Services.mr
services Storage {
    scoped IUserStore = SqlUserStore;
    decorate IUserStore with CachingUserStore;
}

services AppBase : Storage {
    scoped IAuditLog = FileAuditLog;
    transient WelcomeMail;                      // a class bound to itself
    singleton IMailer;                          // a requirement: every final block binds it
    singleton IHealthCheck += DbCheck;
    singleton IHealthCheck += DiskCheck;
}

@Cfg(Debug = true)
services App : AppBase {
    override singleton IMailer = ConsoleMailer;
}

@Cfg(Debug = false)
services App : AppBase {
    override singleton IMailer = SmtpMailer;
}
```

```mora
// src/Program.mr
using Std.Net.Http;

let app = WebApp.Create<App>(args);              // the one place the composition is named

// The server provides `Request`; the other parameters are injected for each request
app.MapPost("/users", async (req: Request, users: IUserStore, audit: IAuditLog, mail: WelcomeMail) => {
    let input = await req.ReadJsonAsync<NewUser>()?;
    let user = users.Add(input);
    audit.Record($"created user {user.Id}");
    mail.Queue(user);
    return Results.Created($"/users/{user.Id}", user);
});

await app.RunAsync("127.0.0.1:8080")?;
```

**Blocks and bindings.** The fragment below joins the ungrammar of §14.4 when M6.9 adds its parser.

```ebnf
ServicesDecl = Attribute* Modifier* "services" Identifier TypeParams? ( ":" Path ( "," Path )* )? Constraints* "{" Binding* "}" ;
Binding      = Attribute* "override"? Lifetime TypeParams? Type ( "per" Type )? ( "=" ( Type | Lambda | "[" ( Type ( "," Type )* )? "]" ) | "+=" ( Type | Lambda ) )? ";"
             | Attribute* "override"? "decorate" TypeParams? Type "with" Type ";" ;
Lifetime     = "singleton" | "scoped" | "transient" ;
```

- A `services` block is declared at namespace level, may be `public`, and holds only bindings. `services`, `singleton`, `scoped`, `transient`, `per`, `override` and `decorate` are keywords only in these positions (§3.4).
- `scoped IUserStore = SqlUserStore;` binds a service type, an interface or a class, to an implementation, which MUST implement the interface or be the class.
- `transient WelcomeMail;` binds a class to itself. A binding of an interface without `=`, `singleton IMailer;`, is a **requirement**: it fixes the lifetime and leaves the implementation to a block that inherits it.
- `= Settings.Load` or `= (db: Db) => new Cache(db, 64)` binds a service to a **factory**: a method group or a lambda whose parameters are injected and which returns the implementation, `Result<TImpl, E>`, an option of the implementation, or, when it is `async`, a future of one of these. A factory lambda is `static` (§7.6) whether or not it says so, so a capture is `MR0332`.
- A block binds a service with `=` at most once (`MR0701`); `+=` lines for one service may repeat, each adding an element.
- **Associated types.** Each binding defines an associated type of the composition, named after the service: `App.IUserStore` is the type that consumers of `IUserStore` receive in `App`, here `CachingUserStore` around `SqlUserStore`. Hover shows it on every injected parameter.
- **Constructor injection.** Without a factory, the implementation is built by its constructor, whose parameters are its dependencies; nothing marks them. The implementation MUST declare exactly one constructor visible to the block, or none and an initializer on every field. Otherwise it is `MR0708`, whose fix binds a factory that calls the constructor meant.

**Inheriting blocks** (D-124). A block lists its bases after `:`. This combines declarations; it is not class inheritance.

- A block has every binding of its bases. One binding that reaches it through two bases, because both inherit it from a shared base, counts once.
- A block MUST NOT reach itself through its bases, directly or through other blocks (`MR0717`, which prints the cycle).
- Redefining an inherited binding MUST be marked `override` (`MR0700`). Binding a requirement is an override too. An `override` MUST match an inherited binding of the same service with the same lifetime (`MR0702`).
- Two bases that bind one service to different implementations or with different lifetimes are `MR0712`, which names both blocks and both bindings; the fix is an `override` in the combining block. Base order never settles it.
- `override singleton ILogger = FastLog.ILogger;` reuses the binding of the base `FastLog`, its implementation and its factory or configuration, by naming the base's associated type.
- **Collections and decorators combine.** The `+=` elements and the `decorate` lines of several bases are combined, never in conflict: the bases' come first, in base-list order, then the block's own, so the block's own decorator is outermost. These are the only places where the order of bases matters.
- **Replacing a collection or the decorators.** A collection expression assigns a whole list: `override singleton I = [X, Y];` drops every element of `I` the block inherits and puts `X` and `Y`, with that lifetime, in their place; the block's `+=` lines for `I` then add to it, in the order written. An inherited element is kept by listing it again, and `= []` empties the list. Without `override`, assigning a list to a service whose elements a base gives is `MR0700`, as for any inherited binding; in a block with no inherited elements, `= [X, Y]` is the same as two `+=` lines. `override decorate I with D;` drops the inherited decorators of `I` the same way. Either form on a service that no base gives elements or decorators is `MR0702`.

```mora
services App : LibBase {
    override singleton IHealthCheck = [MyDbCheck, QueueCheck];   // replaces LibBase's checks
    scoped IHealthCheck += RequestCheck;   // App's list: MyDbCheck, QueueCheck, RequestCheck
}
```
- `@Cfg` applies to whole blocks and to single bindings (§6.11, §17.2). An inactive block or binding does not exist. Two active blocks with one name are `MR0128` wherever the name is used.
- A **final** composition is one that a host runs: the type argument of `WebApp.Create<App>(args)` or `ServiceHost.StartAsync<TestApp>()`. It MUST bind every requirement it inherits (`MR0713`, naming the requirement and the block that declares it), and the checks of the rest of this section apply to it. Other blocks are checked only for the rules of this list.
- A library publishes blocks with `public services` for apps to inherit. A binding may name a type of the library that the app cannot see, because only the composition builds it.

**Generic blocks** (D-140). A block may take type parameters and constraints, as a generic type does (§8.1, §8.2), so a library can publish one block for a graph that depends on a type the app chooses. The app names an instantiation of it as a base:

```mora
// In a library
public services SqlStore<TDb> where TDb : class, IDatabase {
    singleton DbOptions<TDb>;                    // built by its constructor unless an app overrides it
    singleton IConnectionPool = PgPool;
    scoped TDb;
}

// In the app
services App : SqlStore<UsersDb> {
    override singleton DbOptions<UsersDb> = (cfg: AppConfig) => DbOptions<UsersDb>.Postgres(cfg.Database);
    scoped IUserStore = SqlUserStore;
}
```

- `SqlStore<UsersDb>` in a base list gives the block every binding of `SqlStore` with `UsersDb` in place of `TDb`. The type arguments MUST meet the block's constraints, as any type arguments must (§8.2).
- The cycle rule above goes by block name, whatever the type arguments: `services A<T> : B<List<T>>` with `services B<T> : A<List<T>>` is `MR0717`, although `A<int>` never reaches `A<int>` again. Each path through the bases then meets each block once, so a final composition holds a finite set of instantiations.
- **Two instantiations are two bases.** `SqlStore<UsersDb>` and `SqlStore<AuditDb>` in one base list combine under the rules above. `scoped TDb;` gives two services, `UsersDb` and `AuditDb`, and `singleton IConnectionPool = PgPool;`, which does not use `TDb`, comes out the same from both and counts once. Had the block bound `scoped IDatabase = TDb;`, the two would bind `IDatabase` two ways, which is `MR0712` until the combining block overrides it. The `+=` elements of both join one list, in base-list order.
- One instantiation reached through several bases counts once, as any shared base does, but only with the same type arguments: `services C : A, B` has one `SqlStore<UsersDb>` when `A` and `B` both inherit it, and two instantiations when `B` inherits `SqlStore<AuditDb>` instead.
- **Associated types** are named by the service after substitution: `App.UsersDb`, `App.DbOptions<UsersDb>` and `App.IConnectionPool`. A type parameter never names one, so `App.TDb` is `MR0127`. Reusing a base's binding names the instantiation, as in `SqlStore<UsersDb>.IConnectionPool`.
- **Overrides** match an inherited binding after substitution: `override singleton DbOptions<UsersDb>` in `App` matches `singleton DbOptions<TDb>;` of `SqlStore<UsersDb>`. An `override` that matches no inherited binding after substitution, such as `override singleton DbOptions<AuditDb>` in `App`, is `MR0702`.
- **Checking.** A generic block is checked once, with each type parameter standing for an unknown type that meets its constraints, as a service class is (D-123). Its bindings MUST hold for every such type: without `class` in its constraints, `scoped TDb;` would be `MR0711`, since `TDb` could be a struct. What depends on the type argument itself, such as the constructor that `scoped TDb;` calls and that constructor's dependencies, is checked with each instantiation, in the graph of every final composition that holds it.
- **Diagnostics** that name a block name the instantiation, `SqlStore<UsersDb>` rather than `SqlStore`. That covers the two blocks of `MR0712` and the chain of `MR0703`: ``no binding for `ISchema`, needed by `UsersDb` (scoped, from `SqlStore<UsersDb>`), needed by `SqlUserStore` (scoped), needed by the handler of `POST /users` ``.
- A final composition MUST NOT be a generic block. `WebApp.Create<SqlStore<UsersDb>>(args)` is `MR0716`, whose fix declares a block that inherits the instantiation and runs that block instead, so every world the program compiles is a block it declares.
- **Cost.** Each instantiation is checked on its own, in each final composition that holds it, so the work of checking grows with the instantiations a program uses. Nothing is added at run time: each final composition is already compiled as its own world (D-128), whatever instantiations it holds.

**Choosing an implementation** (D-125). No binding is made by running code, so a run-time `if` around a binding cannot be written.

- **At build time**, `@Cfg` on blocks and bindings, as `App` above shows.
- **At startup or per scope**, among known implementations, a *choice*: a lambda whose results are implementation types rather than values.

  ```mora
  scoped IUserStore = (cfg: StoreConfig) => cfg.UseSql ? SqlUserStore : MemoryUserStore;
  ```

  Every result of the body, through the arms of conditional and `match` expressions, MUST name an implementation type. The parameters are injected. When the binding is built, the body picks a type and the wiring builds that implementation with its own dependencies; the others are not built. `App.IUserStore` is then a closed union of the candidates that the compiler generates, and a call on it compiles to a MIR `Switch` (§14.7) with a direct call in each arm, so it inlines and its effects stay precise (§6.14). Each candidate is checked as if it were bound alone.
- **Open-ended**, a factory that returns `any IUserStore`. Consumers receive the `any` value (§8.4), and only calls on this service are dynamic.
- **Optional**, a factory whose result is an option and returns `none` when the service is absent. Consumers MUST declare `IUserStore?`; a consumer that declares `IUserStore` is `MR0703`.

**Lifetimes** (D-126).

- **Singleton.** Built when the host starts (`RunAsync`, `ServiceHost.StartAsync`), in dependency order, before any root runs. A singleton object is immortal (§13.3): it is never retained, released or freed, its destructor does not run (OQ-27), and the leak report of §10.11 does not count it.
- **Scoped.** Built once per scope (below), and released when the scope ends.
- **Transient.** One constructor call for each consumer, which owns the result.
- A singleton or scoped implementation MUST be a shared class, because its consumers share one object. A struct or a unique class is transient only. `per` belongs to scoped bindings only. Each of these is `MR0711`.
- **Captive dependencies.** A service MUST NOT depend on one that dies before it: a singleton on a scoped service, a service of an outer scope on one of an inner scope, or either through a transient, which lives as long as its consumer. The error, `MR0705`, prints the chain: ``singleton `ReportCache` depends on scoped `DbSession` through transient `ReportQuery` ``.
- A cycle among constructor and factory dependencies is `MR0704`, which prints the cycle.

**Collections.**

- `singleton IHealthCheck += DbCheck;` adds an element to the collection for `IHealthCheck`, where `=` would bind the one implementation. Each element states its own lifetime like any binding: `scoped IHealthCheck += RequestCheck;`. A collection lives as long as its shortest-lived element: with a scoped element it is built per scope, sharing its singleton elements.
- A consumer takes the collection as `ReadOnlySpan<any IHealthCheck>`, a view for use during the call (§10.6), or as `ReadOnlyMemory<any IHealthCheck>`, which a service can keep in a field and an async root can hold across `await` (§5.9). The elements have different types, so each one is an `any` value (§8.4), in the order given above. When every element is a singleton, the array behind the collection is built with the singletons and is immortal; otherwise each scope that uses the collection builds one.
- A service with both `+=` elements and an `=` binding is `MR0707`.

**Service classes** (D-123).

- A class that a `services` block of its own project binds, as an implementation, a decorator or itself, is a **service class**. In it, a bare interface type in a field or a constructor parameter names the binding of the composition that builds the class: below, `_mailer` has type `App.IMailer` in `App` and `TestApp.IMailer` in `TestApp`.

  ```mora
  public class WelcomeMail {
      private _mailer: IMailer;

      public WelcomeMail(mailer: IMailer) {
          _mailer = mailer;
      }

      public Queue(user: User) {
          _mailer.Send(user, "Welcome");
      }
  }
  ```

- The class is checked once, with each such type standing for an unknown type that implements its interface, as a constrained type parameter would (§8.2). It is not generic: its name takes no type arguments, and `List<WelcomeMail>` or `WelcomeMail?` can be written anywhere.
- Each final composition is compiled as its own world. The code that its roots and bindings reach is instantiated for it during monomorphization (§14.10), so every call through a binding is a direct, inlinable call. Two compositions that bind the same types produce identical bodies, which are folded (§14.14).
- A class that no block of its project binds follows §8.3 unchanged: a bare interface field is `MR0236`.

**Scopes and roots** (D-127).

- `host.RunScoped(f)` opens a scope, builds the services `f` needs, calls `f`, and releases the scope's services in reverse construction order, which runs their destructors (§10.8). There is no disposal list. `f` is a lambda or method group whose parameters are injected, and `RunScoped` returns what `f` returns. When `f` is `async`, `RunScoped` returns its future, and the scope ends when the future completes or is dropped (§12.5).
- `RunScoped(f, v1, ..., vn)`, with up to 8 values, provides values to the scope: each `vi` goes to the parameter of its type and is a scoped service of that scope, so a scoped `CurrentUser` can take the `Request`. A provided value whose type is bound, or two of one type, is `MR0701`.
- **Roots.** Every function that reaches `RunScoped`, directly or through a wrapper, is a root, and the compiler generates one wiring function for each. A wrapper takes the function as a type parameter `F : IRoot<S, R>`, which every lambda and method group whose call returns `R` satisfies, with `S` the composition, and passes it on. The map functions of §16.1 are such wrappers. Queue consumers, job runners and command-line runners are libraries built the same way.
- A root's parameter MUST be a bound service, a provided value, a collection, a lazy function (below) or a `Scope<K>`. Anything else is `MR0703`, which names what is missing and who needs it, back to the root: ``no binding for `ITemplates`, needed by `SmtpMailer` (transient), needed by the handler of `POST /users` ``.
- **Parameter modes.** An injected parameter is written without a mode, which is `in` (§10.3). `move` is allowed only for a transient, which the root then owns, and `ref` is never allowed (`MR0715`). An async root owns its parameters, as every async function does.
- **Failing and async construction.** A scoped or transient factory that returns `Err(e)` ends the unit of work: `f` does not run, and `RunScoped` returns `e` converted to `f`'s error type as `?` converts it (§11.3). When `f`'s result cannot hold it, the error is `MR0201`. The HTTP server turns it into a response through `IError`, like any handler error (§16.2). A synchronous root that needs a service with an `async` factory is `MR0710`.
- **No escape.** A scoped service, a `Scope<K>` and a lazy function for a scoped service MUST NOT outlive their root. A service class value MUST NOT leave its composition's world. So a root, and any code it calls, MUST NOT store one in a static or in an object that outlives the scope, such as a singleton, capture it in a closure that outlives the root, such as a `Task.SpawnDetached` task or a stored callback, or return it from the root. The compiler finds these with the escape summaries of §14.18. The wiring's own constructor calls, and `Task.Scope` children, which complete inside the root (§12.4), are not escapes. A violation is `MR0714`, which prints the path.

**Nested scopes.**

- A scope may have a kind, a marker type that a library declares. `host.RunScoped<Connection>(f, socket)` opens a scope of kind `Connection`. A root inside it may take a `Scope<Connection>` parameter, and `conn.RunScoped<Message>(g)`, or `conn.RunScoped(g)` without a kind, opens a child scope in it.
- `scoped ChatSession per Connection;` puts a scoped service in the nearest enclosing scope of kind `Connection`. A plain `scoped` binding means the innermost scope, the one `RunScoped` has just opened.
- The struct of a scope of kind `K` holds every binding marked `per K`, built eagerly when the scope opens, so its layout comes from the composition and needs no analysis of the roots. Every `RunScoped` call is made on a host or on a `Scope<K>` that its enclosing root received, and a `Scope<K>` cannot leave that root, so the chain of kinds around each root is known where the root is written.
- A root that needs a service per `K` and does not run inside a scope of kind `K` is `MR0706`, which names the kind. A service per `K` that depends on a service of an inner scope is a captive dependency, `MR0705`.
- `Scope<K>` follows the view rules of §10.6, with their codes, and `MR0714`, so scopes nest strictly. Unlike other views, it may be a parameter of an async root and live across `await`, because it holds the scope's services instead of pointing into memory that can move.

  ```mora
  services Chat {                                  // a base of App
      scoped ChatSession per Connection;           // one per connection
      scoped MessageLog;                           // one per message
  }

  // A WebSocket library's `MapSocket` runs its handler in a scope of kind `Connection`
  app.MapSocket("/chat", async (socket: WebSocket, conn: Scope<Connection>) => {
      while (socket.IsOpen) {
          let text = await socket.ReceiveTextAsync()?;
          conn.RunScoped<Message>((session: ChatSession, log: MessageLog) => {
              log.Append(text);
              session.Broadcast(text);
          });
      }
      return Ok(());
  });
  ```

**Cost** (D-128). Wiring costs nothing beyond the services it builds:

- The scope is a struct the compiler generates for each root, in the frame of the root's wiring, or in the root's future's state when the root is async. That state is allocated once already (§12.3), so the scope adds no allocation of its own.
- The wiring builds exactly the services the root needs, directly or through other services, eagerly and in dependency order, with no flags, maps, lookups or locks; a scope of kind `K` builds its `per K` services when it opens. A synchronous root receives them as `in` borrows, and an async root has them moved into its state, which drops them in reverse construction order. Neither retains or releases them, except that a service held in two places takes one retain for the second holder, as any shared object does.
- A singleton is one load from the host, and retains and releases of it never write its count (§13.3). A service per `K` reached from a child scope is one load through the `Scope<K>` for a synchronous child root, and one retain and one release for an async one, which owns its parameters (OQ-28).
- Teardown is the destructor calls.
- Calls between services are direct, with precise effects (§6.14), because each composition is its own world. Dynamic dispatch appears only where the source writes `any`: a factory that returns `any I`, a collection, and plugins.
- **Laziness is explicit.** A parameter of type `() => T` receives a function that builds `T` when called: a new one per call for a transient, the singleton for a singleton, and, for a scoped `T`, the scope's instance, built on the first call. That one slot is the only flag the wiring generates. Only a root may take `() => T` for a scoped `T`, and it cannot escape the root (`MR0714`).
- **Unused bindings** are not compiled. Lint `unused-service` (warn by default) reports a binding that no root of a final composition reaches, except one inherited from another project's block.

**Hosts.**

- A composition type implements `IServices`, which the compiler provides, and has no values: it appears only as a type argument.
- `WebApp.Create<App>(args)` returns a `WebApp<App>`. `RunAsync` builds the singletons before it accepts a connection and returns `Err(StartupError)` when a singleton factory fails, so `await app.RunAsync(...)?` ends the process with exit code 1 (§4.6). `WebApp.Create(args)` uses `Std.Inject.NoServices`, the empty composition.
- `await ServiceHost.StartAsync<TestApp>()` builds the singletons and returns `Result<ServiceHost<TestApp>, StartupError>`, for tests, job runners and command-line programs. `StartupError` is an internal `IError` (§11.2) that names the service and carries the factory error's code and text.
- A host is never injected.

**Decorators, generic bindings and configuration** (D-128).

- `decorate IUserStore with CachingUserStore;` wraps the bound implementation. The decorator's constructor MUST take a parameter of the service type, which receives what it wraps (`MR0709` otherwise), and its other parameters are injected. It has the lifetime of what it wraps. Decorators of one service apply in the order given above, the first innermost. A `decorate` with nothing to wrap is `MR0702`.
- `scoped<T> IRepository<T> = SqlRepository<T>;` declares its open type parameters after the lifetime and binds every instantiation that a root needs, checking constraints per instantiation. `scoped IRepository<User> = UserRepository;` may stand beside it and wins for `IRepository<User>` because it is more specific, wherever either is written. Two bindings that match one service equally well are `MR0701`.
- A factory reads configuration, for example `singleton Settings = Settings.Load;` with `Load` returning `Result<Settings, E>`, so invalid configuration fails startup. [post-MVP] `@Config` (M17) binds one typed value per configuration section, validated when the host starts.

**Tests.** A test composition inherits the app's and overrides what the test fakes. A `uses none` bound on the test (§6.14) fails the build with `MR0250`, and the call path, if any service it reaches still uses a capability such as the network, the file system or the clock.

```mora
test {
    services TestApp : App {
        override scoped IUserStore = MemoryUserStore;     // SqlUserStore would use net.outbound
        override scoped IAuditLog = MemoryAuditLog;       // FileAuditLog would use fs.write
    }

    @Test
    async AddRecordsAnAuditEntry(): Result<(), StartupError> uses none {
        let host = await ServiceHost.StartAsync<TestApp>()?;
        host.RunScoped((users: IUserStore, audit: IAuditLog) => {
            let user = users.Add(new NewUser("Ada", none));
            audit.Record($"created user {user.Id}");
            Assert.Equal(1, user.Id);
        });
        return Ok(());
    }
}
```

**Plugins** [post-MVP]. `singleton IExporter += from "plugins/exporters";` adds the implementations that the plugin libraries in that folder provide through the plugin ABI (M8), loaded when the host starts, as `any IExporter` elements after the collection's other elements. The `uses` bounds of `IExporter`'s methods stand for the effects of calls into a plugin, and a call through a method without one is `dyn` (§6.14). Plugins, factories that return `any I` and collections are the only places where injection adds dynamic dispatch.

**Compilation.** `mora-syntax` parses blocks. `mora-check` resolves each block's bases, instantiating generic ones, and its bindings, checks every final composition's graph and checks each root where it is written, against the composition `S` of its `IRoot<S, R>`, before monomorphization, so `mora check` and the language server report every error of this section. `mora-mir` generates the wiring and instantiates each world during monomorphization. `mora expand` shows each root's wiring as Mora code (§9.8).

## 17. Toolchain, CLI and editor support

### 17.1 Commands

| Command | Does |
|---|---|
| `mora new <name> [--lib] [--template web]` | Creates a project: `mora.toml`, `src/Program.mr` (or `src/Lib.mr`), `.gitignore`, and for `--template web` the program of §2.2 trimmed to one endpoint |
| `mora build [--release] [--target T] [--emit obj\|mir\|clif]` | Builds the project into `target/<profile>/<name>[.exe]` |
| `mora run [--release] [-- args]` | Builds and runs |
| `mora check [--watch]` | Runs every analysis and check, no code generation, and warns about files that `mora fmt` would change (§17.7). `--watch` checks again after every change (§17.6) |
| `mora test [filter] [--seed <hex>] [--bless]` | Builds and runs `@Test` functions and doc-comment examples (§15.9, §17.5); `--seed` replays a printed seed; `--bless` rewrites snapshot files |
| `mora fmt [--check]` | Formats every `.mr` file in the project |
| `mora fix [--code MRnnnn] [--applicability safe\|review]` | Applies the fixes of the project's diagnostics, only `safe` ones unless told otherwise, then checks again (§17.6) |
| `mora repl` | Interactive loop (§14.16) |
| `mora explain MR0310` | Prints the long explanation of an error code, with examples of the error and its fix |
| `mora explain-effects <function>` | Prints the function's effect set and, for each effect, the call path that introduces it (§6.14) |
| `mora explain-cost [function] [--diff <revision>]` | Lists, per function and after optimization, every heap allocation with its type, size and the reason it was not promoted (`user` escapes: stored into `_users` on line 14), every `any` box, retains and releases left inside loops, snapshots and copy-in/copy-out, struct copies over 64 bytes, calls left inside loops, bounds checks left in loops, future state sizes and recursive-await boxes, and reuse sites. `--diff` compares with another revision (D-76) |
| `mora docs --llm` | Prints the reference for programs and agents (§17.10) |
| `mora bench [filter]` | Builds the test sections and `tests/` with the `release` profile (§15.9), runs `@Benchmark` functions and reports time with deterministic counts: allocations, bytes allocated, and retains executed under the debug runtime |
| `mora lsp` | Runs the language server on stdin and stdout |
| `mora demangle <symbol>` | Reverses §13.7 mangling |
| `mora version` | Toolchain version, commit, runtime targets installed: three lines (`mora <version>`, `commit: <hash>`, `runtime targets: x86_64-linux, ...` or `none`), or with `--message-format json` one object with `schema` (`mora.version/1`), `version`, `commit` and `runtime_targets`. The targets are the directories of `lib/rt/` (§19.2) that hold `libmora_rt.a`. `mora --version` prints the first line only |
| `mora query`, `mora context`, `mora mcp` | Post-MVP (§17.10) |
| `mora fetch`, `mora publish`, `mora caps` | Post-MVP (§18) |

Exit codes: 0 success, 1 build or test failure, 2 usage error, 101 internal compiler error. Every command takes `--color auto|always|never` and `--message-format human|json`; the JSON format emits one diagnostic object per line, with the fields of §17.6, for CI tools. Every JSON object the toolchain prints follows the rules of §17.10. `auto` colors when the output is a terminal and `NO_COLOR` is unset or empty. The post-MVP commands are unknown commands until they land, so they exit with 2. While the toolchain is being built, a command that does not work yet prints ``error: `mora <command>` is not implemented yet`` and exits with 2. That line is plain text whichever `--message-format` is given, since it is not a diagnostic of §17.6.

### 17.2 Manifest

```toml
[project]
name = "users-api"          # lowercase, digits, '-'
version = "0.1.0"           # semver
kind = "app"                # "app" or "lib"
namespace = "UsersApi"      # root namespace (§4.2); defaults to the PascalCase name
entry = "src/Program.mr"    # app entry file, default shown

[dependencies]
core = { path = "../core" } # binds Core (§4.4). MVP: path only. Post-MVP: version, git, url, registry (§18)

[features]
metrics = []                # enables @Cfg(Feature = "metrics")

[profile.release]
overflow-checks = false     # default; true keeps overflow panics in release builds
runtime = "release"         # "release" or "debug" (§13.2)
debug-info = "lines"        # "none", "lines", "full"

[lints]
boxing = "allow"            # "allow", "warn", "deny"
prefer-let = "warn"
```

Unknown keys are error `MR0500` so that typos fail loudly. `mora.lock` is written in the MVP with path dependencies only, so that its format exists before packages need it (§18.5).

`@Cfg` conditions (§6.11) know `Os` (`linux`, `macos`, `windows`), `Arch` (`x86_64`, `aarch64`, `riscv64`, `wasm32`), both taken from the target description (§19.4), `Debug` (`true` in the dev profile), `Test` (`true` under `mora test`) and `Feature`.

### 17.3 Project layout

```text
users-api/
  mora.toml
  mora.lock
  src/              # the root namespace; each folder below is a nested one (§4.2)
    Program.mr
    Model.mr
    Model.tests.mr  # unit tests for Model.mr, built only by `mora test`
  tests/            # integration tests: each file is a separate test program
    ApiTests.mr
  target/           # build output, ignored by git
```

### 17.4 Profiles

`dev` (default): debug runtime, overflow checks, Cranelift `none` optimization, full debug info. `release` (`--release`): release runtime, wrapping overflow, Cranelift `speed`, line tables only. Both profiles are customizable in `[profile.*]`.

### 17.5 Test runner

`mora test` compiles the project with `Test = true`, collects `@Test` functions from the `test` sections and `.tests.mr` files in `src/` (unit tests, which can see `internal` items, §15.9) and from every file in `tests/` (integration tests, which see only `public` items), builds one test executable, and runs each test. Because a panic aborts (§11.4), the runner forks a child process per test on Unix and spawns one per test on Windows; a panicking test fails without stopping the others. Output follows `dotnet test` conventions: one line per failing test with the assertion message, and a summary `Passed: 41, Failed: 1, Ignored: 2`. `--leak-check` (on by default under `mora test`) fails a test that leaves objects alive (§10.11).

**Seeds** (D-109). Each run of `mora test` draws one seed, which drives `Random.Shared` and the executor's choice among ready tasks in every test (§12.1). A failing test prints it, `seed: 0x9e3779b97f4a7c15`, and `mora test <filter> --seed 0x9e3779b97f4a7c15` runs the test again with the same random numbers and the same task interleaving.

**Doc examples and snapshots** (D-114). `mora test` also runs the examples in the doc comments of `src/` (§15.9), each reported like a test under its example name. `--bless` writes the snapshot files of the tests it runs (§15.9) instead of comparing them.

`mora bench` collects `@Benchmark` functions from the same places: `test` sections, `.tests.mr` files and `tests/`. Tests and benchmarks are named by their qualified name, `Namespace.Type.Function`, and `[filter]` on either command keeps those whose qualified name contains it. `mora bench` runs benchmarks in one process, one after another, and prints one line per benchmark with the counts of §15.9.

### 17.6 Diagnostics

Every diagnostic has a code (`MR` plus four digits), a severity, a primary span, optional secondary spans with labels, a one-line message, an optional note, and zero or more **fixes** (text edits the LSP offers as code actions, each with an applicability).

| Range | Kind |
|---|---|
| `MR0001`-`MR0099` | Lexical |
| `MR0100`-`MR0199` | Syntax and declarations |
| `MR0200`-`MR0299` | Types, generics, interfaces, propagation |
| `MR0300`-`MR0399` | Memory model: moves, borrows, views, escapes, closures |
| `MR0400`-`MR0499` | Async, threads, FFI, constant evaluation |
| `MR0500`-`MR0599` | Manifest and build |
| `MR0600`-`MR0699` | Macros |
| `MR0700`-`MR0799` | Dependency injection |
| `MR2000`-`MR2999` | Warnings and lints |
| `MR9000`-`MR9999` | Internal compiler errors |

Every code is listed once, with its summary, in the registry in `mora-diag`, and the compiler refers to codes only through it, writing each one out as `codes::MR0310`. The spec assigns codes, so a code is registered when the spec first names it. Once a code is used (a crate reports it, a UI test expects it, or an example names it in its first line) it needs a page in `docs/errors/` explaining it in plain terms, with a failing example and a fixed one; `docs/errors/README.md` holds the template. The repository's test suite compiles both examples of every page (§20.4), so explanations cannot go stale. `cargo xtask error-codes` checks that the registry matches the range table above and the codes this document names, that every code is in its range, that every used code has a page, that every page belongs to a registered code, and that no crate outside `mora-diag` names a code in a way the check cannot follow (`Code::new`, or a braced or glob import of `codes`). Diagnostics never use the words "lifetime" or "borrow checker"; they say what would go wrong ("`span` would point at memory that `buffer` no longer owns").

Rendering in the terminal:

```text
error[MR0310]: cannot store `conn` because `Connection` is unique
  --> src/Pool.mr:14:19
   |
12 |     public Return(conn: Connection)
   |                   ---------------- `conn` is only borrowed for this call
13 |     {
14 |         _idle.Add(conn);
   |                   ^^^^ storing it would leave the caller with a connection it no longer owns
   |
help: take ownership of the parameter
   |
12 |     public Return(move conn: Connection)
   |                   ++++
```

The severity is `error` or `warning`. Secondary labels in other files follow the primary file under a `:::` line, the note prints as `= note:`, and each fix prints as a `help:` section showing its edits. Line and column numbers start at 1, and columns count characters, not bytes. The renderer clamps a span that runs past the end of its file or splits a character, so a wrong span from a compiler pass still produces a readable report.

With `--message-format json` each diagnostic is one JSON object on one line. For the example above, in a file whose first eleven lines are not shown:

```json
{"code":"MR0310","severity":"error","message":"cannot store `conn` because `Connection` is unique","primary":{"span":{"file":"src/Pool.mr","byte_start":211,"byte_end":215,"line_start":14,"column_start":19,"line_end":14,"column_end":23},"label":"storing it would leave the caller with a connection it no longer owns"},"secondary":[{"span":{"file":"src/Pool.mr","byte_start":169,"byte_end":185,"line_start":12,"column_start":19,"line_end":12,"column_end":35},"label":"`conn` is only borrowed for this call"}],"note":null,"fixes":[{"message":"take ownership of the parameter","edits":[{"span":{"file":"src/Pool.mr","byte_start":169,"byte_end":169,"line_start":12,"column_start":19,"line_end":12,"column_end":19},"replacement":"move "}]}]}
```

Every span carries byte offsets, for tools that edit text, and the same lines and columns the terminal shows. A label without text is `null`. Each object also starts with `"schema":"mora.diagnostic/1"` (§17.10), and each fix has `"applicability"` after its `message`; the example leaves both out until roadmap deliverable M1.20 adds them, because a test compares it byte for byte with what the compiler prints. Its fix would be `"applicability":"review"`, because it changes the signature of `Return`: callers give up their connection.

**Applicability** (D-113). Each fix says how far a tool may trust it:

| Applicability | Meaning | Applied by `mora fix` |
|---|---|---|
| `safe` | The edit fixes the problem and changes nothing else the program does | By default |
| `review` | The edit is probably what was meant, but it changes behaviour or a signature other code sees, so a person or an agent should read it | With `--applicability review` |
| `placeholder` | The edit leaves text the author must replace, such as a missing argument, so the result does not compile as written | Never |

`mora fix` runs the checks of `mora check`, applies each diagnostic's fix at the requested level, and checks again, repeating while new fixes appear, for at most four rounds. A diagnostic with two fixes at that level is left alone, since choosing between them is the author's call, and of two fixes whose edits overlap only the first in the file is applied. `--code MRnnnn`, which may be repeated, limits it to diagnostics with those codes. It writes only the project's own source files, then prints the diagnostics that remain and exits 1 if an error remains.

**Watching** (D-113). `mora check --watch` keeps running and checks again after a source file or `mora.toml` changes. Each round prints every current diagnostic, not only the new ones, then a summary: the line `checked: 2 errors, 1 warning` in the human format, and the object `{"schema":"mora.check/1","errors":2,"warnings":1}` in the JSON format. A tool replaces what it shows with the diagnostics of the latest round. The command runs until it is interrupted.

### 17.7 Formatter

`mora fmt` produces one canonical layout, with no configuration beyond line width (default 120): K&R braces (an opening brace ends the line that starts its construct, and `} else {` and `} while (c);` share a line), four-space indentation, one statement per line, spaces around binary operators, trailing commas in multi-line lists, `using` directives sorted with `Std` first. It never changes tokens other than whitespace, trailing commas, the order of `using` directives and the parentheses below, and it is idempotent (D-41, amended by D-95 and D-115).

**One reading of `?`** (D-115). Every form parses without types (§14.4), but some `?` forms need the rules of §7.4 to read, and the formatter writes those one way. A propagation `?` directly before the `:` of a conditional gets parentheses, so `c ? r? : d` becomes `c ? (r?) : d`. The formatter never adds or removes the space between `?` and `[`, because `x?[i]` is conditional access and `flag ? [1] : []` is a conditional.

**Format is checked** (D-115). `mora check` reports each file that `mora fmt` would change as warning `MR2030` (lint `format`, warn by default), with its primary span on the first line that differs and a `safe` fix holding the formatter's edits. The repository then has one text form, and `mora fix` restores it.

### 17.8 Language server

The MVP server supports: diagnostics on change (debounced, 100 ms), hover with types and doc comments, go to definition, find references, completion for members, locals, types, namespaces and keywords, signature help, document symbols, workspace symbols, formatting, code actions for every diagnostic fix, and inlay hints for inferred `let`/`var` types, boxing sites (§8.4), shared captured variables (§10.9) and the other implicit steps (D-115): an implicit conversion names the `IFrom` it calls (§5.10), a call to an extension member names its extension block (§6.9), a call that runs an interface default names the interface that gives it (§6.8), and a read of a `static lazy` says that it may run the initializer (§4.7). Hover on `new` says where the object lives and why (§10.13), and hover on a parameter shows `scoped` when it does not escape (§14.18). A cost lens, off by default and turned on per file, adds the hints `heap`, `stack`, `box`, `+1 retain`, `snapshot` and `reuse?` from `mora explain-cost`. Rename, semantic tokens, call hierarchy and refactorings are post-MVP.

### 17.9 VS Code extension

Published as `wolfware.mora` on the Visual Studio Marketplace and on Open VSX. It contains a TextMate grammar for `.mr` files, language configuration (brackets, comments, auto-closing), snippets for common shapes (`class`, `struct`, `union`, `match`, `@Test`), and an LSP client that starts `mora lsp` from the `mora` executable on `PATH` or a configured path. The extension shows the toolchain version in the status bar and offers to install the toolchain when it is missing.

### 17.10 Tools for programs and agents

Editor plugins, CI scripts and coding agents read the toolchain's output, so the output is a contract (D-112, D-113).

**Stable JSON.** Every JSON object the toolchain prints carries `schema`, a name and a version such as `mora.diagnostic/1`. The version grows when a field is removed or changes meaning. A new field does not change it, so readers ignore fields they do not know. JSON output is deterministic: the same sources and options give the same bytes, with diagnostics ordered by file path, then the start of the primary span, then code. Schemas change only in a toolchain release, and its notes list the change.

**`mora docs --llm`** prints a compact Markdown reference meant for a model's context: a summary of the grammar generated from the ungrammar (§14.4), the standard library's public signatures with the first line of each doc comment and each function's effects (§6.14), and every registered diagnostic code with its summary and, where it has a page, the failing and fixed examples. It describes Mora alone and compares it with no other language (D-59). Its first line names the toolchain version, and the text depends on nothing else, so it can be cached per version. The build generates it from the sources of the API reference (§15) and of the error pages (§17.6), so it cannot drift from them, and embeds it in the `mora` executable, so it works offline. The documentation site serves the same text as `llms.txt` at its root (D-130).

**Error pages as data.** For each release the documentation site publishes the pages of `docs/errors/` as one JSON Lines file, `mora-errors.jsonl`, with one object per page: `schema` (`mora.errors/1`), `code`, `summary`, the explanation, and the failing and fixed examples. These are the examples §20.4 compiles, so each failing example is known to fail and each fixed one to pass.

**Planned commands** [post-MVP]. Three commands answer from the analysis database that serves the compiler and the language server (§14.3), under the same JSON rules:

| Command | Returns |
|---|---|
| `mora query symbol`, `refs`, `callers`, `callees`, `type-at`, `effects --why` | A symbol's signature, doc, effects, location and visibility; references and calls with their enclosing symbols; the type at a position with the conversions applied there and the macro that generated it; a function's effects with the call path behind each one |
| `mora context <symbol> --budget <tokens>` | A context pack sized to a token budget: signature, docs, effects, field layout, one hop of callers, and the tests that reach the symbol |
| `mora mcp` | A Model Context Protocol server on stdin and stdout that offers the commands above with `check`, `test`, `expand`, `explain` and `docs`. It is read-only by default: it edits files only through `fix`, and only when started with `--allow-edits` |

None of these tools calls a model: they give an agent facts about the code, and the agent does the rest.

## 18. Libraries, WebAssembly and packages (post-MVP)

This section keeps the original direction for libraries and packages so that MVP choices stay compatible with it. Its details are settled when milestones M8 and M9 start, and each of those milestones begins by updating this section.

### 18.1 WebAssembly target

`mora build --target wasm32-wasi` (WASI preview 2) and `--target wasm32-unknown` (browser) lower MIR to WebAssembly with `wasm-encoder`. Layouts are those of §13 with 4-byte pointers; the shared-object header is 8 bytes, as on every target. The runtime is `mora-rt` compiled for the wasm targets. Async on wasm uses the WASI event loop on `wasm32-wasi` and the host's promise integration in the browser. `mora run --target wasm32-wasi` runs the module in embedded Wasmtime.

### 18.2 The library format

A library is one core wasm module whose custom sections carry the library's metadata and typed IR next to its code:

| Section | Contents |
|---|---|
| Code, data, imports, exports | Compiled non-generic functions; imports from other libraries and from host capabilities |
| `mora.meta` | Types, members, signatures, attributes, doc comments |
| `mora.ir` | Serialized generic MIR (§14.7) for every function: needed to instantiate generics and to inline, and the input for native whole-program builds |
| `mora.deps` | Dependency names, versions and content hashes |
| `mora.caps` | Capabilities the library requests (§18.6) |
| `mora.abi` | ABI version and a hash of the public surface |

Monomorphized generics mean `List<MyType>` cannot exist in the library's compiled code, so the consumer's compiler reads `mora.ir`, instantiates what it needs and compiles it into its own output. The `mora.ir` encoding is versioned and covered by OQ-15.

### 18.3 Linking libraries

- **Wasm app, static (default):** libraries link into one module with one linear memory, so `List<T>` and spans cross library boundaries without copying.
- **Wasm, dynamic:** libraries stay separate modules that import a shared memory and allocator from the host, for plugins.
- **Component Model:** `--emit component` exports `public` APIs as WIT interfaces. It is shared-nothing and copies at the boundary: the cross-language and sandboxed-plugin path, not the default link.
- **Native:** the compiler reads each dependency's `mora.ir` and compiles the whole program (§14). A plain wasm module without `mora.ir`, written in another language, is translated ahead of time from wasm to native code through Cranelift, runs sandboxed in its own memory, and is called through the Component Model boundary.

### 18.4 ABI stability

Within a major version the `mora.abi` surface hash must stay compatible, and `mora publish` refuses a minor release that breaks it. Compatible changes are listed in the spec: adding a member, adding a type, adding a case to a `@NonExhaustive` union, adding an optional parameter at the end with an overload kept for the old signature. Generic and inlined bodies are recompiled by consumers, so changing them changes behaviour but never breaks the ABI.

### 18.5 Imports, manifest and lockfile

Post-MVP dependency sources resolve to a `.wasm` library pinned by content hash in `mora.lock`.

| Source | Example entry under `[dependencies]` |
|---|---|
| Registry package | `json = "2"` |
| Git host and tag | `http = "github.com/wolfware-labs/http@1.4"` |
| Direct `.wasm` URL | `x = { url = "https://.../x.wasm" }` |

- A project still names its dependencies only in `mora.toml`, and each key binds the dependency's root namespace as in §4.4: the `http` entry above gives every file `Http`.
- Scripts and small tools may name a dependency inline instead: `import Http from "github.com/wolfware-labs/http@1.4";` binds `Http` in that file the same way, and `mora fetch` offers to move it into the manifest. This is the only use of `import`.
- A tag is resolved once; its content hash goes into the lockfile. A tag that later points at different bytes is a hard error, not a silent update.
- Git sources may ship a prebuilt `.wasm` as a release asset, or `mora fetch` builds them from source. Registry packages always ship a prebuilt `.wasm` and the source.
- One version per major in a build graph. Two majors of the same package can coexist because symbols are namespaced by major version.

```toml
[[package]]
name = "http"
source = "github.com/wolfware-labs/http@1.4.2"
hash = "sha256-..."
caps = ["net.outbound"]
```

### 18.6 Capabilities

Every library declares the capabilities it needs in `mora.caps`: `net.outbound`, `net.listen`, `fs.read`, `fs.write`, `env`, `clock`, `random`, `process`, `ffi`. The MVP standard library already tags its functions with `@RequiresCapability` and `mora check` computes the set each project uses from the effect analysis (§6.14), so the data exists before enforcement does.

The app manifest grants capabilities per package. A dependency whose update adds a capability fails the build until the grant is approved. On wasm targets an ungranted capability is not linked; on native targets the compiler enforces grants statically, and an `ffi` grant means "trust fully". A JSON parser that suddenly wants the network becomes a build error.

### 18.7 Registry

An optional central package registry: `mora publish` uploads the `.wasm`, the source and a signature. Packages are signed with Sigstore keyless signing, and the registry keeps an append-only transparency log of hashes. Mirrors and private registries use the same protocol. Hosting and governance are OQ-16.

## 19. Targets, distribution and installation

### 19.1 Targets

| Target | Tier in the MVP | CI |
|---|---|---|
| `x86_64-linux` (static, musl) | 1 | build, test, acceptance |
| `aarch64-linux` (static, musl) | 1 | build, test, acceptance |
| `aarch64-macos` | 1 | build, test, acceptance |
| `x86_64-macos` | 2 | build, test |
| `x86_64-windows` (gnullvm) | 1 | build, test, acceptance |
| `aarch64-windows` (gnullvm) | 2 | build, test |
| `riscv64gc-linux` (static, musl) | 3: run tests under QEMU in CI, no release | build, run tests (§19.4 rule 5) |
| `wasm32-wasi`, `wasm32-unknown` | post-MVP | |
| `thumbv7em`, `riscv32` | post-MVP: needs the LLVM backend (M13); `mora-rt-core` already builds for `thumbv7em` in CI | |

Tier 1 targets block a release when any CI job fails. Tier 2 targets ship but a failure is a bug, not a blocker. Cross-compiling with `--target` works from any host to any Linux or Windows target; macOS targets require a macOS host because the system linker and SDK are needed (§14.14).

### 19.2 Toolchain archive

```text
mora-<version>-<host>/
  bin/mora[.exe]
  lib/std/                      # standard library source
  lib/rt/<target>/libmora_rt.a  # hosted layer with mora-rt-core bundled; and libmora_rt_debug.a
  lib/rt/<target>/libmora_rt_core.a  # mora-rt-core alone, for bare-metal platform layers (post-MVP)
  lib/sysroot/<target>/         # musl or MinGW start files and libraries, e.g. x86_64-linux
  lib/ld/bin/ld.lld, lld-link   # one lld under each flavor's name
  lib/ld/lib/                   # the libLLVM it loads (in lib/ld/bin/ on Windows)
  LICENSES/                     # Mora, LLVM, musl, MinGW-w64, winpthreads
```

An archive contains every Tier 1 and Tier 2 target that its host can build for. Every host, macOS included, ships `lld` and the Linux and Windows sysroots, so that it can cross-compile to those targets (§19.1); a macOS target links with the system `ld` (§14.14). `lld` and the MinGW-w64 pieces come from a pinned llvm-mingw release, and the musl pieces from the `rust-std` component of the pinned Rust toolchain. MinGW-w64's profiling code is under the GPL and is not shipped. `LICENSES/README.md` in the repository records which license covers which file. `cargo xtask sysroot` installs the same pieces into `target/sysroot`, laid out as `lib/`, after checking each download against its pinned SHA-256.

### 19.4 Portability

Linux, macOS and Windows are the MVP's targets, and the design keeps every other target reachable without rewriting the core (G7, D-50). Five rules make that hold:

1. **Targets are data.** Each target is a TOML description in `targets/` (for example `targets/x86_64-linux.toml`), read by `mora-target`: triple, pointer width, endianness, integer and float alignments, object format (ELF, Mach-O, COFF, wasm), linker flavor and arguments, the runtime platform layer to link, the backend, and the platform capabilities it offers (`heap`, `threads`, `fs`, `net`, `stdio`, `clock`, `random`, `process`, `env`). `mora build --target path/to/custom.toml` accepts a description that is not built in: a `--target` value ending in `.toml` is a path, and any other value names a built-in target. An unknown key in a description is an error. No code outside `mora-target`, `mora-link` and the backends may branch on a target name, and `cargo xtask layers` rejects a string literal naming a built-in target's name, triple, `os` or `arch` anywhere else.
2. **Layouts are computed, never assumed.** Every size, alignment and offset (§13.3 to §13.6) comes from the target description. The shared-object header is two `u32` counts, 8 bytes on every target (§13.3); the debug runtime's prefix word is pointer-sized. Code that hard-codes a size or a byte order outside `mora-target` is a review blocker.
3. **One platform interface.** `mora-rt-core` calls the platform only through a small set of functions that each platform layer provides: `mora_pal_alloc`, `mora_pal_realloc`, `mora_pal_free`, `mora_pal_good_size`, `mora_pal_abort`, `mora_pal_write_diagnostic` (where panics go), `mora_pal_now` (optional) and `mora_pal_wait` (the executor's idle hook: the `mio` reactor on hosted targets, a wait-for-interrupt loop on bare metal). A new platform implements these, plus whichever hosted services its capabilities declare.

   | Function (C ABI) | Contract |
   |---|---|
   | `u8* mora_pal_alloc(usize size, usize align)` | `size` is not zero and `align` is a power of two; null when out of memory |
   | `u8* mora_pal_realloc(u8* ptr, usize old_size, usize align, usize new_size)` | Keeps the contents up to the smaller size; null, with the block untouched, when out of memory |
   | `void mora_pal_free(u8* ptr, usize size, usize align)` | Takes the size and alignment the block was allocated with |
   | `usize mora_pal_good_size(usize size, usize align)` | The usable size of the block `mora_pal_alloc(size, align)` would return, at least `size`; a platform that does not know returns `size` |
   | `noreturn void mora_pal_abort(void)` | Ends the process at once, without destructors or flushing |
   | `void mora_pal_write_diagnostic(const u8* ptr, usize len)` | Writes UTF-8 text where panics go: stderr on hosted platforms |
   | `u64 mora_pal_now(void)` | Monotonic nanoseconds from an arbitrary start; `u64::MAX` on a platform without the `clock` capability, which is what "optional" means |
   | `void mora_pal_wait(u64 timeout_ns)` | Blocks until an event the platform watches or the timeout; `u64::MAX` waits without a timeout. It may return early, so the executor checks its queues again after every call |
4. **The standard library is layered.** `Std.Core` needs nothing from the platform (primitives, `Option`, `Result`, spans, text formatting into caller buffers, interfaces, `Std.Meta`). `Std.Alloc` needs the `heap` capability (strings, arrays, collections, sequence operators, JSON, futures and the executor). The rest of `Std` needs the hosted capabilities it is tagged with (`Console` needs `stdio`, `File` needs `fs`, `TcpListener` needs `net`). Using an API whose capability the target lacks is error `MR0510`, naming the API, the capability and the target, at compile time instead of link time.
5. **CI guards it from M0.** Before any embedded target ships, CI builds `mora-rt-core` for `thumbv7em-none-eabihf` (`no_std`, 32-bit), runs the layout unit tests against a 32-bit target description (`tests/targets/test32.toml`), and runs the run-test suite on `riscv64gc-linux` under QEMU, a third instruction set that Cranelift supports. A change that breaks one of these fails CI like any other check.

Embedded targets need two more things, both planned: a backend for 32-bit instruction sets, which Cranelift does not generate (LLVM, M13), and a target description with a bare-metal platform layer. Neither changes anything above code generation. Big-endian targets are covered by rule 2 but have no CI guard yet (OQ-19).

### 19.3 Installation

| Channel | Command |
|---|---|
| Shell (Linux, macOS) | `curl -fsSL https://moralang.org/install.sh \| sh` |
| PowerShell (Windows) | `irm https://moralang.org/install.ps1 \| iex` |
| Homebrew | `brew install wolfware-labs/tap/mora` |
| winget | `winget install Wolfware.Mora` |
| Manual | Archives on GitHub Releases |

The scripts install into `~/.mora` (`%USERPROFILE%\.mora`) and add `~/.mora/bin` to `PATH`. `mora self update` replaces the toolchain. Every archive has a SHA-256 checksum and a Sigstore signature produced by the release workflow through GitHub OIDC, whose certificate identity is the release workflow of `wolfware-labs/mora-lang` on a version tag.

What each channel verifies:

| Channel | Integrity | Authenticity |
|---|---|---|
| `install.sh`, `install.ps1` | SHA-256 checksum | HTTPS to `github.com` only. If `cosign` is on `PATH` the script also verifies the signature against the release identity and fails on a mismatch; otherwise it prints the `cosign verify-blob` command to run by hand |
| `mora self update` | SHA-256 checksum | Always verifies the Sigstore signature and the release identity, with the verification built into `mora`, before replacing anything |
| Homebrew, winget | The checksum pinned in the formula or manifest | The tap and the winget repository, which are updated by the release workflow |
| Manual download | `SHA256SUMS` | `cosign verify-blob` with the certificate identity above, documented on the install page |

The domain is `moralang.org` (D-130).

## 20. Testing and quality

### 20.1 Test layers

| Layer | What | Where |
|---|---|---|
| Unit | Rust tests inside each crate | `crates/*/src`, `crates/*/tests` |
| UI | A `.mr` file per case, with `//~ ERROR MR0310` annotations on the lines that must report; the rendered output is snapshot-tested | `tests/ui/` |
| Run | A `.mr` program with `// expect:` stdout, optional `// exit:` code; built, run through the JIT and through an AOT build, outputs compared (differential); its HIR and MIR are snapshot-tested beside it (`.hir`, `.mir`) | `tests/run/` |
| Memory | The suite of §20.3 | `tests/memory/` |
| MIR snapshots | `--emit mir` after ARC optimization for the guarantees of §10.13 | `tests/mir/` |
| Spec examples | §20.4 | extracted |
| CLI | `mora` commands end to end on temporary projects | `crates/mora-cli/tests` |
| LSP | Scripted sessions: open, edit, expect diagnostics, hover, completion | `crates/mora-lsp/tests` |
| Acceptance | §20.2 | `examples/users-api` |
| Fuzz | Lexer and parser (no panics, lossless round trip), programs generated from the ungrammar (§14.4), type checker on generated programs | `fuzz/`, nightly job |

The UI and run harnesses live in `mora-testkit` and run under `cargo test`. Each finds the `.mr` files below its directory, runs them in parallel and reports every failure with a diff; a panic fails only its case.

- **UI annotations** go after the code on the line where the primary label starts: `//~ ERROR MRnnnn` or `//~ WARNING MRnnnn`, several per line if needed, with any text after the code as a comment. A diagnostic without an annotation fails the case, and so does an annotation nothing reported.
- **UI snapshots** are the diagnostics rendered without color as in §17.6, in the `.stderr` file beside the case; a case that reports nothing has none. `cargo test -p mora-testkit --test ui -- --bless` rewrites them.
- **Run expectations** are one `// expect:` line per line of standard output, in order, and at most one `// exit:` line, 0 when absent. The program runs through every backend the compiler has, and each run is compared with them, so the JIT and the AOT build must agree. A `\r\n` line ending compares equal to `\n`.

### 20.2 The MVP acceptance test

A Rust test in `mora-testkit` builds `examples/users-api` with `mora build --release`, starts it on a free port, runs the request transcript of §2.2 over TCP, compares status codes, headers that matter (`Location`, `Content-Type`) and bodies byte for byte, sends SIGINT (or Ctrl+Break on Windows), and checks for a clean exit with code 0. It runs on every Tier 1 target in CI.

### 20.3 Memory-model suite

- **No leaks:** every run test executes under the debug runtime with the leak report on; any object alive at exit fails the test.
- **No use-after-free:** the debug runtime poisons freed memory and traps on releases of dead objects; adversarial tests exercise every row of the table in §10.4, each closure rule of §10.9 and each view rule of §10.6.
- **Drop order:** programs whose destructors print, checked against the order §10.8 defines, including early returns, `?`, `break`, moves in branches and drop flags.
- **Weak references and cycles:** expected `none` reads after the target dies; the leak report naming the fields of a deliberate cycle.
- **Collections:** structural changes during `foreach` and while a span is live panic with the §10.10 message.
- **Optimizer guarantees:** MIR snapshots for each item of §10.13.

### 20.4 Executable examples

`cargo xtask spec-examples` extracts every `mora` code block in this document and in the pages of `docs/errors/` (not the template in its README) whose first line is `// expect: <stdout>`, `// error: MRnnnn` or `// warning: MRnnnn`, compiles it, and checks the output or the reported diagnostic. CI runs it on every change, so a spec change that breaks an example fails, and an example cannot drift away from the implementation. `mora test` checks the examples in a project's doc comments by the same rules (§15.9).

An `// expect:` example passes the run checks of §20.1. An `// error: MRnnnn` or `// warning: MRnnnn` example passes when the compiler reports that code with that severity and no error with another code. Each example has an id: the number of the section it is in, or the code of its error page, then `#` and its position among the examples there, such as `2.1#1` or `MR0300#2`. An example that needs a feature not yet built is listed in `tests/spec-examples.pending` with the roadmap deliverable that makes it pass. The check fails when a listed example passes, when a listed id or deliverable does not exist, and when an unlisted example fails, so the list only shrinks.

### 20.5 Continuous integration

Required checks on every pull request: the portability guards of §19.4 rule 5, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` on Linux, macOS and Windows, `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`, `cargo deny check`, `cargo xtask layers`, `cargo xtask spec-examples`, `cargo xtask error-codes` (every code used has a page; `spec-examples` compiles the pages' examples), `cargo xtask codegen --check` (§14.4), and the acceptance test on Tier 1 targets once milestone M6 delivers it. Third-party GitHub Actions are pinned by commit SHA. On every pull request that changes code, `cargo mutants` mutates the changed lines; each mutant must make a test fail or stop the build, and one that survives fails the check, because it marks code that no test checks. Generated code and the `xtask` glue files are left out, as for coverage (`.cargo/mutants.toml`). A coverage job runs the workspace tests under `cargo llvm-cov`, and a separate job that runs no repository code uploads the report to Codecov through GitHub OIDC; Codecov reports project and patch coverage on each pull request that changes code (Markdown-only changes skip the job with the other build jobs) and leaves out the generated code under `crates/mora-syntax/src/generated/` and the `xtask` files that only run other programs (`main.rs`, `runtime.rs`). The report is best effort: pull requests from forks get no OIDC token, so they are not uploaded, and an upload that fails during a Codecov outage does not fail the build. A change that touches only Markdown may skip the Rust build jobs, except changes to `docs/design/mora.md`, `docs/errors/` and `docs/guide/`, which contain executable examples and always run `spec-examples` and `error-codes`.

### 20.6 Budgets

Initial targets, measured on the CI Linux x86_64 runner and revised at the end of milestone M7:

| Measure | Budget |
|---|---|
| `mora build` of hello world, warm standard-library cache | under 1.5 s |
| `mora build` of `users-api`, warm cache | under 5 s |
| `mora check` after editing one function in `users-api` (LSP, warm) | under 150 ms |
| Hello world release binary on Linux | under 1.5 MiB |
| Process start to first line of output | under 5 ms |
| `users-api` `GET /users/1`, keep-alive, one core | at least 20,000 requests per second |
| `users-api` `GET /users/1`, warm | at most 3 heap allocations per request |

A pull request that worsens a measured budget by more than 10% fails the `bench` check unless its description explains why. The check also tracks deterministic counts (allocations, bytes allocated, retains executed under the debug runtime, and instructions retired where the runner reports them), which do not vary from run to run because `mora bench` fixes the hash key (§15.3).

**Agent success** (D-112). An eval suite in `evals/` holds 100 to 200 small tasks, such as an endpoint, a parser or a refactor of a union, each with a prompt and tests the model does not see. Every release candidate runs it with two or three widely used models and one small model that runs locally. The model versions are fixed for a release line and listed in `evals/models.toml`, so results compare across the releases of that line. Each model gets the task, the toolchain, the output of `mora docs --llm` and the diagnostics in JSON. The suite reports two rates per model: the tasks that compile on the first try, and the tasks whose tests pass within three rounds of compiling and fixing. A release whose rate falls by more than 10% from the previous release of its line needs an explanation in its release notes, as a slower benchmark does in a pull request. The results guide fixes, messages and documentation, not syntax.

## 21. Project conventions

- **Repository:** `github.com/wolfware-labs/mora-lang`, public from the first commit (D-32). Dual licensed under MIT and Apache-2.0; contributions require the Wolfware Contributor License Agreement, checked by a bot on the first pull request; `CODE_OF_CONDUCT.md`, `SECURITY.md` and `CONTRIBUTING.md` as in other Wolfware projects.
- **Spec first:** this document, its examples and the golden files under `tests/` are the oracle. A behaviour change updates the spec in the same pull request, before or with the code. Deciding behaviour in code without writing it here is a review blocker.
- **Roadmap IDs:** every deliverable in `ROADMAP.md` has an ID such as `M2.4`, and maps to one issue and one pull request. Spec references look like §10.4, decisions like D-12, open questions like OQ-3.
- **Commits and pull requests:** Conventional Commits with the roadmap ID at the end of the subject, for example `feat(mir): add the move checker (M2.3)`. Imperative, lowercase, at most 72 characters. Pull requests are squash-merged and their title becomes the commit. Every commit is signed; the organization ruleset rejects unsigned commits.
- **Labels:** `kind/*` (`feature`, `bug`, `spec`, `docs`, `test`, `infra`, `perf`, `security`, `tracking`), `area/*` (one per crate group: `syntax`, `hir`, `check`, `mir`, `macro`, `async`, `codegen`, `link`, `rt`, `std`, `http`, `driver`, `pkg`, `fmt`, `lsp`, `vscode`, `repl`, `cli`, `testkit`, `ci`, `release`, `docs`), `platform/*` (`linux`, `macos`, `windows`), `needs-triage`.
- **Milestones** are named `M0: Foundations` and so on, each with a `kind/tracking` issue listing its deliverables.
- **Rust:** toolchain pinned in `rust-toolchain.toml` (1.99.1 at the time of writing), `RUSTFLAGS=-D warnings` in CI, `cargo deny` for licenses and advisories, Dependabot for Cargo and GitHub Actions.

## 22. Roadmap

Mora is built in thin vertical slices: each milestone runs real programs end to end before the next one adds breadth. The memory model comes second because every later decision depends on it. `ROADMAP.md` breaks each milestone into deliverables with IDs, owners, dependencies and tests; this table is the summary.

| Milestone | Goal | Exit criteria |
|---|---|---|
| **M0: Foundations** | Repository, CI on Linux, macOS and Windows, workspace crates and layering check, spans and diagnostics, error-code registry, test harnesses (UI, run, spec examples), `mora-rt` skeleton built for every Tier 1 target | CI green on all three OSes; harnesses run an empty suite; `cargo xtask layers` enforces §14.2 |
| **M1: Hello, native** | Lexer and full parser with a lossless tree, name resolution and type checking for namespaces, functions, structs, primitives, strings, locals and control flow, MIR lowering, Cranelift code generation, linking on every Tier 1 target, with identical code folding on Linux and Windows, `Console.WriteLine` with interpolation, `mora build`, `run` and `check` | §2.1 builds and runs on every Tier 1 target in CI; the M1 run tests pass through JIT and AOT |
| **M2: Memory model** | Classes, structs, unique classes, parameter modes, move checking, drop elaboration, destructors, aliased-place borrows, views and spans, weak references, closures, ARC insertion and optimization with ownership and modification summaries, release at last use, the bundled allocator and 8-byte header, 16-byte strings, niche layouts, copy elision, inlining and scalar replacement, stack promotion, reuse for `with` and string `+=`, allocation counters and `MaxAllocations` tests, debug runtime leak tracking | The suite of §20.3 passes; the §10.13 snapshots pass |
| **M3: Expressive core** | Unions, enums, patterns and exhaustiveness, options and `none` with flow narrowing, `Result` and `?`, operators and conversions, generics and monomorphization, interfaces with static and dynamic dispatch, associated types, indexers, extension blocks, tuples, collection expressions, interpolation handlers, newtypes and refinements | Run and UI tests for every construct of §§5 to 8; every §2.4 example runs |
| **M4: Macros** | `macro` declarations, `code` quoting and splices, hygiene, the `Std.Meta` model, the sandboxed JIT runner and its cache, `mora expand`, `MacroTest`, and the standard-library macros of §9.9 except `@Json` | Every §9 example runs; `@Equatable`, `@Hashable`, `@Comparable`, `@Display`, `@Default`, `@Clone` and `#Assert` pass their `MacroTest` suites; structs get their automatic implementations from these macros |
| **M5: Standard library core** | Text, collections with borrow counts, sequence operators, console, environment, time, random, files, JSON values and the `@Json` macro, `Html` and `Url`, logging and tracing, `Secret<T>`, `Report`, `Std.Testing`, `mora test` and `test` sections, insertion-ordered hash tables, collection sizing, SIMD text kernels, bounds-check removal, `mora explain-cost` | The std API reference builds from doc comments; JSONTestSuite parsing cases pass; chains of streaming sequence operators meet §15.5 with no allocation |
| **M6: Async and HTTP** | Async lowering, inline futures and pooled task blocks, executor, tasks and cancellation, reactor, TCP, timers, HTTP/1.1 server with pooled buffers, routing, results, `HttpError` and error responses from `IError`, compile-time dependency injection | The acceptance test of §20.2 passes on every Tier 1 target. The MVP is functionally complete |
| **M7: Developer experience and v0.1** | Language server with the cost lens, VS Code extension, formatter, REPL, `mora bench`, `mora new` templates, `mora explain` and error pages, documentation site with the language tour, installers, release workflow, budgets | v0.1.0 published on every channel of §19.3; §1.5 holds; budgets of §20.6 measured and either met or revised in this document |
| M8: WebAssembly and libraries | §18.1 to §18.4, OQ-15, a stable plugin ABI at an opt-in boundary | post-MVP |
| M9: Packages and capabilities | §18.5 URL and git sources, §18.6, grants narrowed per dependency, sandboxed runs | post-MVP |
| M10: Threads | §12.6, OQ-1, OQ-6, OQ-7, supervised worker processes | post-MVP |
| M11: Language growth | Inheritance, variance, iterators, retroactive implementations, arenas, unsafe, `decimal`, `async?`, more macro kinds (OQ-3, 4, 5, 9, 10, 11, 12, 13; §9.10) | post-MVP |
| M12: Registry and signing | §18.7, registry dependency sources | post-MVP |
| M13: Backends and targets | LLVM release backend, Component Model, Windows debug info, embedded targets | post-MVP |
| M14: Contracts and checked types | §6.13, `where` predicates, property tests, unit algebra, compile-time evaluation | post-MVP |
| M15: Test and run tooling | Simulated World, test caching, record and replay, hot reload | post-MVP |
| M16: Agent tooling and API evolution | `mora query`, `mora context` and `mora mcp`, JSON Schema, semantic diff and semver (contract changes from M14), editions | post-MVP |
| M17: Standard library growth | Time zones, `@Config`, retry policies, `Task.Shield` | post-MVP |
| M18: Optimizer | General reuse, MIR deduplication, vector types, devirtualization, `@NoAlloc` | post-MVP |

## 23. Decisions register

Source: **user** means decided by the project owner in the design discussion; **draft** means carried over from the first draft of the language; **author** means a call made while writing this document, open to review.

| ID | Decision | Source | § |
|---|---|---|---|
| D-1 | The language is named Mora; CLI `mora`; crates `mora-*`; custom sections `mora.*` | user | intro |
| D-2 | Source files use the `.mr` extension | user | 3.1 |
| D-3 | Hybrid memory model: values by default, ARC classes, unique classes, weak references, no lifetimes | draft | 10 |
| D-4 | One built-in executor in the MVP; a pluggable executor interface is post-MVP | author | 12.1 |
| D-5 | Thread-shared classes need an explicit `shared` marker; nothing is inferred from `spawn` sites | author | 12.6 |
| D-6 | `T?` is `Option<T>` for every type | draft | 5.6 |
| D-7 | `null` is kept as the literal for the empty option (superseded by D-54) | author | 5.6 |
| D-8 | No exceptions and no `try`/`catch` syntax | user | 11 |
| D-9 | No bridge to another language's runtime or package ecosystem | author | 1.3 |
| D-10 | Attributes are compile-time only | draft | 6.11 |
| D-11 | Generics are monomorphized; sequence operators compile to concrete iterator structs | draft | 8.1, 15.5 |
| D-12 | Integer overflow panics in debug and wraps in release unless configured; `+%` family always wraps | draft, author | 7.12 |
| D-13 | The MVP is native-first: Cranelift native code and JIT; the wasm backend and `.wasm` libraries are post-MVP | author | 1.5, 18 |
| D-14 | `string` is UTF-8 with byte `Length`; `char` is a Unicode scalar value | author | 5.2, 5.3 |
| D-15 | Fields are read-only unless marked `mut` (write scopes added by D-101) | draft | 6.3, 6.9 |
| D-16 | Union case names resolve without the union prefix where the union is the expected type | author | 6.6 |
| D-17 | Destructors are written `~T()` and run deterministically | author | 6.9 |
| D-18 | `Future<T>`, `TaskHandle<T>` and `Subscription` are unique; I/O handles are shared classes so async methods can keep them alive; no automatic rule makes types unique. Amended by D-73: a future is a move-only state value, and `Future<T>` is its boxed form | author | 6.4 |
| D-19 | In `await e?` the `?` applies to the awaited value | author | 7.4 |
| D-20 | A collection expression with no target type is a `List<T>` | author | 7.9 |
| D-21 | Doc comments are Markdown | author | 3.2 |
| D-22 | Superseded by D-96: a file's namespace comes from its folder | | |
| D-23 | Generic parameters are copyable unless declared `where T : allows move` | author | 8.2 |
| D-24 | Superseded by D-48: there is no fixed derive list | | |
| D-25 | Variance is spelled `in`/`out` on type parameters when it lands | author | 8.8 |
| D-26 | A panic aborts the process with exit code 101; there is no unwinding | author | 11.4 |
| D-27 | `async F(): T` declares the awaited type; futures are cold and unique. Spelled name-first since D-58 | draft, author | 12.2 |
| D-28 | No borrows across `await`, so futures never point into themselves and need no pinning | author | 12.3 |
| D-29 | `Dictionary` and `HashSet` iterate in insertion order | author | 15.3 |
| D-30 | No class inheritance in the MVP; interfaces with default methods instead | author | 6.1 |
| D-31 | The MVP is proven by a native REST API with JSON on Linux, macOS and Windows | user | 1.5, 2.2 |
| D-32 | The project lives in a public repository, `wolfware-labs/mora-lang` (named `wolfware-labs/mora` until 2026-10-31), from day one, with the conventions of other Wolfware projects | user | 21 |
| D-33 | The MVP includes the LSP, VS Code extension and installers; D-53 sets the audience | user, author | 1.5, 17 |
| D-34 | HTTP and JSON ship in the standard library in the MVP | author | 16 |
| D-35 | Local path dependencies are in the MVP; remote sources are post-MVP. Amended by D-96: dependencies are named only in `mora.toml` | author | 4.4 |
| D-36 | MVP programs are single-threaded, which keeps reference counts non-atomic and race-free | author | 12.1 |
| D-37 | The toolchain bundles `lld`, musl and MinGW pieces so that Linux and Windows builds need no system toolchain | author | 14.14 |
| D-38 | `ref` borrows of non-plain aliased places use copy-in/copy-out; `in` borrows use snapshots | author | 10.4 |
| D-39 | Escaping closures share captured variables that are reassigned | author | 10.9 |
| D-40 | There is no `using` disposal statement; destructors release resources | draft | 4.3, 10.8 |
| D-41 | The formatter enforces Allman braces and four-space indentation. Amended by D-95 and D-115 | author | 17.7 |
| D-42 | Enumerators detect modification with a version number; only spans of growable collections use the borrow count | author | 10.10 |
| D-43 | A lambda becomes a non-escaping closure, a closure struct or an escaping closure depending on the parameter it is passed to | author | 10.9 |
| D-44 | The standard library may use a private `unsafe` dialect and intrinsics; user code may not in the MVP | author | 13.11 |
| D-45 | Async I/O takes `Memory<T>` / `ReadOnlyMemory<T>` buffers | author | 5.9, 15.7 |
| D-46 | Resurrection by a destructor is detected at run time and aborts | author | 6.9 |
| D-48 | Macros are in the MVP: Mora functions that run at compile time on a typed model and return quoted code (`code { }`, `$x`); attribute and expression macros; the derive list becomes standard-library macros | user, author | 9 |
| D-49 | Ownership, move, borrow, view and escape violations are compile errors that cannot be silenced; the remaining run-time checks panic or abort | user, author | 10 |
| D-50 | Targets are data; layouts are computed from the target; the runtime splits into a `no_std` core and platform layers behind one platform interface; the standard library is layered by capability; CI guards portability from M0 | user, author | 19.4 |
| D-47 | Unique objects follow struct mutation rules: `mut` methods take `ref this`, and `in` borrows are read-only | author | 6.4, 10.4 |
| D-51 | No primary constructors on classes: constructors are written out, and `: this(...)` chains them. Amended by D-56 (positional parameters now belong to positional structs, which replace records) and D-57 (a type without a constructor is created memberwise) | user | 6.9 |
| D-52 | Interfaces keep default implementations: members of different interfaces stay separate, and when two interfaces give different defaults for one inherited member, the implementing type must write its own | user | 6.8 |
| D-53 | Mora is not a copy of any other language: it uses familiar syntax, names and idioms where they fit its own model, and its audience is application developers in general. Supersedes the audience of D-33 | user | 1.1, 1.2, 1.4 |
| D-54 | The empty option is written `none`. Supersedes D-7 | user | 3.4, 5.6 |
| D-55 | Raw strings are written with backticks | user | 3.5 |
| D-56 | Type declarations are struct (a value with generated equality and with) and class (identity); record is removed | user | 5.1, 6.1, 6.2, 6.5 |
| D-57 | A type without a constructor is created memberwise with new T { ... }; required and init are removed. Amends D-51 | user | 6.2, 6.3, 6.9 |
| D-58 | Declarations put the name first and the type after a colon (`Balance: decimal`, `Deposit(amount: decimal): decimal`, `let fee: decimal`); an omitted return type means `()` | user | 4.6, 5.2, 6.1, 6.9, 7.6, 7.10, 10.3, 12.2, 14.4 |
| D-59 | The compiler knows only Mora syntax: code from other languages gets the ordinary syntax error, with no dedicated codes or fixes | user | 3.4, 17.6 |
| D-60 | The MVP runtime is thread-ready, not threaded: reference counting has plain and atomic forms, the executor lives in a per-thread context, and `Std` keeps no global mutable state | user | 12.1, 13.3 |
| D-61 | Values derived from object identity come from a per-process counter, never from an address | user | 6.3 |
| D-62 | `newtype Name : T is pattern;` declares a new value type with `T`'s layout and an optional pattern refinement checked where values are created | user | 6.12 |
| D-63 | Contracts live in `contract` sections linked to members by name, never on the signature line; the MVP reserves the word and v0.3 checks them | user | 6.13 |
| D-64 | Unit tests live in `test` sections or sibling `.tests.mr` files, compiled only under `mora test` | user | 15.9 |
| D-65 | A lookup that can miss returns an option and a mutation that can conflict returns `bool`; `dict[key]` is `TValue?`, and `First`, `Last`, `Single` and `ElementAt` return `T?` | user | 15.3, 15.4 |
| D-66 | One spelling per construct: `readonly`, `static readonly`, `#region`, `new[] { }`, array initializers, several declarators in one declaration, `where T : new()` and `TryParse` are removed | user | 3.2, 4.7, 5.4, 8.2, 15.1 |
| D-67 | A bare interface type is an implicit generic in a parameter and an opaque type in a return; a boxed interface value is written `any I` | user | 8.3, 8.4 |
| D-68 | Mora code allocates through a bundled mimalloc on every hosted target, with sized frees and capacities that use the allocator's usable size; the allocator is part of the platform interface | user | 13.1 |
| D-69 | An object whose reference does not escape, by a published rule, lives in the frame or async state instead of the heap; escape facts are whole-program summaries, not inlining | user | 10.13, 14.18 |
| D-70 | Ownership and modification summaries drive reference-count elision, and a value whose drop runs no user code is released at its last use in every build | user | 10.8, 14.12 |
| D-71 | `with` and string `+=` reuse a unique value's storage in place; general reuse is post-MVP | user | 14.19 |
| D-72 | Shared objects have an 8-byte header with no type pointer in release builds; the debug runtime keeps type metadata in a prefix word | user | 13.3 |
| D-73 | A future is an inline move-only state value; `await` allocates nothing for the awaited future's state, `Task.SpawnDetached` and `scope.Spawn` allocate one block (renamed by D-110), and an await in a cycle of the async call graph is the only implicit box; `Future.Box(f)`, always explicit, allocates once. Amends D-18 | user | 12.2, 12.3 |
| D-74 | The MIR optimizer inlines and scalar-replaces before ARC optimization, with budgets counted in MIR statements, so sequence operator chains and the listed loops have published guarantees | user | 14.1, 14.17 |
| D-75 | `string` is a 16-byte value that stores up to 15 bytes inline; a concatenation allocates at most once, and so does an interpolation whose holes do not allocate | user | 5.3, 13.4 |
| D-76 | Costs are visible and testable: `mora explain-cost`, the LSP cost lens, allocation counters in every build, `MaxAllocations` tests and deterministic benchmark counts | user | 10.13, 17.1 |
| D-77 | `Dictionary` and `HashSet` are dense insertion-ordered arrays with a Swiss-table index, both in one allocation, and a keyed fast hash | user | 15.3 |
| D-78 | `T?` is the same size as `T` for every type with a niche, and the spec lists those types | user | 5.6, 13.5 |
| D-79 | Returned values and call results are constructed in their final place | user | 10.13 |
| D-80 | The HTTP server and JSON serializers work in pooled buffers, with a budget of 3 heap allocations for a warm `GET /users/1` | user | 16.1, 20.6 |
| D-81 | Collections size themselves: exact-count sources allocate their backing storage once, growth rounds up to the allocator's usable size, and an empty collection allocates no backing buffer (the collection object itself follows §10.13) | user | 15.3, 15.5 |
| D-82 | On Linux and Windows the linker merges identical functions; macOS folding and merging identical MIR bodies are post-MVP | user | 14.14 |
| D-83 | Byte scans in the runtime use SIMD; vector types for Mora code are post-MVP, and Cranelift output is not auto-vectorized | user | 13.1 |
| D-84 | Rejected: deferred or coalesced decrements and background cycle collection, because they free memory at points the reader cannot see | user | 10.8 |
| D-85 | Rejected: automatic struct-of-arrays layout, because it changes the cost of element access, `ref` borrows and spans in ways the reader cannot predict | user | 13.5 |
| D-86 | Rejected: pointer compression and packing `bool` fields into bits; Mora has no contiguous heap base, and each field must stay addressable for `ref` | user | 13.5 |
| D-87 | Rejected: automatic string interning, a hidden hashing cost and a global table that keeps strings alive | user | 13.4 |
| D-88 | Rejected: copy-on-write value collections; collections stay reference types, mutated in place | user | 15.3 |
| D-89 | One `match` with arms, `match (e) { p => e, ... }`, as a statement and as an expression; there are no `case` labels and no postfix form. Amended by D-129 | user | 7.7 |
| D-90 | `==` and `!=` come from `IEquatable<T>` and the relational operators from `IComparable<T>`; declaring a comparison operator is `MR0238` | user | 6.9, 8.5 |
| D-91 | A value converts implicitly only through the standard-library interface `IFrom<S>`, which the standard library's own types implement like any other; `implicit operator` and `explicit operator` are removed | user | 5.10, 11.3 |
| D-92 | Function types use the lambda arrow, `(A, B) => R`; `Func`, `Action` and named delegate types are removed, and `->` gets a fix to `=>` | user | 5.8 |
| D-93 | There are no `out` parameters; several results are a tuple, an option or a `Result` | user | 7.2, 10.3 |
| D-94 | Attributes are written `@Name`, one per `@`; a keyword is never a name, so `@class` no longer escapes one | user | 3.3, 6.11 |
| D-95 | The formatter enforces K&R braces and four-space indentation. Amends D-41 | user | 17.7 |
| D-96 | A file's namespace is the project's root namespace plus its folder path under `src/`; `namespace` declarations, the `import` statement and `Alias::Name` are removed from projects, and dependencies are named only in `mora.toml`, where each key binds the dependency's root namespace. Supersedes D-22, amends D-35 | user | 4.2, 4.4, 17.2, 18.5 |
| D-97 | A declaration without a visibility modifier is visible in its folder; `private` is the type, `internal` the project, `public` everyone | user | 4.5 |
| D-98 | Extensions are declared only in extension blocks, `extension<T>(list: List<T>) { ... }`; a future `extension T : I` follows the orphan rule in libraries, apps are exempt, and libraries wrap foreign types in a `newtype` | user | 6.9, OQ-5 |
| D-99 | `new` stays; `[]` with a `Dictionary` target is an empty dictionary | user | 7.9 |
| D-100 | `@From` never marks a field of an `indirect` case, because wrapping it would allocate in an implicit conversion | author | 11.3 |
| D-101 | State lives in fields: visibility says who reads, `mut` who writes, and `mut(private)`, `mut(ns)` or `mut(internal)` narrows writes; auto-properties, accessor bodies and setters are removed, and a write that does work is a method | user | 6.3, 6.9, 9.5 |
| D-102 | Effects are inferred per function from capability-marked std functions, charged to the code that performs them, and bounded where written with `uses`; `uses none` means no capability; lint `blocking-in-async` | user | 6.14, 9.7, 14.8, 18.6 |
| D-103 | `=>` getters and index `get` accessors are checked to be cheap (no heap allocation, loop, mutation or failure, `MR0251`), trusting `Std` functions marked `@Cheap` not to loop (`MR0254` outside `Std`); a `stored` getter is recalculated when an input is written, and its inputs are writable only inside its type (`MR0252`) | user | 6.9, 6.11, 15.3 |
| D-104 | Change notification is the `@Observable` macro over `Signal<T>`, not a language feature; its generated `SetX` is marked `@SoleWriter`, so any other assignment to an observed field is `MR0253`; cached `memo` getters are post-MVP | user | 6.11, 9.9, 15.8 |
| D-105 | An interpolated string builds the handler type its target asks for (`IInterpolate`, `IHole<T>`); trusted text arrives only as the compiler-made `Literal`, and `string` never converts to a handler type such as `Html` or `Url` | user | 7.13, 7.14, 8.5, 15.2 |
| D-106 | `Secret<T>` prints redacted, is read only through a view in `Use`, keeps one copy of its plaintext behind a counted reference and erases it in its destructor, and is left out of JSON output without `@JsonSecret` | user | 6.14, 15.11, 16.3 |
| D-107 | Logging takes interpolated strings and records each hole as a field named after its source text; a disabled level skips the holes; spans end when their value is dropped; logging needs no capability | user | 15.10, 16.1 |
| D-108 | `IError` carries `Code`, `PublicMessage` and `HttpStatus`; HTTP handlers return any copyable `E : IError` and the server maps it without boxing, showing only public messages; `Report` collects context and is reached explicitly with `.Context(...)` or `.Report()`, never by a bare `?` | user | 11.2, 11.5, 16.1, 16.2 |
| D-109 | Clock, timers, randomness, I/O and task scheduling go through one runtime seam, the World, internal in the MVP; `mora test` seeds randomness and the scheduler, prints the seed on failure and replays it with `--seed`; a public and simulated World are post-MVP | user | 12.1, 17.5 |
| D-110 | `Task.Scope` fails fast: a child's `Err` of the scope's error type cancels the body and its siblings and becomes the result (`MR0404` for another error type); deadlines are inherited per task, set by `Task.WithTimeout` and `Task.WithDeadline`, which replace `Task.Timeout`; `Task.Spawn` becomes `Task.SpawnDetached` with lint `detached-task` | user | 12.4, 16.1 |
| D-111 | Time has one type per concept: `Instant`, `MonoTime`, `TimeSpan`, `LocalDate`, `LocalTime`, `LocalDateTime` and `Period` replace `DateTime`; lint `wall-clock-duration`; time zones are post-MVP | user | 15.6 |
| D-112 | Mora is documented for models as well as people: `mora docs --llm` prints a deterministic, versioned reference built from the sources of the API reference and served as `llms.txt`; the error pages are published as a dataset of paired examples; an agent eval suite with fixed model versions is a release budget | user | 17.1, 17.10, 20.6 |
| D-113 | Each fix carries an applicability, `safe`, `review` or `placeholder`; `mora fix` applies `safe` fixes unless told otherwise and checks again; `mora check --watch` streams diagnostics; JSON output carries a `schema` version and is deterministic; `mora query`, `mora context` and `mora mcp` are post-MVP | user | 17.1, 17.6, 17.10 |
| D-114 | Doc-comment examples whose first line is `// expect:`, `// error:` or `// warning:` are tests that `mora test` runs; `Assert.Snapshot` compares output with files in `tests/snapshots/`, and `mora test --bless` rewrites them | user | 3.2, 15.9, 17.1, 17.5, 20.4 |
| D-115 | Every construct parses without types, the ungrammar is the normative grammar and is fuzzed against the parser, the formatter writes the `?` forms of §7.4 one way, implicit steps show as inlay hints, and `mora check` warns about unformatted files (`MR2030`). Amends D-41 | user | 1.6, 14.4, 17.1, 17.7, 17.8, 20.1 |
| D-116 | Rejected: algebraic effect handlers; resumable handlers need stack capture or CPS, which conflicts with state-machine futures (D-27, D-28) and drop elaboration, and the World plus effect inference cover testing and injection | user | 12.1 |
| D-117 | Rejected: content-addressed code storage; code stays in text files, and per-definition semantic hashes give provenance and change detection | user | 18.5 |
| D-118 | Rejected: LLM client, prompt and cost-budget types in the language or std; provider APIs change too often for std, and clients belong in packages | user | 15 |
| D-119 | Rejected: model-backed or other non-deterministic steps inside the compiler or `mora fix`; a build is a pure function of its inputs, and `mora fix` applies only fixes the compiler generates | user | 17.1 |
| D-120 | Rejected: database migrations and feature flags as language features; `@Config`, `@Cfg(Feature = ...)` and macros cover what the language owes them | user | 17.2 |
| D-121 | Rejected: stackful green threads to remove async colouring; async stays stackless (D-27, D-28), and `async?`, `blocking-in-async` and structured concurrency reduce the cost | user | 12.1 |
| D-122 | Dependency injection is resolved at compile time from `services` blocks: a block is a composition type whose bindings give each service an implementation and a lifetime; constructor injection needs no attribute and one constructor; no registration exists in ordinary code, and there is no container, service lookup or property injection at run time | user | 3.4, 16.4 |
| D-123 | Each binding defines an associated type of its composition; in a class that a block of its project binds, a bare interface type in a field or constructor parameter names that binding; the class is checked once and instantiated per composition, and each final composition is compiled as its own world, so calls stay direct; `any I` implements `I`. Extends D-67 | user | 8.3, 8.4, 16.4 |
| D-124 | Blocks inherit bases: redefining a binding needs `override`, conflicting bases need an `override` in the combining block, the same binding reached twice merges, a binding without `=` is a requirement every final composition must bind, `Base.IService` reuses a base's binding, elements and decorators combine in base-list order unless an `override` list assignment or `override decorate` replaces them, and `@Cfg` selects blocks and bindings | user | 16.4 |
| D-125 | Implementations are chosen at build time with `@Cfg`, at startup or per scope with a choice lambda that compiles to a closed union, open-endedly with a factory returning `any I`, and optionally with a factory returning `none` | user | 16.4 |
| D-126 | Services are singleton, scoped or transient; singletons are built at host startup and immortal; singleton and scoped implementations are classes; a captive dependency is a compile error that prints the chain; collections are `+=` elements consumed as `ReadOnlySpan<any I>` or `ReadOnlyMemory<any I>` | user | 16.4 |
| D-127 | `RunScoped(f)` opens a scope and every function that reaches it is a root with its own generated wiring; HTTP handlers and other runners are wrappers over it; nested scopes use `Scope<K>` and `per K`; scoped services, scopes and their lazy functions cannot escape the root | user | 16.1, 16.4 |
| D-128 | Wiring costs nothing beyond the services: a generated scope struct in the root's frame or future, eager construction, direct calls and destructors for teardown; laziness is an explicit `() => T`; factories may be async and fail; decorators, generic bindings with declared parameters, tests by overriding blocks with `uses none`, and plugins [post-MVP] complete the model; the checks run in `mora check` and the language server | user | 16.4 |
| D-129 | The pattern-matching construct is spelled `match`, a contextual keyword that starts a `match` only before a parenthesized scrutinee and `{`; `switch` is not a keyword | user | 3.4, 7.7, 14.4 |
| D-130 | The project domain is `moralang.org`; installers and project links use it | user | 17.10, 19.3 |
| D-131 | One rule for `MR0180`: a local, a lambda or local-function parameter, or a pattern binding may not take the name of a local or parameter in scope, including in the same block; side-by-side blocks may reuse a name | user | 7.10 |
| D-132 | The bindings of an `is` pattern are in scope to the end of the statement that holds it, so a negated test with an early return does not bind for the code after it | user | 7.8 |
| D-133 | Types and values are looked up apart: a type position skips the value members of the enclosing types, so a field may share its type's name | user | 14.5 |
| D-134 | Nesting is limited to 256 levels, chains included; deeper nesting is error `MR0018`, once per statement | user | 3.1, 14.4 |
| D-135 | Until the compiler can look up `IFrom` implementations (M3.6), the checker carries the numeric rows of §5.10 as a built-in table that stands in for those of `Std`; the conversions are the same | user | 5.10 |
| D-136 | A call of a function with one declaration reports a wrong-typed argument as `MR0203` and a missing or extra argument as `MR0205` naming its signature; overloads get `MR0205` and `MR0206` listing the candidates; a positional argument after a named one and a repeated name are `MR0207` in every call, and an unknown name and a name for a parameter given by position are `MR0207` too, with overloads only when every overload fails that way for a name given by position; an argument whose marker does not suit its parameter's mode makes the function not applicable, `MR0205` with a fix for one declaration; a `ref` argument has exactly its parameter's type; calling a value of a known non-function type is `MR0208`; only visible methods are candidates, and a call of a value's method none of whose overloads is visible is `MR0129` | user | 6.9, 10.3 |
| D-137 | Until type arguments are inferred (M3.8) and `params` arguments are collected into a span (M2.9), a call that a generic, `params` or `async` overload could take is not checked, nor, until M3.13, a static member named through a value (D-152); an argument or a parameter of a type not known yet fits, and a call it leaves more than one overload for is not resolved or reported, with a type only when they all return it; a lambda or a `new` without a type passed to a call that is not checked, to a method of a value whose methods are not known yet, to a constructor or to an indexer takes its target type when that call is checked and is not reported before, while a function of the standard library, not loaded yet, gives none; a literal ranks its natural type first, then that type's §5.10 conversions, then the types it takes only as a literal | user | 6.9 |
| D-138 | Control flow is checked per body: until the standard library is loaded, `panic`, `Unreachable` and `Todo` are recognised by name, alone or after `Std.`, as ending a path; a loop ends a path only when its condition is the literal `true` and no `break` leaves it; a `match` whose every arm ends the path ends it, unless its last arm has a guard, whose failing leaves the path open, and whether its arms cover every value is checked with exhaustiveness (M3.4); `MR0184` covers functions, local functions, functions that implement an interface member and functions of an extension, and the block bodies of getters, operators and indexers wait for their return types (M2.1) and top-level statements for theirs (M3.7), and an `async` function's body is not checked for `MR0184` until async lands (§12); a `let` assigned in a loop is assigned again only by a run that goes back to the loop's start; unreachable code reports nothing about assignment; a lambda or a local function is checked where it is declared, sees the locals assigned there and may not assign a `let` around it | user | 7.10, 7.11 |
| D-139 | Deconstruction is an assignment as well as a declaration: `(a, b) = value;` assigns each place on the left, locals, fields, elements and nested tuples, with `_` discarding an element only there; a tuple literal is checked element by element against the places, and another value by its type, element by element; `_` counts toward the number of elements, and a value of another shape, one that is not a tuple or has another number of elements, is `MR0203`, except a positional struct, whose deconstruction is not checked yet, as in a declaration; an assignment to something that is not a place, as in `1 = 2;` or `(a, F()) = pair;`, is not reported until places are checked (M2) | user | 5.5, 6.5, 7.10 |
| D-140 | A `services` block may be generic, with type parameters and constraints; an instantiation such as `SqlStore<UsersDb>` is a base, two instantiations with different type arguments are two bases whose bindings combine under D-124, and one instantiation reached through several paths counts once; associated types and `override` match services after substitution, so a type parameter never names an associated type and an `override` with no match is `MR0702`; a generic block is checked once with each type parameter standing for an unknown type that meets its constraints, and what depends on the type argument is checked per instantiation in each final composition; diagnostics name the instantiation; a final composition must not be generic (`MR0716`); a block that reaches itself through its bases by name, whatever the type arguments, is `MR0717`, so instantiation ends; there is no run-time cost | user | 16.4 |
| D-141 | An assignment is accepted only where nothing uses its value: an expression statement, a `for` update, an arm of a `match` statement, the whole expression body of a function, constructor, destructor, setter, local function or lambda whose result is `()` (for a lambda, only where a function type is expected), and the branches of a conditional in such a place; used as a value, including in an interpolation hole, it is `MR0185`, with a fix that splits `x = y = 5;` into `y = 5; x = y;` where a statement fits before it, and `x++` stays allowed as a value; a `ref` parameter cannot have a default value (`MR0209`), with a fix that removes the default and one that removes `ref` | user | 7.11, 10.3 |
| D-142 | In `new T { ... }`, the written field values run left to right, then the initializers of the omitted fields run in declaration order | user | 6.9 |
| D-143 | In memberwise `new T { ... }`, each name is an instance field of `T` and is assigned once: a name that is no member, a getter, a `static` field or a constant is `MR0147`, suggesting the closest unassigned field, and a repeated field is `MR0148`, with a fix that removes the repetition; on a type that declares a constructor, `new T(args) { ... }` runs it and assigns the members afterwards, so a field may be assigned twice there, while the names are still checked (D-147) | author | 6.9 |
| D-144 | A static member has no `this`: its body, and a local function or lambda inside it, that writes `this` or names an instance field, getter or method of the type is `MR0149`, with no fix, because passing the value as a parameter and dropping `static` both keep the program meaningful; a member of a nested type, `static` or not, has no value of the types around it, so naming one of their instance members is `MR0149` too | author | 6.10 |
| D-145 | The initializer of an instance field has no `this`: it, and a lambda or local function inside it, that writes `this` or names an instance field, getter or method of the type is `MR0152`, with no fix; `static` members and constants stay allowed, because they need no value of the type | user | 6.9 |
| D-146 | A method call evaluates its receiver expression before its arguments; a receiver taken by `in` or `ref` (a `mut` method) is borrowed after the arguments and default values, so the method sees what they did to it, while a `move` receiver, like every argument, is a value taken in order; this matches how a local function borrows a captured `this` after its arguments, and the borrow is what the callee reads, while a moved value is the callee's own copy from that point | author | 7, 14.7 |
| D-147 | `new T { ... }` without parentheses on a type that declares a constructor, or on a positional struct, is `MR0153`, because memberwise creation would skip the constructor; the fix adds `()` when the type declares a constructor visible there that takes no arguments, all its parameters having defaults, and every name in the braces is a field visible there and declared `mut` without a write scope, and offers nothing otherwise, since only the author knows which arguments to pass or how to set a read-only field; the assignments after a constructor call are plain ones, so a name that is not an instance field is `MR0147`, suggesting the closest field visible there and declared `mut` without a write scope, assigned already or not, and a field not visible there is `MR0129`, whose fix to widen it comes only on the first assignment of a field declared `mut` without a write scope and never under `MR0153`, where widening leaves the program invalid, while a field may be assigned twice; the checks that such an assignment may write the field, a read-only field or one outside its write scope, come with the other write checks in M2.1; `new T(args)` with arguments on a type that declares no constructor and is not positional is `MR0205` | author | 6.5, 6.9 |
| D-148 | `new` creates a struct or a class: `new T` with a written type of any other kind, such as `int`, an option, an enum, an interface or a type parameter, is `MR0154`; a `new` without a type creates its target type, which must be a struct or a class; with no target type, as for the initializer of a `let` or `var` without a type, a `new()` statement, an operand, a receiver or an interpolation hole, or with a target type of any other kind, it is `MR0154`; a branch, an arm, a tuple or collection element and a lambda's result have the target of the value they are in, and a value for a field of a type that is an error or checked later is not reported; there is no fix, since only the author knows which type is meant | author | 6.9, 7.3 |
| D-149 | A modifier written twice in the modifiers of one declaration, parameter or lambda is `MR0111`, with a fix that removes the repetition when both are written the same, and no fix for a `mut` repeated with another write scope, since only the author knows which scope is meant | author | 6.1 |
| D-150 | A lambda or a `new` without a type passed to a call, or a conditional, `match`, tuple or collection with one in a branch, an arm or an element, takes the type of its parameter: with one function, its parameter of that name or at that position; with overloads, the type that every overload able to take the arguments has there, when they agree, where an overload is able when it takes their count and names, every argument fits its parameter's mode and every other argument, typed first without a target, fits its parameter's type; when they do not, such an argument gets no target type and a `new` without a type is `MR0154`, unless the call is already `MR0205`, `MR0206` or `MR0207`, or another argument or a parameter of those overloads has a type not known yet, an error included, which leaves the call unresolved and unreported (D-137) | author | 6.9 |
| D-151 | Two visibilities on one declaration, two modes on one parameter, and `mut` with `move` on one method are `MR0112`, with a fix that removes the later modifier, since each set says one thing at most; a modifier that repeats an earlier one is `MR0111` alone | author | 6.1, 10.3 |
| D-152 | Naming a `static` member through a value, as `p.S()` for a static method or `p.K` for a static field or getter, is `MR0155`, with a fix that writes the type's name in place of the value (`P.S()`); the check lands in M3.13 | user | 6.10 |

## 24. Open questions

| ID | Question | Default until decided | Decide by |
|---|---|---|---|
| OQ-1 | Should a panic in a server task end only that task (unwinding or task-level abort with cleanup) instead of the process? | The process aborts (D-26) | M10 |
| OQ-2 | How do Windows builds get debug info (CodeView/PDB) for debuggers and backtraces? | Symbol names only | M13 |
| OQ-3 | Generic variance: rules and implementation | Not supported (D-25 fixes the spelling) | M11 |
| OQ-4 | Class inheritance: `abstract`/`open` classes, `virtual`, `override`, `protected`, `base`, and how it interacts with ARC | Not supported (D-30) | M11 |
| OQ-5 | Retroactive interface implementations, under the coherence rule of D-98 | Not supported | M11 |
| OQ-6 | Threads: executor model (one per core or work stealing), `static mut` access, `Task.Run` | Single-threaded (D-36) | M10 |
| OQ-7 | Are string and array reference counts always atomic once threads exist, or atomic only after their first send? | | M10 |
| OQ-9 | `yield return` iterators | Not supported; library iterators are hand-written | M11 |
| OQ-10 | Dynamic interface values holding move-only values | Error `MR0230` | M11 |
| OQ-11 | `unsafe` code and raw pointers | Not supported; FFI limited by §13.9 | M11 |
| OQ-12 | `decimal`, `Int128`, `UInt128` | Not supported | M11 |
| OQ-13 | Allocator-parameterized collections (`List<T, Arena>`) and the allocator interface | Global allocator only | M11 |
| OQ-14 | Cultures and globalization for formatting and parsing | One culture-independent convention (§15.2) | after M9 |
| OQ-15 | Encoding and versioning of `mora.ir` | | M8 |
| OQ-16 | Registry hosting, governance and name policy | | M12 |
| OQ-17 | Does user code need an on-disk incremental cache in the MVP? | No, unless §20.6 budgets fail | M7 |
| OQ-19 | Which big-endian target, if any, gets a CI guard (s390x under QEMU is the candidate)? | None | M13 |
| OQ-20 | Do structs also get generated ordering (`IComparable<T>`, by field order)? | No; `@Comparable` adds it (§9.9) | M4 |
| OQ-21 | The rules by which the compiler stores a value inline, on the stack or in a heap cell (§6.1, §10.13) | A struct lives inline in its owner and is boxed only as §8.4 says; a class lives on the heap | M2 |
| OQ-22 | Predicate refinements on newtypes (`where EmailAddress.IsValid(value)`): which functions may a predicate call, and how is that checked? | Only patterns in the MVP (§6.12) | M14 |
| OQ-23 | Stack promotion limits: 1 KiB per object and 16 KiB per frame, and the limit inside async states | As in §10.13 | M2 |
| OQ-24 | Default hash function for `Dictionary` and `HashSet`: speed against resistance to hash flooding for string keys | Keyed fast hash, `Hasher.Strong` available | M5 |
| OQ-25 | Is `scoped` a keyword that checks the fact, or only a hover annotation? | Hover only in the MVP | M7 |
| OQ-26 | Is the boxed future spelled `Future<T>` or `any IFuture<T>`? | `Future<T>` | M6 |
| OQ-27 | How does a singleton that holds a resource, such as a connection pool or a log file, shut down in order when the program exits, since singletons are immortal (§16.4)? | Singleton destructors never run; the operating system reclaims the resources at exit | M10 |
| OQ-28 | Can an async child scope reach the services of an enclosing scope through a pointer, with no retain or release, when futures may move (D-28)? | One retain and one release per such service and child scope (§16.4) | M11 |

## 25. References

- Alex Reinking, Ningning Xie, Leonardo de Moura, Daan Leijen. *Perceus: Garbage Free Reference Counting with Reuse*. PLDI 2021.
- Joe Duffy. The Midori blog series, on a safe systems language with ownership and no collector, used to write the Midori research operating system.
- Luc Maranget. *Warnings for pattern matching*. Journal of Functional Programming, 2007.
- David F. Bacon, V. T. Rajan. *Concurrent Cycle Collection in Reference Counted Systems*. ECOOP 2001.
- WebAssembly Component Model and WIT; WASI preview 2.
- Sigstore: keyless signing and the Rekor transparency log.
