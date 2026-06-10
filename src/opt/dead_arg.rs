//! Dead-argument elimination for contained procedures.
//!
//! Removes unused parameters from internal-only functions and trims the
//! matching argument slots from all `Call(Internal, ...)` sites.

use crate::ir::inst::*;

use super::pass::Pass;

pub struct DeadArgElim;

impl Pass for DeadArgElim {
    fn name(&self) -> &'static str {
        "dead-arg-elim"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut live_masks: Vec<Vec<bool>> = module
            .functions
            .iter()
            .map(|func| vec![false; func.params.len()])
            .collect();

        let mut changed = true;
        while changed {
            changed = false;
            for (func_idx, func) in module.functions.iter().enumerate() {
                if !func.internal_only || func.params.is_empty() {
                    continue;
                }
                for (param_idx, param) in func.params.iter().enumerate() {
                    let live = param_has_observable_use(module, func_idx, param.id, &live_masks);
                    if live_masks[func_idx][param_idx] != live {
                        live_masks[func_idx][param_idx] = live;
                        changed = true;
                    }
                }
            }
        }

        let mut removed_any = false;
        for (idx, func) in module.functions.iter_mut().enumerate() {
            if !func.internal_only || func.params.is_empty() {
                continue;
            }
            let keep = &live_masks[idx];
            if keep.iter().all(|&slot| slot) {
                continue;
            }
            let old_params = std::mem::take(&mut func.params);
            func.params = old_params
                .into_iter()
                .zip(keep.iter().copied())
                .filter_map(|(param, keep_slot)| keep_slot.then_some(param))
                .collect();
            removed_any = true;
        }

        if !removed_any {
            return false;
        }

        let internal_only: Vec<bool> = module
            .functions
            .iter()
            .map(|func| func.internal_only)
            .collect();

        for func in &mut module.functions {
            for block in &mut func.blocks {
                for inst in &mut block.insts {
                    let InstKind::Call(FuncRef::Internal(callee_idx), args) = &mut inst.kind else {
                        continue;
                    };
                    if !internal_only
                        .get(*callee_idx as usize)
                        .copied()
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let Some(keep) = live_masks.get(*callee_idx as usize) else {
                        continue;
                    };
                    let old_args = std::mem::take(args);
                    *args = old_args
                        .into_iter()
                        .zip(keep.iter().copied())
                        .filter_map(|(arg, keep_slot)| keep_slot.then_some(arg))
                        .collect();
                }
            }
        }

        true
    }
}

fn param_has_observable_use(
    module: &Module,
    func_idx: usize,
    param_id: ValueId,
    live_masks: &[Vec<bool>],
) -> bool {
    let func = &module.functions[func_idx];
    for block in &func.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::Call(FuncRef::Internal(callee_idx), args) => {
                    for (arg_idx, arg) in args.iter().enumerate() {
                        if *arg != param_id {
                            continue;
                        }
                        let Some(callee) = module.functions.get(*callee_idx as usize) else {
                            return true;
                        };
                        if !callee.internal_only {
                            return true;
                        }
                        let Some(callee_live) = live_masks
                            .get(*callee_idx as usize)
                            .and_then(|mask| mask.get(arg_idx))
                        else {
                            return true;
                        };
                        if *callee_live {
                            return true;
                        }
                    }
                }
                kind => {
                    if inst_uses_param(kind, param_id) {
                        return true;
                    }
                }
            }
        }
        if block_terminator_uses_param(&block.terminator, param_id) {
            return true;
        }
    }
    false
}

fn inst_uses_param(kind: &InstKind, param_id: ValueId) -> bool {
    match kind {
        InstKind::ConstInt(..)
        | InstKind::ConstFloat(..)
        | InstKind::ConstBool(..)
        | InstKind::ConstString(..)
        | InstKind::Undef(..)
        | InstKind::Alloca(..)
        | InstKind::GlobalAddr(..) => false,

        InstKind::IAdd(a, b)
        | InstKind::ISub(a, b)
        | InstKind::IMul(a, b)
        | InstKind::IDiv(a, b)
        | InstKind::IMod(a, b)
        | InstKind::FAdd(a, b)
        | InstKind::FSub(a, b)
        | InstKind::FMul(a, b)
        | InstKind::FDiv(a, b)
        | InstKind::FPow(a, b)
        | InstKind::ICmp(_, a, b)
        | InstKind::FCmp(_, a, b)
        | InstKind::And(a, b)
        | InstKind::Or(a, b)
        | InstKind::BitAnd(a, b)
        | InstKind::BitOr(a, b)
        | InstKind::BitXor(a, b)
        | InstKind::Shl(a, b)
        | InstKind::LShr(a, b)
        | InstKind::AShr(a, b)
        | InstKind::Store(a, b) => *a == param_id || *b == param_id,

        InstKind::INeg(a)
        | InstKind::FNeg(a)
        | InstKind::FAbs(a)
        | InstKind::FSqrt(a)
        | InstKind::Not(a)
        | InstKind::BitNot(a)
        | InstKind::CountLeadingZeros(a)
        | InstKind::CountTrailingZeros(a)
        | InstKind::PopCount(a)
        | InstKind::IntToFloat(a, _)
        | InstKind::FloatToInt(a, _)
        | InstKind::FloatExtend(a, _)
        | InstKind::FloatTrunc(a, _)
        | InstKind::IntExtend(a, _, _)
        | InstKind::IntTrunc(a, _)
        | InstKind::PtrToInt(a)
        | InstKind::IntToPtr(a, _)
        | InstKind::Load(a)
        | InstKind::ExtractField(a, _) => *a == param_id,

        InstKind::Select(c, t, f) => *c == param_id || *t == param_id || *f == param_id,
        InstKind::GetElementPtr(base, idxs) => *base == param_id || idxs.contains(&param_id),
        InstKind::RuntimeCall(_, args) | InstKind::Call(FuncRef::External(_), args) => {
            args.contains(&param_id)
        }
        InstKind::Call(FuncRef::Indirect(target), args) => {
            *target == param_id || args.contains(&param_id)
        }
        InstKind::InsertField(agg, _, val) => *agg == param_id || *val == param_id,
        InstKind::Call(FuncRef::Internal(_), _) => false,

        // Vector ops — fall through to the generic operand walk so we
        // don't accidentally treat a vector inst as not using the
        // param if a future vectorizer makes it consume one.
        kind @ (InstKind::VAdd(..)
        | InstKind::VSub(..)
        | InstKind::VMul(..)
        | InstKind::VDiv(..)
        | InstKind::VNeg(..)
        | InstKind::VAbs(..)
        | InstKind::VSqrt(..)
        | InstKind::VFma(..)
        | InstKind::VSelect(..)
        | InstKind::VMin(..)
        | InstKind::VMax(..)
        | InstKind::VICmp(..)
        | InstKind::VFCmp(..)
        | InstKind::VLoad(..)
        | InstKind::VStore(..)
        | InstKind::VBitcast(..)
        | InstKind::VExtract(..)
        | InstKind::VInsert(..)
        | InstKind::VBroadcast(..)
        | InstKind::VReduceSum(..)
        | InstKind::VReduceMin(..)
        | InstKind::VReduceMax(..)) => crate::ir::walk::inst_uses(kind).contains(&param_id),
    }
}

fn block_terminator_uses_param(term: &Option<Terminator>, param_id: ValueId) -> bool {
    match term {
        Some(Terminator::Return(Some(v))) => *v == param_id,
        Some(Terminator::Branch(_, args)) => args.contains(&param_id),
        Some(Terminator::CondBranch {
            cond,
            true_args,
            false_args,
            ..
        }) => *cond == param_id || true_args.contains(&param_id) || false_args.contains(&param_id),
        Some(Terminator::Switch { selector, .. }) => *selector == param_id,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id,
            kind,
            ty: ty.clone(),
            span: span(),
        });
        f.register_type(id, ty);
        id
    }

    #[test]
    fn trims_unused_param_and_call_arg_for_internal_only_function() {
        let mut module = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let params = vec![
            Param {
                name: "x".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "unused".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut callee = Function::new("helper".into(), params, IrType::Int(IntWidth::I32));
        callee.internal_only = true;
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(ValueId(0))));
        module.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let a = push(
            &mut caller,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut caller,
            InstKind::ConstInt(9, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![a, b]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(call)));
        module.add_function(caller);

        assert!(DeadArgElim.run(&mut module));
        assert_eq!(module.functions[0].params.len(), 1);

        let call_args = match &module.functions[1].blocks[0].insts[2].kind {
            InstKind::Call(FuncRef::Internal(0), args) => args,
            other => panic!("expected internal call, got {:?}", other),
        };
        assert_eq!(call_args, &vec![a]);
    }

    #[test]
    fn preserves_abi_for_non_internal_function() {
        let mut module = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let params = vec![
            Param {
                name: "x".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "unused".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut func = Function::new("exported".into(), params, IrType::Int(IntWidth::I32));
        let entry = func.entry;
        func.block_mut(entry).terminator = Some(Terminator::Return(Some(ValueId(0))));
        module.add_function(func);

        assert!(!DeadArgElim.run(&mut module));
        assert_eq!(module.functions[0].params.len(), 2);
    }

    #[test]
    fn preserves_non_internal_call_args_when_trimming_other_function() {
        let mut module = Module::new("t".into(), crate::target::TargetLayout::LP64);

        let exported_params = vec![
            Param {
                name: "lhs".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "rhs".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut exported = Function::new(
            "module_proc".into(),
            exported_params,
            IrType::Int(IntWidth::I32),
        );
        let exported_entry = exported.entry;
        exported.block_mut(exported_entry).terminator = Some(Terminator::Return(Some(ValueId(0))));
        module.add_function(exported);

        let helper_params = vec![Param {
            name: "unused".into(),
            ty: IrType::Int(IntWidth::I32),
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut helper = Function::new("contained_helper".into(), helper_params, IrType::Void);
        helper.internal_only = true;
        let helper_entry = helper.entry;
        helper.block_mut(helper_entry).terminator = Some(Terminator::Return(None));
        module.add_function(helper);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let lhs = push(
            &mut caller,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let rhs = push(
            &mut caller,
            InstKind::ConstInt(9, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let exported_call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![lhs, rhs]),
            IrType::Int(IntWidth::I32),
        );
        push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(1), vec![rhs]),
            IrType::Void,
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(exported_call)));
        module.add_function(caller);

        assert!(DeadArgElim.run(&mut module));
        assert_eq!(module.functions[0].params.len(), 2);
        assert_eq!(module.functions[1].params.len(), 0);

        let exported_args = match &module.functions[2].blocks[0].insts[2].kind {
            InstKind::Call(FuncRef::Internal(0), args) => args,
            other => panic!(
                "expected call to exported internal function, got {:?}",
                other
            ),
        };
        assert_eq!(
            exported_args,
            &vec![lhs, rhs],
            "non-internal module-procedure call arguments are ABI, not dead-arg slots"
        );

        let helper_args = match &module.functions[2].blocks[0].insts[3].kind {
            InstKind::Call(FuncRef::Internal(1), args) => args,
            other => panic!("expected call to internal-only helper, got {:?}", other),
        };
        assert!(helper_args.is_empty());
    }
}
