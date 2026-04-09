//! Call resolution pass.
//!
//! Rewrites `Call(FuncRef::External(name), args)` to
//! `Call(FuncRef::Internal(idx), args)` when `name` matches a
//! function defined in the same module. This enables the inliner
//! to find the callee's IR body.
//!
//! Runs as the first pass at O1+ (before Mem2Reg).

use std::collections::HashMap;
use crate::ir::inst::*;
use super::pass::Pass;

pub struct CallResolve;

impl Pass for CallResolve {
    fn name(&self) -> &'static str { "call-resolve" }

    fn run(&self, module: &mut Module) -> bool {
        // Build name → index mapping for all functions in the module.
        let name_to_idx: HashMap<String, u32> = module.functions.iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i as u32))
            .collect();

        let mut changed = false;

        for func in &mut module.functions {
            for block in &mut func.blocks {
                for inst in &mut block.insts {
                    match &mut inst.kind {
                        InstKind::Call(ref mut func_ref, _) => {
                            if let FuncRef::External(name) = func_ref {
                                if let Some(&idx) = name_to_idx.get(name.as_str()) {
                                    *func_ref = FuncRef::Internal(idx);
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::ir::inst::*;
    use crate::opt::pass::Pass;

    #[test]
    fn resolves_internal_calls() {
        let mut m = Module::new("test".into());

        // Callee: function "double_it"
        let mut callee = Function::new("double_it".into(), vec![], IrType::Int(IntWidth::I32));
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(None));
        m.add_function(callee);

        // Caller: calls "double_it" via External ref
        let mut caller = Function::new("main".into(), vec![], IrType::Void);
        let call_id = caller.next_value_id();
        caller.register_type(call_id, IrType::Int(IntWidth::I32));
        caller.block_mut(caller.entry).insts.push(Inst {
            id: call_id,
            ty: IrType::Int(IntWidth::I32),
            span: crate::lexer::Span {
                file_id: 0,
                start: crate::lexer::Position { line: 0, col: 0 },
                end: crate::lexer::Position { line: 0, col: 0 },
            },
            kind: InstKind::Call(FuncRef::External("double_it".into()), vec![]),
        });
        caller.block_mut(caller.entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        let pass = CallResolve;
        let changed = pass.run(&mut m);
        assert!(changed, "should resolve the internal call");

        // Verify the call was rewritten.
        let caller_func = &m.functions[1];
        let call_inst = &caller_func.blocks[0].insts[0];
        match &call_inst.kind {
            InstKind::Call(FuncRef::Internal(idx), _) => {
                assert_eq!(*idx, 0, "should point to function index 0 (double_it)");
            }
            other => panic!("expected Internal call, got {:?}", other),
        }
    }

    #[test]
    fn leaves_external_calls_unchanged() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("main".into(), vec![], IrType::Void);
        let call_id = f.next_value_id();
        f.register_type(call_id, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: call_id,
            ty: IrType::Void,
            span: crate::lexer::Span {
                file_id: 0,
                start: crate::lexer::Position { line: 0, col: 0 },
                end: crate::lexer::Position { line: 0, col: 0 },
            },
            kind: InstKind::Call(FuncRef::External("afs_write_int".into()), vec![]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = CallResolve;
        let changed = pass.run(&mut m);
        assert!(!changed, "runtime calls should stay External");
    }
}
