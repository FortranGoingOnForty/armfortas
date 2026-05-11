//! IR verifier — checks well-formedness of the SSA IR.
//!
//! Run after every IR transformation to catch bugs early.
//! Checks: SSA dominance, type consistency, block structure,
//! terminator completeness, block param/branch arg matching.

use super::inst::*;
use super::types::{IntWidth, IrType};
use super::walk::{compute_dominators, inst_uses, terminator_targets, terminator_uses};
use std::collections::{HashMap, HashSet};

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

    // 2. Entry block has no predecessors. The block params on entry
    //    represent function parameters and are reserved for the
    //    function ABI; nothing inside the function body should be
    //    able to branch back to entry. The check splits into:
    //      a) entry has no block params, AND
    //      b) no other block's terminator targets entry.
    //    Both halves must hold for SSA construction to be sound —
    //    a back-edge into entry would let cross-block phi flow
    //    overwrite the function's incoming params.
    let entry_block = func.block(func.entry);
    if !entry_block.params.is_empty() {
        errors.push(VerifyError {
            msg: "entry block must not have block parameters".into(),
        });
    }
    for block in &func.blocks {
        let Some(term) = &block.terminator else {
            continue;
        };
        if terminator_targets(term).contains(&func.entry) {
            errors.push(VerifyError {
                msg: format!(
                    "block '{}' branches back into the entry block — \
                     entry must have no predecessors",
                    block.name,
                ),
            });
        }
    }

    // 3. All ValueIds used must be defined.
    let defined = collect_defined_values(func);
    for block in &func.blocks {
        for inst in &block.insts {
            for used in inst_uses(&inst.kind) {
                if !defined.contains(&used) {
                    errors.push(VerifyError {
                        msg: format!(
                            "value %{} used in block '{}' but not defined",
                            used.0, block.name
                        ),
                    });
                }
            }
        }
        if let Some(term) = &block.terminator {
            for used in terminator_uses(term) {
                if !defined.contains(&used) {
                    errors.push(VerifyError {
                        msg: format!(
                            "value %{} used in terminator of block '{}' but not defined",
                            used.0, block.name
                        ),
                    });
                }
            }
        }
    }

    // 3a. Every defined value must have an entry in the function's
    // type cache. A miss here means rebuild_type_cache was never
    // run after an IR-mutating pass (or the pass forgot to set
    // inst.ty). Without the cache entry, check_type_consistency
    // silently skips every check on that instruction — a single
    // stale cache turns the whole verifier into a no-op. This was
    // a latent soundness hole: wrong-width iadd, bogus pointee
    // mismatches, and non-integer ops all sailed through. Flag it
    // explicitly so the bug points at the actual cache bug, not
    // the downstream codegen symptom.
    for val in &defined {
        if func.value_type(*val).is_none() {
            errors.push(VerifyError {
                msg: format!(
                    "value %{} is defined but has no entry in the type cache (call rebuild_type_cache)",
                    val.0,
                ),
            });
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
                        msg: format!(
                            "block '{}' branches to undefined block {}",
                            block.name, target.0
                        ),
                    });
                }
            }
        }
    }

    // 8. SSA dominance: every use must be dominated by its definition.
    check_dominance(func, &mut errors);

    errors
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

/// Check that branch arguments match block parameters in count and type.
fn check_branch_args(
    func: &Function,
    term: &Terminator,
    from_block: &str,
    errors: &mut Vec<VerifyError>,
) {
    let mut check = |dest: BlockId, args: &[ValueId]| {
        let target = func.block(dest);
        if target.params.len() != args.len() {
            errors.push(VerifyError {
                msg: format!(
                    "branch from '{}' to '{}': expected {} args, got {}",
                    from_block,
                    target.name,
                    target.params.len(),
                    args.len()
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
        Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        } => {
            check(*true_dest, true_args);
            check(*false_dest, false_args);
        }
        Terminator::Switch { cases, default, .. } => {
            // Switch targets shouldn't have block params (simplified model).
            let default_block = func.block(*default);
            if !default_block.params.is_empty() {
                errors.push(VerifyError {
                    msg: format!(
                        "switch default target '{}' has block parameters",
                        default_block.name
                    ),
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

/// Vector type shape check: 128-bit total, lanes ∈ {2,4,8,16}, elem
/// scalar of matching width. Returns an error string if invalid.
fn vector_shape_error(ty: &IrType) -> Option<String> {
    let IrType::Vector { lanes, elem } = ty else {
        return None;
    };
    let lanes = *lanes;
    let elem_bits = match elem.as_ref() {
        IrType::Int(w) => w.bits(),
        IrType::Float(w) => w.bits(),
        other => {
            return Some(format!(
                "vector element type must be scalar int/float, got {}",
                other
            ));
        }
    };
    if !matches!(lanes, 2 | 4 | 8 | 16) {
        return Some(format!(
            "vector lane count {} unsupported (NEON: 2/4/8/16)",
            lanes
        ));
    }
    if (lanes as u32) * elem_bits != 128 {
        return Some(format!(
            "vector total width {}b must be 128 ({}× {}b)",
            (lanes as u32) * elem_bits,
            lanes,
            elem_bits
        ));
    }
    None
}

/// Check type consistency for instructions.
fn check_type_consistency(func: &Function, inst: &Inst, errors: &mut Vec<VerifyError>) {
    if let Some(msg) = vector_shape_error(&inst.ty) {
        errors.push(VerifyError { msg });
    }
    match &inst.kind {
        InstKind::IAdd(a, b)
        | InstKind::ISub(a, b)
        | InstKind::IMul(a, b)
        | InstKind::IDiv(a, b)
        | InstKind::IMod(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            // Report missing types so the upstream cache-miss (already
            // flagged as error #3a) doesn't let width/type-category
            // mismatches sail through silently.
            if ta.is_none() {
                errors.push(VerifyError {
                    msg: format!("integer op %{}: operand %{} has no type", inst.id.0, a.0),
                });
            }
            if tb.is_none() {
                errors.push(VerifyError {
                    msg: format!("integer op %{}: operand %{} has no type", inst.id.0, b.0),
                });
            }
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_int() {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer op %{} has non-integer operand %{} : {}",
                            inst.id.0, a.0, ta
                        ),
                    });
                }
                if !tb.is_int() {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer op %{} has non-integer operand %{} : {}",
                            inst.id.0, b.0, tb
                        ),
                    });
                }
                // Audit MAJOR-4: enforce exact width agreement.
                // Mixing widths like IMul(i32, i64) would silently
                // miscompile because codegen has no implicit
                // promotion — every binary op picks one operand's
                // width and the other operand reads stale upper
                // bits. Lowering today never produces a width
                // mismatch, but the verifier is the last line of
                // defense for future passes.
                if ta.is_int() && tb.is_int() && ta != tb {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer op %{}: operand width mismatch %{} : {} vs %{} : {}",
                            inst.id.0, a.0, ta, b.0, tb,
                        ),
                    });
                }
            }
        }
        InstKind::FAdd(a, b)
        | InstKind::FSub(a, b)
        | InstKind::FMul(a, b)
        | InstKind::FDiv(a, b)
        | InstKind::FPow(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_float() {
                    errors.push(VerifyError {
                        msg: format!(
                            "float op %{} has non-float operand %{} : {}",
                            inst.id.0, a.0, ta
                        ),
                    });
                }
                if !tb.is_float() {
                    errors.push(VerifyError {
                        msg: format!(
                            "float op %{} has non-float operand %{} : {}",
                            inst.id.0, b.0, tb
                        ),
                    });
                }
                // Same width-agreement rule for floats. Mixing
                // f32 and f64 in a single binary op is illegal —
                // lowering inserts FloatExtend/FloatTrunc to align
                // operands, and a missing widening would land here.
                if ta.is_float() && tb.is_float() && ta != tb {
                    errors.push(VerifyError {
                        msg: format!(
                            "float op %{}: operand width mismatch %{} : {} vs %{} : {}",
                            inst.id.0, a.0, ta, b.0, tb,
                        ),
                    });
                }
            }
        }
        InstKind::Store(val, addr) => {
            let addr_ty = func.value_type(*addr);
            if let Some(ty) = &addr_ty {
                if !ty.is_ptr() {
                    errors.push(VerifyError {
                        msg: format!("store %{} to non-pointer %{} : {}", inst.id.0, addr.0, ty),
                    });
                }
            }
            // Audit Maj-2: enforce value/pointee type agreement.
            // A Store(i64_val, ptr<i32>) would silently truncate
            // at codegen because isel picks the str width from
            // the value's reg class, not the pointer's pointee.
            if let (Some(IrType::Ptr(pointee)), Some(vty)) = (&addr_ty, func.value_type(*val)) {
                let inner: &IrType = pointee.as_ref();
                // Byte-level GEPs into derived-type layouts use
                // `ptr<i8>` as a marker with arbitrary pointee on
                // the real access path. Skip the check in that
                // specific case to avoid spurious errors.
                let pointee_is_byte = matches!(inner, IrType::Int(IntWidth::I8));
                // Also skip when both are pointer types (different pointees
                // but same machine-level size on ARM64).
                let both_ptrs = matches!(inner, IrType::Ptr(_)) && matches!(&vty, IrType::Ptr(_));
                if !pointee_is_byte && !both_ptrs && vty != *inner {
                    errors.push(VerifyError {
                        msg: format!(
                            "store %{}: value type {} doesn't match pointee type {}",
                            inst.id.0, vty, inner,
                        ),
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

        // Bitwise binary ops: both operands must be integers of
        // the same width. Audit Med-1.
        InstKind::BitAnd(a, b)
        | InstKind::BitOr(a, b)
        | InstKind::BitXor(a, b)
        | InstKind::Shl(a, b)
        | InstKind::LShr(a, b)
        | InstKind::AShr(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_int() || !tb.is_int() {
                    errors.push(VerifyError {
                        msg: format!(
                            "bitwise op %{}: operand must be integer ({} / {})",
                            inst.id.0, ta, tb,
                        ),
                    });
                }
                if ta.is_int() && tb.is_int() && ta != tb {
                    errors.push(VerifyError {
                        msg: format!(
                            "bitwise op %{}: operand width mismatch {} vs {}",
                            inst.id.0, ta, tb,
                        ),
                    });
                }
            }
        }

        // Logical And/Or: both operands must be Bool.
        InstKind::And(a, b) | InstKind::Or(a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !matches!(ta, IrType::Bool) || !matches!(tb, IrType::Bool) {
                    errors.push(VerifyError {
                        msg: format!(
                            "logical op %{}: operands must be Bool ({} / {})",
                            inst.id.0, ta, tb,
                        ),
                    });
                }
            }
        }

        // ---- SIMD vector ops ----
        //
        // Element-wise binary / unary / fma ops require all vector
        // operands and the result to share the same lane shape.
        InstKind::VAdd(a, b)
        | InstKind::VSub(a, b)
        | InstKind::VMul(a, b)
        | InstKind::VDiv(a, b)
        | InstKind::VMin(a, b)
        | InstKind::VMax(a, b) => {
            if let (Some(ta), Some(tb)) = (func.value_type(*a), func.value_type(*b)) {
                if ta != tb || ta != inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "vector binop operands and result must share type: lhs {}, rhs {}, result {}",
                            ta, tb, inst.ty
                        ),
                    });
                }
            }
        }
        InstKind::VFma(a, b, c) => {
            if let (Some(ta), Some(tb), Some(tc)) = (
                func.value_type(*a),
                func.value_type(*b),
                func.value_type(*c),
            ) {
                if ta != tb || tb != tc || ta != inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "vfma operands and result must share type: a {}, b {}, c {}, result {}",
                            ta, tb, tc, inst.ty
                        ),
                    });
                }
            }
        }
        InstKind::VSelect(_m, t, f) => {
            // Mask is a vector of any element type (typically the
            // bool/int result of vicmp/vfcmp); t and f must share
            // type with each other and with the result.
            if let (Some(tt), Some(tf)) = (func.value_type(*t), func.value_type(*f)) {
                if tt != tf || tt != inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "vselect t/f and result must share type: t {}, f {}, result {}",
                            tt, tf, inst.ty
                        ),
                    });
                }
            }
        }
        InstKind::VNeg(a) | InstKind::VAbs(a) | InstKind::VSqrt(a) => {
            if let Some(ta) = func.value_type(*a) {
                if ta != inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "vector unop operand and result must share type: {} vs {}",
                            ta, inst.ty
                        ),
                    });
                }
            }
        }
        InstKind::VExtract(v, lane) => {
            if let Some(IrType::Vector { lanes, .. }) = func.value_type(*v) {
                if *lane >= lanes {
                    errors.push(VerifyError {
                        msg: format!(
                            "vextract lane {} out of range (vector has {} lanes)",
                            lane, lanes
                        ),
                    });
                }
            }
        }
        InstKind::VInsert(v, lane, _s) => {
            if let Some(IrType::Vector { lanes, .. }) = func.value_type(*v) {
                if *lane >= lanes {
                    errors.push(VerifyError {
                        msg: format!(
                            "vinsert lane {} out of range (vector has {} lanes)",
                            lane, lanes
                        ),
                    });
                }
            }
        }

        _ => {} // other instructions checked as needed
    }
}

#[cfg(test)]
mod tests {
    use super::super::builder::FuncBuilder;
    use super::super::types::*;
    use super::*;

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
            id: ValueId(99),
            ty: IrType::Int(IntWidth::I32),
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("entry block")));
    }

    #[test]
    fn back_edge_into_entry_errors() {
        // entry → body → entry  (illegal back-edge into entry).
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let body = func.create_block("body");
        func.block_mut(func.entry).terminator = Some(Terminator::Branch(body, vec![]));
        func.block_mut(body).terminator = Some(Terminator::Branch(func.entry, vec![]));
        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|e| e.msg.contains("entry block")),
            "expected entry-back-edge error, got: {:?}",
            errs,
        );
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
        assert!(errs
            .iter()
            .any(|e| e.msg.contains("expected 1 args, got 0")));
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
    fn store_value_pointee_mismatch_errors() {
        // Audit Min-1: Store(i64_val, ptr<i32>) must be rejected
        // by the verifier — codegen has no implicit narrowing
        // and would silently truncate.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let val = b.const_i64(123);
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            b.store(val, addr);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(
            errs.iter()
                .any(|e| e.msg.contains("doesn't match pointee type")),
            "expected pointee type mismatch error, got: {:?}",
            errs,
        );
    }

    #[test]
    fn int_op_width_mismatch_errors() {
        // IMul(i32, i64) should be rejected — codegen has no
        // implicit width promotion and the verifier is the last
        // line of defense before mismatched widths reach isel.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func);
            let a = b.const_i32(1);
            let c = b.const_i64(2);
            b.emit_bogus_iadd(a, c);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|e| e.msg.contains("width mismatch")),
            "expected width mismatch, got: {:?}",
            errs,
        );
    }

    #[test]
    fn type_consistency_survives_stale_cache() {
        // Width-mismatched iadd inserted directly into a block (i.e.
        // bypassing the builder) used to slip past the type checker
        // because the type cache hadn't been refreshed and value_type
        // returned None for both operands. With on-demand fallback
        // the verifier walks the instruction list and still rejects
        // the bogus IR. Audit MAJOR: silent verifier short-circuit.
        use crate::lexer::{Position, Span};
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let span = Span {
            file_id: 0,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        };
        // %1: i32, %2: i64 — distinct widths, no cache update.
        func.blocks[0].insts.push(Inst {
            id: ValueId(1),
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].insts.push(Inst {
            id: ValueId(2),
            kind: InstKind::ConstInt(2, IntWidth::I64),
            ty: IrType::Int(IntWidth::I64),
            span,
        });
        func.blocks[0].insts.push(Inst {
            id: ValueId(3),
            kind: InstKind::IAdd(ValueId(1), ValueId(2)),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        // Note: type_cache is the constructor-time snapshot — it
        // does NOT contain the manually-inserted instructions.
        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|e| e.msg.contains("width mismatch")),
            "expected width mismatch even without cache: {:?}",
            errs,
        );
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
        use crate::lexer::{Position, Span};
        let span = Span {
            file_id: 0,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        };
        func.blocks[2].insts.insert(
            0,
            Inst {
                id: ValueId(100),
                kind: InstKind::ConstInt(99, IntWidth::I32),
                ty: IrType::Int(IntWidth::I32),
                span,
            },
        );
        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|e| e.msg.contains("does not dominate")),
            "expected dominance error, got: {:?}",
            errs
        );
    }

    #[test]
    fn dominance_same_block_order_violation() {
        // Use a value before it's defined in the same block.
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        use crate::lexer::{Position, Span};
        let span = Span {
            file_id: 0,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        };

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
        assert!(
            errs.iter()
                .any(|e| e.msg.contains("used before its definition")),
            "expected same-block order error, got: {:?}",
            errs
        );
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
        use crate::lexer::{Position, Span};
        let span = Span {
            file_id: 0,
            start: Position { line: 0, col: 0 },
            end: Position { line: 0, col: 0 },
        };
        func.blocks[0].insts.push(Inst {
            id: ValueId(0),
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].insts.push(Inst {
            id: ValueId(0),
            kind: InstKind::ConstInt(2, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span,
        });
        func.blocks[0].terminator = Some(Terminator::Return(None));
        let errs = verify_function(&func);
        assert!(errs.iter().any(|e| e.msg.contains("duplicate value ID")));
    }

    // ---- SIMD vector type / verify tests ----

    #[test]
    fn vector_shape_4xi32_ok() {
        assert!(vector_shape_error(&IrType::Vector {
            lanes: 4,
            elem: Box::new(IrType::Int(IntWidth::I32)),
        })
        .is_none());
    }

    #[test]
    fn vector_shape_2xf64_ok() {
        assert!(vector_shape_error(&IrType::Vector {
            lanes: 2,
            elem: Box::new(IrType::Float(FloatWidth::F64)),
        })
        .is_none());
    }

    #[test]
    fn vector_shape_3xi32_rejected() {
        // 3 lanes is not a NEON shape.
        assert!(vector_shape_error(&IrType::Vector {
            lanes: 3,
            elem: Box::new(IrType::Int(IntWidth::I32)),
        })
        .is_some());
    }

    #[test]
    fn vector_shape_8xi32_rejected_for_total_width() {
        // 8 × 32 = 256 bits, > NEON 128b.
        assert!(vector_shape_error(&IrType::Vector {
            lanes: 8,
            elem: Box::new(IrType::Int(IntWidth::I32)),
        })
        .is_some());
    }

    #[test]
    fn vector_type_displays_with_angle_bracket_form() {
        let ty = IrType::Vector {
            lanes: 4,
            elem: Box::new(IrType::Int(IntWidth::I32)),
        };
        assert_eq!(format!("{}", ty), "<4 x i32>");
    }

    #[test]
    fn vector_type_size_bytes_is_16() {
        let ty = IrType::Vector {
            lanes: 2,
            elem: Box::new(IrType::Float(FloatWidth::F64)),
        };
        assert_eq!(ty.size_bytes(), 16);
    }
}
