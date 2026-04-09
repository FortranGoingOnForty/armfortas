//! Pass trait + PassManager.
//!
//! Every optimization pass implements `Pass` and is registered with a
//! `PassManager`. The manager runs passes to fixpoint, verifying the IR
//! after every pass so that any miscompile is caught immediately.

use crate::ir::inst::Module;
use crate::ir::verify::{verify_module, VerifyError};

/// A single optimization pass.
pub trait Pass {
    /// Human-readable pass name (used in diagnostics + IR dumps).
    fn name(&self) -> &'static str;

    /// Run the pass over the module. Return `true` if the IR was modified.
    fn run(&self, module: &mut Module) -> bool;
}

/// Result of a pass-manager run.
#[derive(Debug)]
pub struct PassRunResult {
    /// Total number of times any pass reported "changed".
    pub change_count: usize,
    /// Number of fixpoint iterations performed.
    pub iterations: usize,
}

/// A linearly-ordered collection of passes that are run to fixpoint.
pub struct PassManager {
    passes: Vec<Box<dyn Pass>>,
    /// Run `verify_module` after every pass and panic on failure.
    /// Always-on in debug builds; can be disabled for benchmarking.
    pub verify_after_each: bool,
    /// Maximum fixpoint iterations before bailing out (defensive).
    pub max_iterations: usize,
}

impl PassManager {
    pub fn new() -> Self {
        Self {
            passes: Vec::new(),
            verify_after_each: true,
            max_iterations: 32,
        }
    }

    /// Add a pass to the end of the pipeline.
    pub fn add(&mut self, pass: Box<dyn Pass>) {
        self.passes.push(pass);
    }

    /// Number of passes registered.
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    #[cfg(test)]
    pub(crate) fn pass_names(&self) -> Vec<&'static str> {
        self.passes.iter().map(|pass| pass.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }

    /// Run all passes to fixpoint over the module.
    ///
    /// Each iteration runs every pass once. We keep iterating as long as
    /// at least one pass reported a change in the previous round, up to
    /// `max_iterations`.
    pub fn run(&self, module: &mut Module) -> PassRunResult {
        let mut change_count = 0usize;
        let mut iterations = 0usize;

        // Verify on entry — bad input shouldn't be blamed on a pass.
        self.verify_or_panic(module, "<entry>");

        let mut changed = true;
        while changed && iterations < self.max_iterations {
            changed = false;
            iterations += 1;
            for pass in &self.passes {
                let pass_changed = pass.run(module);
                if pass_changed {
                    change_count += 1;
                    changed = true;
                    // Rebuild O(1) type caches after IR mutations.
                    for func in &mut module.functions {
                        func.rebuild_type_cache();
                    }
                }
                if self.verify_after_each {
                    self.verify_or_panic(module, pass.name());
                }
            }
        }

        PassRunResult { change_count, iterations }
    }

    fn verify_or_panic(&self, module: &Module, after: &str) {
        if !self.verify_after_each {
            return;
        }
        let errors: Vec<VerifyError> = verify_module(module);
        if !errors.is_empty() {
            let mut msg = format!(
                "IR verifier failed after pass `{}`:\n",
                after
            );
            for e in &errors {
                msg.push_str("  - ");
                msg.push_str(&e.msg);
                msg.push('\n');
            }
            panic!("{}", msg);
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::inst::*;
    use crate::ir::types::IrType;

    /// A pass that does nothing — used to test infrastructure plumbing.
    struct NoopPass;
    impl Pass for NoopPass {
        fn name(&self) -> &'static str { "noop" }
        fn run(&self, _module: &mut Module) -> bool { false }
    }

    /// A pass that claims it changed once, then is idle.
    struct OneShotPass {
        fired: std::cell::Cell<bool>,
    }
    impl Pass for OneShotPass {
        fn name(&self) -> &'static str { "oneshot" }
        fn run(&self, _module: &mut Module) -> bool {
            if self.fired.get() { false } else { self.fired.set(true); true }
        }
    }

    fn empty_module() -> Module {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        // Entry block needs a terminator to be valid.
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        m
    }

    #[test]
    fn empty_pipeline_runs_one_iteration() {
        let pm = PassManager::new();
        let mut m = empty_module();
        let r = pm.run(&mut m);
        assert_eq!(r.change_count, 0);
        // One iteration to discover nothing changed.
        assert_eq!(r.iterations, 1);
    }

    #[test]
    fn noop_pass_terminates() {
        let mut pm = PassManager::new();
        pm.add(Box::new(NoopPass));
        let mut m = empty_module();
        let r = pm.run(&mut m);
        assert_eq!(r.change_count, 0);
        // First iteration runs every pass; since none change, we're done.
        assert_eq!(r.iterations, 1);
    }

    #[test]
    fn oneshot_runs_then_terminates() {
        let mut pm = PassManager::new();
        pm.add(Box::new(OneShotPass { fired: std::cell::Cell::new(false) }));
        let mut m = empty_module();
        let r = pm.run(&mut m);
        assert_eq!(r.change_count, 1);
        // Iter 1 changed → iter 2 stable.
        assert_eq!(r.iterations, 2);
    }
}
