# Run tests

Each `.mr` file here is a program (§20.1). Its `// expect:` lines give its standard output, one line each, and an optional `// exit:` line gives its exit code, 0 by default:

```mora
// expect: 3
Console.WriteLine(1 + 2);
```

The harness first compiles the program, which must report no error, and compares the text form of its HIR and of its MIR with the `.hir` and `.mir` files beside it. Then it builds and runs the program through every backend the compiler has, the JIT and an AOT build, and compares each run with the expected output. While the compiler has no backend, compiling and the two snapshots are the whole test. A program that reports errors has neither snapshot.

In the HIR, a local is `name#n`, and a name that is not a local is followed by what it names in braces, as `Console{external}` or `X{field App.Point.X}`. An operand that is itself an operation is in parentheses.

The MIR is one function per body (`fn name(params) -> type { ... }`), each a list of typed locals and numbered basic blocks, `bb0`, `bb1` and so on, without gaps. A block is a list of statements ending in one terminator: `goto bbN;`, `switch`, `return;` or a call. Every statement ends in `// line:col`, the place in the source it comes from. A local keeps its HIR number (`x#0`), `this` is the receiver and an unnamed temporary is `_n`, where `_0` is the return value. A real constant always has a decimal point (`5.0`), so it reads differently from an integer.

- `copy x` reads a place and leaves it usable. `move x` hands it over.
- `&in x` and `&ref x` borrow a place, to read it or to change it. `(*x)` is the place a borrow `x` points to, so a parameter `n: int` is read as `copy (*n#0)`.
- `(T)a` converts a number to `T` as the source wrote it. `(implicit T)a` is a conversion the compiler inserted because every value fits, such as `int` to `long`.
- `call f(args) -> bbN;` calls `f` and continues in `bbN`; `x = call ...` stores the result. `-> !` marks a call that does not return.
- `Console.WriteLine{external}` is a function of the standard library, which is not loaded yet, called by the path as written. It is the only external callee here.
- `$"text{a}"` builds an interpolated string. It stands in until the standard library formats values, and then it is replaced.
- A local function is named after its owner, `Owner.Local`. When one owner has several local functions with the same name, the second is `Owner.Local#2`, the third `Owner.Local#3`, in the order of the text.

After a change to the HIR or the MIR, update the snapshots and review the diff:

```sh
cargo test -p mora-testkit --test run -- --bless [filter...]
```

`--bless` writes both snapshots, and removes one that is empty, instead of comparing. Filters run only the cases whose path contains one of them.
