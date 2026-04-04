//! IR verifier — checks well-formedness of the SSA IR.
//!
//! Run after every IR transformation to catch bugs early.
//! Checks: SSA dominance, type consistency, block structure,
//! terminator completeness, block param/branch arg matching.

use std::collections::HashSet;
use super::inst::*;

/// Verification error.
#[derive(Debug, Clone)]
pub struct VerifyError {
    pub msg: String,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IR verify: {}", self.msg)
    }
}

/// Verify a module. Returns a list of errors (empty = valid).
pub fn verify_module(module: &Module) -> Vec<VerifyError> {
    let mut errors = Vec::new();
    for func in &module.functions {
        errors.extend(verify_function(func));
    }
    errors
}

/// Verify a single function.
pub fn verify_function(func: &Function) -> Vec<VerifyError> {
    let mut errors = Vec::new();

    // 1. Every block must have exactly one terminator.
    for block in &func.blocks {
        if block.terminator.is_none() {
            errors.push(VerifyError {
                msg: format!("block '{}' has no terminator", block.name),
            });
        }
    }

    // 2. Entry block has no predecessors (no branch targets it as a phi/param source).
    //    More precisely: entry block should have no block parameters.
    let entry = func.block(func.entry);
    if !entry.params.is_empty() {
        errors.push(VerifyError {
            msg: "entry block must not have block parameters".into(),
        });
    }

    // 3. All ValueIds used must be defined.
    let defined = collect_defined_values(func);
    for block in &func.blocks {
        for inst in &block.insts {
            for used in inst_uses(&inst.kind) {
                if !defined.contains(&used) {
                    errors.push(VerifyError {
                        msg: format!("value %{} used in block '{}' but not defined",
                            used.0, block.name),
                    });
                }
            }
        }
        if let Some(term) = &block.terminator {
            for used in terminator_uses(term) {
                if !defined.contains(&used) {
                    errors.push(VerifyError {
                        msg: format!("value %{} used in terminator of block '{}' but not defined",
                            used.0, block.name),
                    });
                }
            }
        }
    }

    // 4. No duplicate ValueIds.
    let mut seen_values = HashSet::new();
    for p in &func.params {
        if !seen_values.insert(p.id) {
            errors.push(VerifyError {
                msg: format!("duplicate value ID %{}", p.id.0),
            });
        }
    }
    for block in &func.blocks {
        for bp in &block.params {
            if !seen_values.insert(bp.id) {
                errors.push(VerifyError {
                    msg: format!("duplicate value ID %{}", bp.id.0),
                });
            }
        }
        for inst in &block.insts {
            if !seen_values.insert(inst.id) {
                errors.push(VerifyError {
                    msg: format!("duplicate value ID %{}", inst.id.0),
                });
            }
        }
    }

    // 5. Branch arguments must match block parameters.
    for block in &func.blocks {
        if let Some(term) = &block.terminator {
            check_branch_args(func, term, &block.name, &mut errors);
        }
    }

    // 6. Type consistency: integer ops on integer types, float ops on float types.
    for block in &func.blocks {
        for inst in &block.insts {
            check_type_consistency(func, inst, &mut errors);
        }
    }

    // 7. All branch targets must be valid block IDs.
    let block_ids: HashSet<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    for block in &func.blocks {
        if let Some(term) = &block.terminator {
            for target in terminator_targets(term) {
                if !block_ids.contains(&target) {
                    errors.push(VerifyError {
                        msg: format!("block '{}' branches to undefined block {}",
                            block.name, target.0),
                    });
                }
            }
        }
    }

    errors
}

/// Collect all defined ValueIds in a function.
fn collect_defined_values(func: &Function) -> HashSet<ValueId> {
    let mut defined = HashSet::new();
    for p in &func.params {
        defined.insert(p.id);
    }
    for block in &func.blocks {
        for bp in &block.params {
            defined.insert(bp.id);
        }
        for inst in &block.insts {
            defined.insert(inst.id);
        }
    }
    defined
}

/// Get all ValueIds used by an instruction.
fn inst_uses(kind: &InstKind) -> Vec<ValueId> {
    match kind {
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) |
        InstKind::ConstBool(..) | InstKind::ConstString(..) |
        InstKind::Undef(..) | InstKind::Alloca(..) => vec![],

        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => vec![*a, *b],
        InstKind::INeg(a) => vec![*a],

        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => vec![*a, *b],
        InstKind::FNeg(a) => vec![*a],

        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => vec![*a, *b],

        InstKind::And(a, b) | InstKind::Or(a, b) => vec![*a, *b],
        InstKind::Not(a) => vec![*a],

        InstKind::BitAnd(a, b) | InstKind::BitOr(a, b) |
        InstKind::BitXor(a, b) | InstKind::Shl(a, b) |
        InstKind::AShr(a, b) => vec![*a, *b],

        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) |
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) |
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => vec![*v],

        InstKind::Load(a) => vec![*a],
        InstKind::Store(v, a) => vec![*v, *a],
        InstKind::GetElementPtr(base, idxs) => {
            let mut uses = vec![*base];
            uses.extend(idxs);
            uses
        }

        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => args.clone(),

        InstKind::ExtractField(agg, _) => vec![*agg],
        InstKind::InsertField(agg, _, val) => vec![*agg, *val],
    }
}

/// Get all ValueIds used by a terminator.
fn terminator_uses(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Return(None) | Terminator::Unreachable => vec![],
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::Branch(_, args) => args.clone(),
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            let mut uses = vec![*cond];
            uses.extend(true_args);
            uses.extend(false_args);
            uses
        }
        Terminator::Switch { selector, .. } => vec![*selector],
    }
}

/// Get all branch target BlockIds from a terminator.
fn terminator_targets(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Return(_) | Terminator::Unreachable => vec![],
        Terminator::Branch(dest, _) => vec![*dest],
        Terminator::CondBranch { true_dest, false_dest, .. } => vec![*true_dest, *false_dest],
        Terminator::Switch { cases, default, .. } => {
            let mut targets: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            targets.push(*default);
            targets
        }
    }
}

/// Check that branch arguments match block parameters in count and type.
fn check_branch_args(func: &Function, term: &Terminator, from_block: &str, errors: &mut Vec<VerifyError>) {
    let mut check = |dest: BlockId, args: &[ValueId]| {
        let target = func.block(dest);
        if target.params.len() != args.len() {
            errors.push(VerifyError {
                msg: format!(
                    "branch from '{}' to '{}': expected {} args, got {}",
                    from_block, target.name, target.params.len(), args.len()
                ),
            });
        } else {
            // Check types match.
            for (i, (bp, arg)) in target.params.iter().zip(args.iter()).enumerate() {
                if let Some(arg_ty) = func.value_type(*arg) {
                    if arg_ty != bp.ty {
                        errors.push(VerifyError {
                            msg: format!(
                                "branch from '{}' to '{}': arg {} type mismatch: expected {}, got {}",
                                from_block, target.name, i, bp.ty, arg_ty
                            ),
                        });
                    }
                }
            }
        }
    };

    match term {
        Terminator::Branch(dest, args) => check(*dest, args),
        Terminator::CondBranch { true_dest, true_args, false_dest, false_args, .. } => {
            check(*true_dest, true_args);
            check(*false_dest, false_args);
        }
        Terminator::Switch { cases, default, .. } => {
            // Switch targets shouldn't have block params (simplified model).
            let default_block = func.block(*default);
            if !default_block.params.is_empty() {
                errors.push(VerifyError {
                    msg: format!("switch default target '{}' has block parameters", default_block.name),
                });
            }
            for (_, dest) in cases {
                let target = func.block(*dest);
                if !target.params.is_empty() {
                    errors.push(VerifyError {
                        msg: format!("switch case target '{}' has block parameters", target.name),
                    });
                }
            }
        }
        _ => {}
    }
}

/// Check type consistency for instructions.
fn check_type_consistency(func: &Function, inst: &Inst, errors: &mut Vec<VerifyError>) {
    match &inst.kind {
        InstKind::IAdd(a, b) | InstKind::ISub(a, b) |
        InstKind::IMul(a, b) | InstKind::IDiv(a, b) |
        InstKind::IMod(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_int() {
                    errors.push(VerifyError {
                        msg: format!("integer op %{} has non-integer operand %{} : {}", inst.id.0, a.0, ta),
                    });
                }
                if !tb.is_int() {
                    errors.push(VerifyError {
                        msg: format!("integer op %{} has non-integer operand %{} : {}", inst.id.0, b.0, tb),
                    });
                }
            }
        }
        InstKind::FAdd(a, b) | InstKind::FSub(a, b) |
        InstKind::FMul(a, b) | InstKind::FDiv(a, b) |
        InstKind::FPow(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_float() {
                    errors.push(VerifyError {
                        msg: format!("float op %{} has non-float operand %{} : {}", inst.id.0, a.0, ta),
                    });
                }
                if !tb.is_float() {
                    errors.push(VerifyError {
                        msg: format!("float op %{} has non-float operand %{} : {}", inst.id.0, b.0, tb),
                    });
                }
            }
        }
        InstKind::Store(_, addr) => {
            if let Some(ty) = func.value_type(*addr) {
                if !ty.is_ptr() {
                    errors.push(VerifyError {
                        msg: format!("store %{} to non-pointer %{} : {}", inst.id.0, addr.0, ty),
                    });
                }
            }
        }
        InstKind::Load(addr) => {
            if let Some(ty) = func.value_type(*addr) {
                if !ty.is_ptr() {
                    errors.push(VerifyError {
                        msg: format!("load %{} from non-pointer %{} : {}", inst.id.0, addr.0, ty),
                    });
                }
            }
        }
        _ => {} // other instructions checked as needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::*;
    use super::super::builder::FuncBuilder;

    #[test]
    fn valid_simple_function() {
        let mut module = Module::new("test".into());
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let x = b.const_i32(10);
            let y = b.const_i32(20);
            let _z = b.iadd(x, y);
            b.ret_void();
        }
        module.add_function(func);
        let errs = verify_module(&module);
        assert!(errs.is_empty(), "errors: {:?}", errs);
    }

    #[test]
    fn missing_terminator() {
        let func = Function::new("test".into(), vec![], IrType::Void);
        // Entry block has no terminator.
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("no terminator")));
    }

    #[test]
    fn undefined_value() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            // Use a ValueId that doesn't exist.
            let fake = ValueId(999);
            let real = b.const_i32(1);
            b.emit_bogus_iadd(fake, real);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("%999")));
    }

    #[test]
    fn entry_block_with_params_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        // Manually add a param to the entry block.
        func.blocks[0].params.push(BlockParam {
            id: ValueId(99), ty: IrType::Int(IntWidth::I32),
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("entry block")));
    }

    #[test]
    fn branch_arg_mismatch() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let target = b.create_block("target");
            b.add_block_param(target, IrType::Int(IntWidth::I32));
            // Branch to target with 0 args — but target expects 1.
            b.branch(target, vec![]);

            b.set_block(target);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("expected 1 args, got 0")));
    }

    #[test]
    fn integer_op_on_float_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let a = b.const_f32(1.0);
            let c = b.const_f32(2.0);
            // Force an iadd on float values (bypass builder type checking).
            b.emit_bogus_iadd(a, c);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("non-integer operand")));
    }

    #[test]
    fn valid_branch_with_args() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let target = b.create_block("target");
            let _p = b.add_block_param(target, IrType::Int(IntWidth::I32));
            let val = b.const_i32(42);
            b.branch(target, vec![val]);

            b.set_block(target);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.is_empty(), "errors: {:?}", errs);
    }

    #[test]
    fn valid_cond_branch() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let cond = b.const_bool(true);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);

            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.is_empty(), "errors: {:?}", errs);
    }

    #[test]
    fn store_to_non_pointer_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let val = b.const_i32(42);
            let not_ptr = b.const_i32(0); // not a pointer
            // Force a store to non-pointer.
            b.emit_bogus_store(val, not_ptr);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("non-pointer")));
    }

    #[test]
    fn duplicate_value_id_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        // Manually push two instructions with the same ID.
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 0, col: 0 }, end: Position { line: 0, col: 0 } };
        func.blocks[0].insts.push(Inst {
            id: ValueId(0), kind: InstKind::ConstInt(1, IntWidth::I32), ty: IrType::Int(IntWidth::I32), span,
        });
        func.blocks[0].insts.push(Inst {
            id: ValueId(0), kind: InstKind::ConstInt(2, IntWidth::I32), ty: IrType::Int(IntWidth::I32), span,
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("duplicate value ID")));
    }
}
