//! Dead function elimination.
//!
//! After inlining, some contained functions may have zero remaining
//! callers. This pass removes them from the module, reducing code
//! size and symbol table pollution.
//!
//! The lowered program entry function (`__prog_*`) is always kept.
//! When no program body exists, function index 0 is kept as the
//! conservative fallback root. Functions called via External refs
//! (runtime, external linkage) are also kept since the call might come
//! from outside.

use super::pass::Pass;
use crate::ir::inst::*;
use std::collections::HashSet;

pub struct DeadFuncElim;

impl Pass for DeadFuncElim {
    fn name(&self) -> &'static str {
        "dead-func-elim"
    }

    fn run(&self, module: &mut Module) -> bool {
        let n = module.functions.len();
        if n <= 1 {
            return false;
        }

        // Collect all function indices that are referenced by Internal calls.
        let mut referenced: HashSet<u32> = HashSet::new();
        let mut kept_program_root = false;
        for (idx, func) in module.functions.iter().enumerate() {
            if func.name.starts_with("__prog_") {
                referenced.insert(idx as u32);
                kept_program_root = true;
            }
        }
        if !kept_program_root {
            referenced.insert(0);
        }

        for func in &module.functions {
            for block in &func.blocks {
                for inst in &block.insts {
                    if let InstKind::Call(FuncRef::Internal(idx), _) = &inst.kind {
                        referenced.insert(*idx);
                    }
                }
            }
        }

        // Also keep functions that might be called externally (by name).
        // Collect all External call names.
        let mut external_names: HashSet<String> = HashSet::new();
        for func in &module.functions {
            for block in &func.blocks {
                for inst in &block.insts {
                    if let InstKind::Call(FuncRef::External(name), _) = &inst.kind {
                        external_names.insert(name.clone());
                    }
                }
            }
        }
        // Keep functions whose names match External calls.
        for (i, func) in module.functions.iter().enumerate() {
            if external_names.contains(&func.name) {
                referenced.insert(i as u32);
            }
        }

        // Keep all module-visible functions — they may be called from other
        // translation units even if nobody calls them in this TU.
        for (i, func) in module.functions.iter().enumerate() {
            if !func.internal_only {
                referenced.insert(i as u32);
            }
        }

        // Remove unreferenced functions (iterate in reverse to preserve indices).
        let dead: Vec<usize> = (0..n)
            .filter(|i| !referenced.contains(&(*i as u32)))
            .collect();

        if dead.is_empty() {
            return false;
        }

        // Remove in reverse order so indices stay valid.
        for &idx in dead.iter().rev() {
            module.functions.remove(idx);
        }

        // After removing functions, Internal(idx) references in remaining
        // functions may have stale indices. Rebuild the index mapping.
        // Build old_idx → new_idx for surviving functions.
        let mut idx_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut new_idx = 0u32;
        for old_idx in 0..n as u32 {
            if referenced.contains(&old_idx) {
                idx_map.insert(old_idx, new_idx);
                new_idx += 1;
            }
        }

        // Remap all Internal call references.
        for func in &mut module.functions {
            for block in &mut func.blocks {
                for inst in &mut block.insts {
                    if let InstKind::Call(FuncRef::Internal(ref mut idx), _) = inst.kind {
                        if let Some(&new) = idx_map.get(idx) {
                            *idx = new;
                        }
                    }
                }
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    #[test]
    fn removes_uncalled_internal_function() {
        let mut m = Module::new("test".into());
        // func 0: main (entry, kept)
        let mut main_f = Function::new("main".into(), vec![], IrType::Void);
        main_f.block_mut(main_f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(main_f);
        // func 1: dead internal function (never called, internal_only)
        let mut dead_f = Function::new("dead".into(), vec![], IrType::Void);
        dead_f.internal_only = true;
        dead_f.block_mut(dead_f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(dead_f);

        assert_eq!(m.functions.len(), 2);
        let pass = DeadFuncElim;
        let changed = pass.run(&mut m);
        assert!(changed);
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "main");
    }

    #[test]
    fn keeps_called_function() {
        let mut m = Module::new("test".into());
        // func 0: caller (entry, always kept) — calls callee at index 1
        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        let cid = caller.next_value_id();
        caller.register_type(cid, IrType::Void);
        caller.block_mut(caller.entry).insts.push(Inst {
            id: cid,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Call(FuncRef::Internal(1), vec![]),
        });
        caller.block_mut(caller.entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);
        // func 1: callee — called by func 0, should be kept
        let mut callee = Function::new("callee".into(), vec![], IrType::Void);
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(None));
        m.add_function(callee);

        let pass = DeadFuncElim;
        let changed = pass.run(&mut m);
        assert!(!changed, "both functions are referenced");
        assert_eq!(m.functions.len(), 2);
    }

    #[test]
    fn keeps_module_visible_function_even_without_callers() {
        let mut m = Module::new("test".into());
        // func 0: __prog_entry (kept as program root)
        let mut prog = Function::new("__prog_entry".into(), vec![], IrType::Void);
        prog.block_mut(prog.entry).terminator = Some(Terminator::Return(None));
        m.add_function(prog);
        // func 1: module-visible function, no internal callers
        let mut visible = Function::new("get".into(), vec![], IrType::Void);
        visible.internal_only = false; // module-public
        visible.block_mut(visible.entry).terminator = Some(Terminator::Return(None));
        m.add_function(visible);
        // func 2: internal-only function, no callers (should be removed)
        let mut internal = Function::new("helper".into(), vec![], IrType::Void);
        internal.internal_only = true;
        internal.block_mut(internal.entry).terminator = Some(Terminator::Return(None));
        m.add_function(internal);

        let pass = DeadFuncElim;
        let changed = pass.run(&mut m);
        assert!(changed, "internal-only helper should be removed");
        assert_eq!(m.functions.len(), 2);
        assert_eq!(m.functions[0].name, "__prog_entry");
        assert_eq!(m.functions[1].name, "get");
    }

    #[test]
    fn keeps_program_body_even_when_helper_is_first() {
        let mut m = Module::new("test".into());

        let mut helper = Function::new("helper".into(), vec![], IrType::Void);
        helper.internal_only = true;
        helper.block_mut(helper.entry).terminator = Some(Terminator::Return(None));
        m.add_function(helper);

        let mut prog = Function::new("__prog_entry".into(), vec![], IrType::Void);
        prog.block_mut(prog.entry).terminator = Some(Terminator::Return(None));
        m.add_function(prog);

        let pass = DeadFuncElim;
        let changed = pass.run(&mut m);
        assert!(
            changed,
            "dead helper should be removed while keeping program body"
        );
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "__prog_entry");
    }
}
