//! The allocation entry point that compiled code calls (§13.1).

use crate::pal;

/// Allocates `size` bytes aligned to `align`, or returns null when the
/// platform is out of memory.
///
/// The allocation counters of §13.1 are not kept yet.
///
/// # Safety
///
/// `size` is not zero and `align` is a power of two.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mora_rt_alloc(size: usize, align: usize) -> *mut u8 {
    // SAFETY: the caller upholds the platform's preconditions.
    match unsafe { pal::alloc(size, align) } {
        Some(block) => block.as_ptr(),
        None => core::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn mora_rt_alloc_returns_aligned_memory() {
        // SAFETY: a non-zero size and a power-of-two alignment; the block is
        // freed with the layout it was allocated with.
        unsafe {
            let block = mora_rt_alloc(24, 8);
            assert!(!block.is_null());
            assert_eq!(block as usize % 8, 0);
            pal::free(NonNull::new(block).unwrap(), 24, 8);
        }
    }

    #[test]
    fn mora_rt_alloc_returns_null_when_out_of_memory() {
        // SAFETY: the size is non-zero and the alignment a power of two;
        // the fake platform refuses a request this large.
        assert!(unsafe { mora_rt_alloc(usize::MAX - 4, 8) }.is_null());
    }
}
