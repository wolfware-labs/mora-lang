# Run tests

Each `.mr` file here is a program (§20.1). Its `// expect:` lines give its standard output, one line each, and an optional `// exit:` line gives its exit code, 0 by default:

```mora
// expect: 3
Console.WriteLine(1 + 2);
```

The harness first compiles the program, which must report no error, and compares the text form of its HIR with the `.hir` file beside it. Then it builds and runs the program through every backend the compiler has, the JIT and an AOT build, and compares each run with the expected output. While the compiler has no backend, compiling and the HIR snapshot are the whole test.

In the HIR, a local is `name#n`, and a name that is not a local is followed by what it names in braces, as `Console{external}` or `X{field App.Point.X}`. An operand that is itself an operation is in parentheses. After a change to the HIR, update the snapshots and review the diff:

```sh
cargo test -p mora-testkit --test run -- --bless [filter...]
```

Filters run only the cases whose path contains one of them.
