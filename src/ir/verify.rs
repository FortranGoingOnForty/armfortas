//! IR verifier — checks well-formedness of the SSA IR.
//!
//! Run after every IR transformation to catch bugs early.
//! Checks: SSA dominance, type consistency, block structure,
//! terminator completeness, block param/branch arg matching.

use std::collections::{HashMap, HashSet};
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

    // 8. SSA dominance: every use must be dominated by its definition.
    check_dominance(func, &mut errors);

    errors
}

/// Compute the dominator set for each block using iterative dataflow.
/// Returns a map from BlockId to the set of BlockIds that dominate it.
fn compute_dominators(func: &Function) -> HashMap<BlockId, HashSet<BlockId>> {
    let all_blocks: HashSet<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    let mut doms: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();

    // Entry block is dominated only by itself.
    let mut entry_set = HashSet::new();
    entry_set.insert(func.entry);
    doms.insert(func.entry, entry_set);

    // All other blocks start dominated by every block.
    for block in &func.blocks {
        if block.id != func.entry {
            doms.insert(block.id, all_blocks.clone());
        }
    }

    // Build predecessor map.
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for block in &func.blocks {
        preds.entry(block.id).or_default();
        if let Some(term) = &block.terminator {
            for target in terminator_targets(term) {
                preds.entry(target).or_default().push(block.id);
            }
        }
    }

    // Iterate until fixed point.
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            if block.id == func.entry { continue; }
            let pred_list = preds.get(&block.id).cloned().unwrap_or_default();
            if pred_list.is_empty() { continue; }

            // dom(B) = {B} ∪ ∩{dom(P) for P in preds(B)}
            let mut new_dom = all_blocks.clone();
            for pred in &pred_list {
                if let Some(pred_doms) = doms.get(pred) {
                    new_dom = new_dom.intersection(pred_doms).copied().collect();
                }
            }
            new_dom.insert(block.id);

            if doms.get(&block.id) != Some(&new_dom) {
                doms.insert(block.id, new_dom);
                changed = true;
            }
        }
    }

    doms
}

/// Map each ValueId to the block where it's defined.
fn value_def_block(func: &Function) -> HashMap<ValueId, BlockId> {
    let mut map = HashMap::new();
    // Function params are defined in the entry block.
    for p in &func.params {
        map.insert(p.id, func.entry);
    }
    for block in &func.blocks {
        for bp in &block.params {
            map.insert(bp.id, block.id);
        }
        for inst in &block.insts {
            map.insert(inst.id, block.id);
        }
    }
    map
}

/// Check SSA dominance: every use of a value must be in a block dominated
/// by the block where the value is defined. For same-block uses, the
/// definition must precede the use in instruction order.
fn check_dominance(func: &Function, errors: &mut Vec<VerifyError>) {
    let doms = compute_dominators(func);
    let def_blocks = value_def_block(func);

    for block in &func.blocks {
        let block_doms = doms.get(&block.id).cloned().unwrap_or_default();

        // Build intra-block ordering: map ValueId → instruction index.
        // Block params have index -1 (always dominate all instructions).
        let mut inst_order: HashMap<ValueId, i32> = HashMap::new();
        for bp in &block.params {
            inst_order.insert(bp.id, -1);
        }
        for (idx, inst) in block.insts.iter().enumerate() {
            inst_order.insert(inst.id, idx as i32);
        }

        let check_use = |val: ValueId, use_idx: i32, errors: &mut Vec<VerifyError>| {
            if let Some(def_block) = def_blocks.get(&val) {
                if *def_block == block.id {
                    // Same-block: def must precede use in instruction order.
                    if let Some(&def_idx) = inst_order.get(&val) {
                        if def_idx >= use_idx {
                            errors.push(VerifyError {
                                msg: format!(
                                    "value %{} used before its definition in block '{}'",
                                    val.0, block.name,
                                ),
                            });
                        }
                    }
                } else if !block_doms.contains(def_block) {
                    // Cross-block: def block must dominate use block.
                    errors.push(VerifyError {
                        msg: format!(
                            "value %{} defined in '{}' does not dominate use in '{}'",
                            val.0,
                            func.block(*def_block).name,
                            block.name,
                        ),
                    });
                }
            }
        };

        for (idx, inst) in block.insts.iter().enumerate() {
            for used in inst_uses(&inst.kind) {
                check_use(used, idx as i32, errors);
            }
        }
        if let Some(term) = &block.terminator {
            let term_idx = block.insts.len() as i32; // terminators come after all instructions
            for used in terminator_uses(term) {
                check_use(used, term_idx, errors);
            }
        }
    }
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
    fn dominance_cross_block_violation() {
        // Value defined in block B used in block A that B doesn't dominate.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let bb_a = b.create_block("block_a");
            let bb_b = b.create_block("block_b");

            // Entry branches to A.
            b.branch(bb_a, vec![]);

            // Block A uses a value from block B (which it can't reach yet).
            b.set_block(bb_a);
            // Manually insert a use of a value that will be defined in block B.
            let future_val = ValueId(100);
            b.emit_bogus_iadd(future_val, future_val);
            b.branch(bb_b, vec![]);

            // Block B defines the value.
            b.set_block(bb_b);
            let _v = b.const_i32(42); // This gets some ID, but we used 100 above.
            b.ret_void();
        }
        // Manually inject the value definition in block B with the ID we referenced.
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 0, col: 0 }, end: Position { line: 0, col: 0 } };
        func.blocks[2].insts.insert(0, Inst {
            id: ValueId(100),
            kind: InstKind::ConstInt(99, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("does not dominate")),
            "expected dominance error, got: {:?}", errs);
    }

    #[test]
    fn dominance_same_block_order_violation() {
        // Use a value before it's defined in the same block.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        use crate::lexer::{Span, Position};
        let span = Span { file_id: 0, start: Position { line: 0, col: 0 }, end: Position { line: 0, col: 0 } };

        // Manually construct: %1 = iadd %0, %0 then %0 = const_int 42
        // (use of %0 before its definition)
        func.blocks[0].insts.push(Inst {
            id: ValueId(1),
            kind: InstKind::IAdd(ValueId(0), ValueId(0)),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].insts.push(Inst {
            id: ValueId(0),
            kind: InstKind::ConstInt(42, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("used before its definition")),
            "expected same-block order error, got: {:?}", errs);
    }

    #[test]
    fn dominance_valid_loop() {
        // A valid loop: entry → header → body → header. Block params carry the value.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let header = b.create_block("header");
            let i_param = b.add_block_param(header, IrType::Int(IntWidth::I32));
            let init = b.const_i32(0);
            b.branch(header, vec![init]);

            b.set_block(header);
            let one = b.const_i32(1);
            let next = b.iadd(i_param, one);
            let limit = b.const_i32(10);
            let done = b.icmp(CmpOp::Ge, next, limit);
            let exit = b.create_block("exit");
            b.cond_branch(done, exit, vec![], header, vec![next]);

            b.set_block(exit);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(errs.is_empty(), "valid loop should pass, got: {:?}", errs);
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
