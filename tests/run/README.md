# Run tests

Each `.mr` file here is a program (§20.1). Its `// expect:` lines give its standard output, one line each, its `// stderr:` lines give its standard error the same way (empty when there are none), and an optional `// exit:` line gives its exit code, 0 by default:

```mora
// expect: 3
Console.WriteLine(1 + 2);
```

The program runs in the debug and in the release profile, and every line holds for both. A line that holds for one profile only names it after the marker, as `// expect[release]:`, `// stderr[debug]:` or `// exit[debug]:`. An overflow of `+` panics in a debug build and wraps in a release one (§7.12), so `overflow.mr` states both:

```mora
// expect: before
// exit[debug]: 101
// stderr[debug]: panic at src/Program.mr:8:11: arithmetic overflow: 2147483647 + 1
// expect[release]: -2147483648
```

The harness first compiles the program, which must report no error, and compares the text form of its HIR and of its MIR with the `.hir` and `.mir` files beside it. Then it runs the program through the JIT in each profile, in a child `jit-runner` process because a panic exits the process, and compares its standard output, standard error and exit code with the ones expected in that profile. The AOT build joins the JIT with M1.14. While the compiler has no backend, compiling and the two snapshots are the whole test. A program that reports errors has neither snapshot.

In the HIR, a local is `name#n`, and a name that is not a local is followed by what it names in braces, as `Console{external}` or `X{field App.Point.X}`. An operand that is itself an operation is in parentheses.

The MIR is one function per body (`fn name(params) -> type { ... }`), each a list of typed locals and numbered basic blocks, `bb0`, `bb1` and so on, without gaps. A block is a list of statements ending in one terminator: `goto bbN;`, `switch`, `return;`, a call, or `unreachable;` where control never gets. Every statement ends in `// line:col`, the place in the source it comes from. A local keeps its HIR number (`x#0`), `this` is the receiver and an unnamed temporary is `_n`, where `_0` is the return value. A real constant always has a decimal point (`5.0`), so it reads differently from an integer.

A body that a later milestone of the roadmap lowers, such as a getter, a constructor or a function with a class type in its signature, is one line in place of its function: `// App.Point.Length: not lowered until M2.1`, the owner's name and the milestone.

A constant is one line, `const App.Size: int = 8;`, with the value constant evaluation computed (§14.6); a struct or tuple value prints as the values of its fields. A read of a constant is that value.

- `copy x` reads a place and leaves it usable. `move x` hands it over.
- `&in x` and `&ref x` borrow a place, to read it or to change it. `(*x)` is the place a borrow `x` points to, so a parameter `n: int` is read as `copy (*n#0)`.
- `(T)a` converts a number to `T` as the source wrote it. `(implicit T)a` is a conversion the compiler inserted because every value fits, such as `int` to `long`.
- `call f(args) -> bbN;` calls `f` and continues in `bbN`; `x = call ...` stores the result. `-> !` marks a call that does not return.
- `App.Point{struct App.Point}(a, b)` builds a struct from the values of its instance fields, in the order they are declared; `(a, b)` builds a tuple. `App.Point{struct App.Point}.Y{init}` is the initializer of the field `Y`, called for a field that `new` leaves out.
- `Console.WriteLine{external}` is a function of the standard library, which is not loaded yet, called by the path as written. It is the only external callee here, and it runs through a temporary runtime stand-in until M1.15 (#43).
- `$"text{a}"` builds an interpolated string. It stands in until the standard library formats values (M1.15, #43), and runs through a temporary runtime stand-in until then.
- A local function is named after its owner, `Owner.Local`. When one owner has several local functions with the same name, the second is `Owner.Local#2`, the third `Owner.Local#3`, in the order of the text.

After a change to the HIR or the MIR, update the snapshots and review the diff:

```sh
cargo test -p mora-testkit --test run -- --bless [filter...]
```

`--bless` writes both snapshots, and removes one that is empty, instead of comparing. Filters run only the cases whose path contains one of them.
