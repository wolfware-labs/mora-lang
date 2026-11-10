# Run tests

Each `.mr` file here is a program (§20.1). Its `// expect:` lines give its standard output, one line each, and an optional `// exit:` line gives its exit code, 0 by default:

```mora
// expect: 3
Console.WriteLine(1 + 2);
```

The harness first compiles the program, which must report no error, and compares the text form of its HIR and of its MIR with the `.hir` and `.mir` files beside it. Then it builds and runs the program through every backend the compiler has, the JIT and an AOT build, and compares each run with the expected output. While the compiler has no backend, compiling and the two snapshots are the whole test. A program that reports errors has neither snapshot.

In the HIR, a local is `name#n`, and a name that is not a local is followed by what it names in braces, as `Console{external}` or `X{field App.Point.X}`. An operand that is itself an operation is in parentheses.

The MIR is one function per body, each a list of locals and numbered basic blocks of statements ending in a terminator. A local keeps its HIR number (`x#0`), an unnamed temporary is `_n`, and every statement ends in `// line:col`, the place in the source it comes from. The layout is described in the documentation of the `dump` module of `mora-mir`.

After a change to the HIR or the MIR, update the snapshots and review the diff:

```sh
cargo test -p mora-testkit --test run -- --bless [filter...]
```

`--bless` writes both snapshots, and removes one that is empty, instead of comparing. Filters run only the cases whose path contains one of them.
