//! Floating-point operations compiled code calls instead of inlining.

/// `a % b` for `double`: the remainder of the division truncated toward
/// zero, which takes the sign of `a`.
#[unsafe(no_mangle)]
pub extern "C" fn mora_rt_fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// `a % b` for `float`.
#[unsafe(no_mangle)]
pub extern "C" fn mora_rt_fmodf(a: f32, b: f32) -> f32 {
    a % b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmod_matches_the_remainder_operator() {
        assert_eq!(mora_rt_fmod(5.5, 2.0), 1.5);
        assert_eq!(mora_rt_fmodf(-5.5, 2.0), -1.5);
    }
}
