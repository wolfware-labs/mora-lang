//! Runs one Mora program through the JIT in its own process, so that a panic
//! in the program, which exits the process, ends only that run.

fn main() {
    mora_testkit::runner::main()
}
