//! A program compiled into the memory of this process.

use std::mem::ManuallyDrop;

use cranelift_jit::JITModule;
use cranelift_module::{FuncOrDataId, Module};

use crate::cranelift::MAIN;

/// A program the JIT compiled, ready to run. Dropping it frees its code.
pub struct JitProgram {
    module: ManuallyDrop<JITModule>,
}

impl JitProgram {
    /// A program whose functions `module` will hold.
    pub(crate) fn new(module: JITModule) -> JitProgram {
        JitProgram {
            module: ManuallyDrop::new(module),
        }
    }

    /// The module, to define functions in.
    pub(crate) fn module_mut(&mut self) -> &mut JITModule {
        &mut self.module
    }

    /// Runs `__mora_main` and returns the exit code.
    pub fn run_main(&self) -> i32 {
        let main = self.function(MAIN);
        // SAFETY: `__mora_main` is defined with the signature `() -> i32` in
        // the host's C convention, which `extern "C"` names, and its code
        // lives as long as `self`, which this call borrows.
        let main = unsafe { std::mem::transmute::<*const u8, extern "C" fn() -> i32>(main) };
        main()
    }

    /// The address of the finalized function `symbol`.
    ///
    /// # Panics
    ///
    /// When the program has no function `symbol`.
    pub(crate) fn function(&self, symbol: &str) -> *const u8 {
        match self.module.get_name(symbol) {
            Some(FuncOrDataId::Func(id)) => self.module.get_finalized_function(id),
            _ => panic!("codegen: the JIT program has no function `{symbol}`"),
        }
    }
}

impl Drop for JitProgram {
    fn drop(&mut self) {
        // SAFETY: `drop` runs once, so the module is taken once, and no
        // pointer into its code outlives `self`: `run_main` borrows `self`
        // for the call, and the test helpers that hand out function pointers
        // keep their program alive while they call them.
        unsafe { ManuallyDrop::take(&mut self.module).free_memory() };
    }
}

#[cfg(test)]
mod tests {
    use crate::Profile;
    use crate::test_util::jit;

    #[test]
    #[should_panic(expected = "the JIT program has no function `_M7MissingE`")]
    fn a_missing_function_is_an_internal_error() {
        jit("Main(): int { return 7; }\n", Profile::Debug).function("_M7MissingE");
    }
}
