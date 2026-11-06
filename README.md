# Mora

[![Coverage](https://img.shields.io/codecov/c/github/wolfware-labs/mora-lang?logo=codecov&label=coverage)](https://codecov.io/gh/wolfware-labs/mora-lang)
[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://app.codspeed.io/wolfware-labs/mora-lang?utm_source=badge)

Mora is a programming language with familiar curly-brace syntax, memory managed by reference counting and single ownership instead of a garbage collector, and compilation to one native executable. Anyone who knows a mainstream curly-brace language can read it, but it keeps only what fits its own model: no exceptions, no inheritance, no `null` references. The compiler, runtime, language server and package tool are written in Rust.

```mora
Console.WriteLine("Hello, world!");
```

## Status

Mora is in design and early development. Nothing here is ready to install.

- [Specification](docs/design/mora.md): the language, its memory model, runtime, standard library and toolchain, as the MVP is built against them.
- [Roadmap](ROADMAP.md): milestones M0 to M13, with every MVP deliverable tracked as a [GitHub issue](https://github.com/wolfware-labs/mora-lang/issues).

The MVP is done when a REST API with JSON bodies, written in Mora, builds into a native executable on Linux, macOS and Windows, and a developer new to Mora can install the toolchain with one command and get diagnostics in VS Code.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Contributions require signing the [Contributor Licence Agreement](CLA.md), and security problems go through [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
