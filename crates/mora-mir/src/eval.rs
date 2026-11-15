//! Constant evaluation (§14.6): a MIR interpreter that computes the values
//! of constants at compile time.

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the interpreter that calls it comes next")
)]
mod arith;
