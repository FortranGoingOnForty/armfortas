//! IR verifier — checks well-formedness of the SSA IR.
//!
//! Run after every IR transformation to catch bugs early.
//! Checks: SSA dominance, type consistency, block structure,
//! terminator completeness, block param/branch arg matching.

use super::inst::*;
use super::types::{FloatWidth, IntWidth, IrType, TypeSizeError};
use super::walk::{compute_dominator_info, inst_uses, terminator_targets, terminator_uses};
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
    for global in &module.globals {
        if matches!(
            global.ty.try_size_bytes(&module.layout),
            Err(TypeSizeError::Overflow)
        ) {
            errors.push(VerifyError {
                msg: format!(
                    "global '{}' has type '{}' whose byte size overflows the target address space",
                    global.name, global.ty
                ),
            });
        }
    }
    for func in &module.functions {
        // Prefix each finding with the enclosing function so a failure
        // in a large amalgamated build points at the offending routine.
        for mut e in verify_function(func) {
            e.msg = format!("in '{}': {}", func.name, e.msg);
            errors.push(e);
        }
    }
    errors
}

/// Verify a single function.
pub fn verify_function(func: &Function) -> Vec<VerifyError> {
    let mut errors = Vec::new();

    // 1. Every block must have a unique identity and exactly one terminator.
    // All later CFG and dominance checks use BlockId-keyed maps, so duplicate
    // IDs make every downstream interpretation ambiguous.
    let mut seen_blocks: HashMap<BlockId, &str> = HashMap::new();
    let mut has_duplicate_blocks = false;
    for block in &func.blocks {
        if let Some(first_name) = seen_blocks.get(&block.id) {
            errors.push(VerifyError {
                msg: format!(
                    "duplicate block ID {}: blocks '{}' and '{}' share an identity",
                    block.id.0, first_name, block.name,
                ),
            });
            has_duplicate_blocks = true;
        } else {
            seen_blocks.insert(block.id, block.name.as_str());
        }
        if block.terminator.is_none() {
            errors.push(VerifyError {
                msg: format!("block '{}' has no terminator", block.name),
            });
        }
    }
    if has_duplicate_blocks {
        return errors;
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

    // 3a. Every present O(1) type-cache entry must match its authoritative
    // definition. Missing entries remain safe because value_type() walks the
    // definitions on a miss. A present hit is different: if Inst.ty changed,
    // all later consistency checks would otherwise reuse the stale cached
    // type and make verification false-green.
    for param in &func.params {
        check_type_cache_entry(func, param.id, &param.ty, &mut errors);
    }
    for block in &func.blocks {
        for param in &block.params {
            check_type_cache_entry(func, param.id, &param.ty, &mut errors);
        }
        for inst in &block.insts {
            check_type_cache_entry(func, inst.id, &inst.ty, &mut errors);
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

    // 5. Branch conditions and arguments must have the expected types.
    for block in &func.blocks {
        if let Some(term) = &block.terminator {
            check_branch_types(func, term, &block.name, &mut errors);
            check_return_type(func, term, &block.name, &mut errors);
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

fn check_type_cache_entry(
    func: &Function,
    value: ValueId,
    defining_type: &IrType,
    errors: &mut Vec<VerifyError>,
) {
    match func.cached_value_type(value) {
        Some(cached_type) if cached_type != defining_type => errors.push(VerifyError {
            msg: format!(
                "value %{} has type '{}' in the type cache but its definition has type '{}' \
                 (call rebuild_type_cache)",
                value.0, cached_type, defining_type,
            ),
        }),
        None | Some(_) => {}
    }
}

fn check_return_type(
    func: &Function,
    term: &Terminator,
    from_block: &str,
    errors: &mut Vec<VerifyError>,
) {
    let Terminator::Return(value) = term else {
        return;
    };
    match (&func.return_type, value) {
        (IrType::Void, None) => {}
        (IrType::Void, Some(value)) => errors.push(VerifyError {
            msg: format!(
                "return from block '{}': void function must not return a value (got %{})",
                from_block, value.0,
            ),
        }),
        (expected, None) => errors.push(VerifyError {
            msg: format!(
                "return from block '{}': non-void function must return a value of type {}",
                from_block, expected,
            ),
        }),
        (expected, Some(value)) => match func.value_type(*value) {
            Some(actual) if &actual != expected => errors.push(VerifyError {
                msg: format!(
                    "return type mismatch in block '{}': expected {}, got {} from %{}",
                    from_block, expected, actual, value.0,
                ),
            }),
            None => errors.push(VerifyError {
                msg: format!(
                    "return from block '{}': value %{} has no type; expected {}",
                    from_block, value.0, expected,
                ),
            }),
            Some(_) => {}
        },
    }
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
    let doms = compute_dominator_info(func);
    let def_blocks = value_def_block(func);

    for block in &func.blocks {
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
                } else if !doms.dominates(*def_block, block.id) {
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

/// Check condition types and match branch arguments to block parameters.
fn check_branch_types(
    func: &Function,
    term: &Terminator,
    from_block: &str,
    errors: &mut Vec<VerifyError>,
) {
    if let Terminator::CondBranch { cond, .. } = term {
        match func.value_type(*cond) {
            Some(IrType::Bool) => {}
            Some(ty) => errors.push(VerifyError {
                msg: format!(
                    "conditional branch from '{}': condition must be Bool, got {}",
                    from_block, ty
                ),
            }),
            None => errors.push(VerifyError {
                msg: format!(
                    "conditional branch from '{}': condition %{} has no type; expected Bool",
                    from_block, cond.0
                ),
            }),
        }
    }

    let mut check = |dest: BlockId, args: &[ValueId]| {
        let Some(target) = func.try_block(dest) else {
            return;
        };
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
            if let Some(default_block) = func.try_block(*default) {
                if !default_block.params.is_empty() {
                    errors.push(VerifyError {
                        msg: format!(
                            "switch default target '{}' has block parameters",
                            default_block.name
                        ),
                    });
                }
            }
            for (_, dest) in cases {
                let Some(target) = func.try_block(*dest) else {
                    continue;
                };
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

/// Vector shapes recognized by both instruction-selection dispatchers for
/// arithmetic opcodes. Narrow i8/i16 vectors remain valid IR for operations
/// such as loads, stores, and bitwise manipulation, but neither backend has an
/// arithmetic dispatcher for them.
fn vector_arithmetic_shape_error(ty: &IrType) -> Option<String> {
    let supported = match ty {
        IrType::Vector { lanes: 4, elem } => matches!(
            elem.as_ref(),
            IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32)
        ),
        IrType::Vector { lanes: 2, elem } => matches!(
            elem.as_ref(),
            IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64)
        ),
        _ => false,
    };
    if supported {
        None
    } else {
        Some(format!(
            "vector arithmetic uses unsupported shape {}; expected <4 x i32>, \
             <2 x i64>, <4 x f32>, or <2 x f64>",
            ty
        ))
    }
}

#[derive(Clone, Copy)]
enum RuntimeAbiType {
    Void,
    I32,
    I64,
    F32,
    BytePtr,
    DataPtr,
    PrintableInt,
}

impl RuntimeAbiType {
    fn matches(self, ty: &IrType) -> bool {
        match self {
            Self::Void => matches!(ty, IrType::Void),
            Self::I32 => matches!(ty, IrType::Int(IntWidth::I32)),
            Self::I64 => matches!(ty, IrType::Int(IntWidth::I64)),
            Self::F32 => matches!(ty, IrType::Float(FloatWidth::F32)),
            Self::BytePtr => {
                matches!(ty, IrType::Ptr(inner) if matches!(inner.as_ref(), IrType::Int(IntWidth::I8)))
            }
            Self::DataPtr => matches!(ty, IrType::Ptr(_)),
            Self::PrintableInt => matches!(
                ty,
                IrType::Int(IntWidth::I32 | IntWidth::I64 | IntWidth::I128)
            ),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::BytePtr => "ptr<i8>",
            Self::DataPtr => "a data pointer",
            Self::PrintableInt => "i32, i64, or i128",
        }
    }
}

fn runtime_signature(runtime_func: &RuntimeFunc) -> (&'static [RuntimeAbiType], RuntimeAbiType) {
    use RuntimeAbiType::*;

    match runtime_func {
        RuntimeFunc::PrintInt => (&[PrintableInt], Void),
        RuntimeFunc::PrintReal => (&[F32], Void),
        RuntimeFunc::PrintString => (&[BytePtr, I64], Void),
        RuntimeFunc::PrintLogical => (&[I32], Void),
        RuntimeFunc::PrintNewline => (&[], Void),
        RuntimeFunc::Allocate => (&[I64], BytePtr),
        RuntimeFunc::Deallocate => (&[DataPtr], Void),
        RuntimeFunc::StringConcat => (&[BytePtr, I64, BytePtr, I64], BytePtr),
        RuntimeFunc::StringCopy => (&[BytePtr, I64, BytePtr, I64], Void),
        RuntimeFunc::StringCompare => (&[BytePtr, I64, BytePtr, I64], I32),
        RuntimeFunc::Stop | RuntimeFunc::ErrorStop => (&[], Void),
        RuntimeFunc::CheckBounds => (&[I64, I64, I64], Void),
    }
}

fn check_runtime_call_signature(
    func: &Function,
    inst: &Inst,
    runtime_func: &RuntimeFunc,
    args: &[ValueId],
    errors: &mut Vec<VerifyError>,
) {
    let (expected_args, expected_result) = runtime_signature(runtime_func);

    if !expected_result.matches(&inst.ty) {
        errors.push(VerifyError {
            msg: format!(
                "runtime call {:?} %{} result type mismatch: expected {}, got {}",
                runtime_func,
                inst.id.0,
                expected_result.description(),
                inst.ty,
            ),
        });
    }

    if args.len() != expected_args.len() {
        errors.push(VerifyError {
            msg: format!(
                "runtime call {:?} %{} argument count mismatch: expected {}, got {}",
                runtime_func,
                inst.id.0,
                expected_args.len(),
                args.len(),
            ),
        });
    }

    for (index, (arg, expected_type)) in args.iter().zip(expected_args).enumerate() {
        let Some(actual_type) = func.value_type(*arg) else {
            continue;
        };
        if !expected_type.matches(&actual_type) {
            errors.push(VerifyError {
                msg: format!(
                    "runtime call {:?} %{} argument {} type mismatch: expected {}, got {}",
                    runtime_func,
                    inst.id.0,
                    index,
                    expected_type.description(),
                    actual_type,
                ),
            });
        }
    }
}

/// Check type consistency for instructions.
fn check_type_consistency(func: &Function, inst: &Inst, errors: &mut Vec<VerifyError>) {
    if let Some(msg) = vector_shape_error(&inst.ty) {
        errors.push(VerifyError { msg });
    }
    if matches!(
        &inst.kind,
        InstKind::VAdd(_, _)
            | InstKind::VSub(_, _)
            | InstKind::VMul(_, _)
            | InstKind::VDiv(_, _)
            | InstKind::VMin(_, _)
            | InstKind::VMax(_, _)
            | InstKind::VFma(_, _, _)
            | InstKind::VNeg(_)
            | InstKind::VAbs(_)
            | InstKind::VSqrt(_)
    ) {
        if let Some(msg) = vector_arithmetic_shape_error(&inst.ty) {
            errors.push(VerifyError {
                msg: format!("vector arithmetic %{}: {}", inst.id.0, msg),
            });
        }
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
                if ta.is_int() && ta == tb && ta != &inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer op %{} result type {} does not match operand type {}",
                            inst.id.0, inst.ty, ta,
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
                if ta.is_float() && ta == tb && ta != &inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "float op %{} result type {} does not match operand type {}",
                            inst.id.0, inst.ty, ta,
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
                // but same machine-level size on LP64 targets).
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

        InstKind::ICmp(_, a, b) => {
            let ta = func.value_type(*a);
            let tb = func.value_type(*b);
            if let (Some(ta), Some(tb)) = (&ta, &tb) {
                if !ta.is_int() || !tb.is_int() {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer compare %{}: operands must be integers ({} / {})",
                            inst.id.0, ta, tb,
                        ),
                    });
                } else if ta != tb {
                    errors.push(VerifyError {
                        msg: format!(
                            "integer compare operand type mismatch: %{} : {} vs %{} : {}",
                            a.0, ta, b.0, tb,
                        ),
                    });
                }
            }
        }

        InstKind::Select(cond, true_val, false_val) => {
            if let Some(cond_ty) = func.value_type(*cond) {
                if cond_ty != IrType::Bool {
                    errors.push(VerifyError {
                        msg: format!("select condition must be Bool: %{} got {}", cond.0, cond_ty,),
                    });
                }
            }
            if let (Some(true_ty), Some(false_ty)) =
                (func.value_type(*true_val), func.value_type(*false_val))
            {
                if true_ty != false_ty || true_ty != inst.ty {
                    errors.push(VerifyError {
                        msg: format!(
                            "select %{}: arms and result must share type ({} / {} / {})",
                            inst.id.0, true_ty, false_ty, inst.ty,
                        ),
                    });
                }
            }
        }

        InstKind::RuntimeCall(runtime_func, args) => {
            check_runtime_call_signature(func, inst, runtime_func, args, errors);
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
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("main".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
    fn valid_runtime_call_signatures_are_accepted() {
        let mut func = Function::new("runtime_calls".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let i32_value = b.const_i32(1);
            let i64_value = b.const_i64(1);
            let i128_value = b.const_i128(1);
            let f32_value = b.const_f32(1.0);
            let bytes = b.const_string(b"bytes");

            b.runtime_call(RuntimeFunc::PrintInt, vec![i32_value], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintInt, vec![i64_value], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintInt, vec![i128_value], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintReal, vec![f32_value], IrType::Void);
            b.runtime_call(
                RuntimeFunc::PrintString,
                vec![bytes, i64_value],
                IrType::Void,
            );
            b.runtime_call(RuntimeFunc::PrintLogical, vec![i32_value], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintNewline, vec![], IrType::Void);

            let allocation = b.runtime_call(
                RuntimeFunc::Allocate,
                vec![i64_value],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
            );
            b.runtime_call(RuntimeFunc::Deallocate, vec![allocation], IrType::Void);
            b.runtime_call(
                RuntimeFunc::StringConcat,
                vec![bytes, i64_value, bytes, i64_value],
                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
            );
            b.runtime_call(
                RuntimeFunc::StringCopy,
                vec![bytes, i64_value, bytes, i64_value],
                IrType::Void,
            );
            b.runtime_call(
                RuntimeFunc::StringCompare,
                vec![bytes, i64_value, bytes, i64_value],
                IrType::Int(IntWidth::I32),
            );
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
            b.runtime_call(
                RuntimeFunc::CheckBounds,
                vec![i64_value, i64_value, i64_value],
                IrType::Void,
            );
            b.ret_void();
        }

        let errs = verify_function(&func);
        assert!(
            errs.is_empty(),
            "valid runtime ABI calls should verify, got: {errs:?}",
        );
    }

    #[test]
    fn malformed_runtime_call_signatures_are_rejected() {
        let mut func = Function::new("runtime_calls".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let boolean = b.const_bool(true);
            let i32_value = b.const_i32(1);
            let i64_value = b.const_i64(1);
            let f64_value = b.const_f64(1.0);
            let bytes = b.const_string(b"bytes");
            let byte_ptr = IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)));

            b.runtime_call(RuntimeFunc::PrintInt, vec![], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintReal, vec![f64_value], IrType::Void);
            b.runtime_call(
                RuntimeFunc::PrintString,
                vec![bytes, i32_value],
                IrType::Void,
            );
            b.runtime_call(RuntimeFunc::PrintLogical, vec![boolean], IrType::Void);
            b.runtime_call(RuntimeFunc::PrintNewline, vec![i32_value], IrType::Void);
            b.runtime_call(
                RuntimeFunc::Allocate,
                vec![i32_value],
                IrType::Int(IntWidth::I64),
            );
            b.runtime_call(RuntimeFunc::Deallocate, vec![i64_value], IrType::Void);
            b.runtime_call(
                RuntimeFunc::StringConcat,
                vec![bytes, bytes],
                byte_ptr.clone(),
            );
            b.runtime_call(
                RuntimeFunc::StringCopy,
                vec![bytes, i64_value, bytes, i64_value],
                byte_ptr,
            );
            b.runtime_call(
                RuntimeFunc::StringCompare,
                vec![bytes, i64_value, bytes, i64_value],
                IrType::Void,
            );
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Int(IntWidth::I32));
            b.runtime_call(RuntimeFunc::ErrorStop, vec![i32_value], IrType::Void);
            b.runtime_call(
                RuntimeFunc::CheckBounds,
                vec![i64_value, i32_value, i64_value],
                IrType::Void,
            );
            b.ret_void();
        }

        let errs = verify_function(&func);
        for runtime_func in [
            "PrintInt",
            "PrintReal",
            "PrintString",
            "PrintLogical",
            "PrintNewline",
            "Allocate",
            "Deallocate",
            "StringConcat",
            "StringCopy",
            "StringCompare",
            "Stop",
            "ErrorStop",
            "CheckBounds",
        ] {
            assert!(
                errs.iter().any(|error| error.msg.contains(runtime_func)),
                "expected a {runtime_func} ABI diagnostic, got: {errs:?}",
            );
        }
    }

    #[test]
    fn oversized_global_array_layout_is_rejected() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_global(Global {
            name: "oversized".into(),
            ty: IrType::Array(Box::new(IrType::Int(IntWidth::I64)), 1_u64 << 61),
            initializer: Some(GlobalInit::Zero),
        });

        let errs = verify_module(&module);
        assert!(
            errs.iter().any(|error| {
                error.msg.contains("global 'oversized'")
                    && error.msg.contains("overflows the target address space")
            }),
            "expected an oversized-global layout error, got: {errs:?}",
        );
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
    fn dangling_cfg_targets_are_reported_without_panicking() {
        let dangling = BlockId(999);

        let mut branch = Function::new("branch".into(), vec![], IrType::Void);
        branch.blocks[0].terminator = Some(Terminator::Branch(dangling, vec![]));
        let branch_errs = verify_function(&branch);
        assert!(
            branch_errs
                .iter()
                .any(|error| error.msg.contains("branches to undefined block 999")),
            "expected a dangling branch diagnostic, got: {branch_errs:?}",
        );

        let mut conditional = Function::new("conditional".into(), vec![], IrType::Void);
        let valid_conditional_target;
        {
            let mut b = FuncBuilder::new(&mut conditional, crate::target::TargetLayout::LP64);
            let condition = b.const_bool(true);
            valid_conditional_target = b.create_block("valid");
            b.cond_branch(
                condition,
                dangling,
                vec![],
                valid_conditional_target,
                vec![],
            );
            b.set_block(valid_conditional_target);
            b.ret_void();
        }
        let conditional_errs = verify_function(&conditional);
        assert!(
            conditional_errs
                .iter()
                .any(|error| error.msg.contains("branches to undefined block 999")),
            "expected a dangling conditional-branch diagnostic, got: {conditional_errs:?}",
        );

        let mut switch = Function::new("switch".into(), vec![], IrType::Void);
        let selector;
        {
            let mut b = FuncBuilder::new(&mut switch, crate::target::TargetLayout::LP64);
            selector = b.const_i32(1);
        }
        switch.blocks[0].terminator = Some(Terminator::Switch {
            selector,
            cases: vec![(1, dangling)],
            default: dangling,
        });
        let switch_errs = verify_function(&switch);
        assert_eq!(
            switch_errs
                .iter()
                .filter(|error| error.msg.contains("branches to undefined block 999"))
                .count(),
            2,
            "case and default edges should each be diagnosed: {switch_errs:?}",
        );
    }

    #[test]
    fn integer_op_on_float_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
    fn scalar_arithmetic_result_type_must_match_operands() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let (integer_result, float_result);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let integer_lhs = b.const_i32(1);
            let integer_rhs = b.const_i32(2);
            integer_result = b.iadd(integer_lhs, integer_rhs);
            let float_lhs = b.const_f32(1.0);
            let float_rhs = b.const_f32(2.0);
            float_result = b.fadd(float_lhs, float_rhs);
            b.ret_void();
        }

        for inst in &mut func.blocks[0].insts {
            if inst.id == integer_result {
                inst.ty = IrType::Int(IntWidth::I64);
            } else if inst.id == float_result {
                inst.ty = IrType::Float(FloatWidth::F64);
            }
        }
        func.rebuild_type_cache();

        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|error| {
                error
                    .msg
                    .contains(&format!("integer op %{}", integer_result.0))
                    && error.msg.contains("result type i64")
                    && error.msg.contains("operand type i32")
            }),
            "expected the integer result-type mismatch to be rejected, got: {errs:?}",
        );
        assert!(
            errs.iter().any(|error| {
                error.msg.contains(&format!("float op %{}", float_result.0))
                    && error.msg.contains("result type f64")
                    && error.msg.contains("operand type f32")
            }),
            "expected the float result-type mismatch to be rejected, got: {errs:?}",
        );
    }

    #[test]
    fn return_form_and_type_must_match_function_signature() {
        let mut wrong_type = Function::new("wrong_type".into(), vec![], IrType::Int(IntWidth::I64));
        {
            let mut b = FuncBuilder::new(&mut wrong_type, crate::target::TargetLayout::LP64);
            let value = b.const_i32(1);
            b.ret(Some(value));
        }
        let wrong_type_errs = verify_function(&wrong_type);
        assert!(
            wrong_type_errs.iter().any(|error| {
                error.msg.contains("return type mismatch")
                    && error.msg.contains("expected i64")
                    && error.msg.contains("got i32")
            }),
            "expected the returned value type to be checked, got: {wrong_type_errs:?}",
        );

        let mut missing_value =
            Function::new("missing_value".into(), vec![], IrType::Int(IntWidth::I32));
        missing_value.blocks[0].terminator = Some(Terminator::Return(None));
        let missing_value_errs = verify_function(&missing_value);
        assert!(
            missing_value_errs.iter().any(|error| error
                .msg
                .contains("non-void function must return a value of type i32")),
            "expected a missing return value to be rejected, got: {missing_value_errs:?}",
        );

        let mut unexpected_value = Function::new("unexpected_value".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut unexpected_value, crate::target::TargetLayout::LP64);
            let value = b.const_i32(1);
            b.ret(Some(value));
        }
        let unexpected_value_errs = verify_function(&unexpected_value);
        assert!(
            unexpected_value_errs
                .iter()
                .any(|error| error.msg.contains("void function must not return a value")),
            "expected a value return from void to be rejected, got: {unexpected_value_errs:?}",
        );

        let mut valid = Function::new("valid".into(), vec![], IrType::Int(IntWidth::I32));
        {
            let mut b = FuncBuilder::new(&mut valid, crate::target::TargetLayout::LP64);
            let value = b.const_i32(1);
            b.ret(Some(value));
        }
        let valid_errs = verify_function(&valid);
        assert!(
            valid_errs.is_empty(),
            "matching typed return should remain valid, got: {valid_errs:?}",
        );
    }

    #[test]
    fn type_consistency_survives_missing_cache_entries() {
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
    fn present_but_stale_type_cache_entry_is_rejected() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let stale_value;
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let lhs = b.const_i32(1);
            let rhs = b.const_i32(2);
            stale_value = b.iadd(lhs, rhs);
            let zero = b.const_i32(0);
            let _use_stale_value = b.iadd(stale_value, zero);
            b.ret_void();
        }

        let defining_inst = func.blocks[0]
            .insts
            .iter_mut()
            .find(|inst| inst.id == stale_value)
            .expect("builder emitted the stale-cache witness");
        assert_eq!(defining_inst.ty, IrType::Int(IntWidth::I32));
        defining_inst.ty = IrType::Int(IntWidth::I64);

        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|error| {
                error.msg.contains(&format!("value %{}", stale_value.0))
                    && error.msg.contains("type cache")
                    && error.msg.contains("i32")
                    && error.msg.contains("i64")
            }),
            "expected the stale present cache entry to be rejected, got: {errs:?}",
        );
        assert_eq!(
            errs.iter()
                .filter(|error| error.msg.contains("type cache"))
                .count(),
            1,
            "one stale definition should produce one cache diagnostic: {errs:?}",
        );

        func.rebuild_type_cache();
        let rebuilt_errs = verify_function(&func);
        assert!(
            rebuilt_errs
                .iter()
                .all(|error| !error.msg.contains("type cache")),
            "rebuilding should restore cache consistency: {rebuilt_errs:?}",
        );
        assert!(
            rebuilt_errs
                .iter()
                .any(|error| error.msg.contains("width mismatch")),
            "verification should expose the underlying malformed iadd after rebuilding: \
             {rebuilt_errs:?}",
        );
    }

    #[test]
    fn valid_branch_with_args() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
    fn cond_branch_requires_bool_condition() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let cond = b.const_i32(1);
            let bb_t = b.create_block("then");
            let bb_f = b.create_block("else");
            b.cond_branch(cond, bb_t, vec![], bb_f, vec![]);

            b.set_block(bb_t);
            b.ret_void();
            b.set_block(bb_f);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(
            errs.iter()
                .any(|e| e.msg.contains("condition must be Bool")),
            "expected non-Bool branch condition error, got: {:?}",
            errs,
        );
    }

    #[test]
    fn select_requires_bool_condition() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let cond = b.const_i32(1);
            let true_val = b.const_i32(2);
            let false_val = b.const_i32(3);
            b.select(cond, true_val, false_val);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(
            errs.iter()
                .any(|e| e.msg.contains("select condition must be Bool")),
            "expected non-Bool select condition error, got: {:?}",
            errs,
        );
    }

    #[test]
    fn icmp_requires_matching_integer_operands() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let wide = b.const_int(1, IntWidth::I128);
            let narrow = b.const_i32(1);
            b.icmp(CmpOp::Eq, wide, narrow);
            b.ret_void();
        }
        let errs = verify_function(&func);
        assert!(
            errs.iter()
                .any(|e| e.msg.contains("integer compare operand type mismatch")),
            "expected integer compare type mismatch, got: {:?}",
            errs,
        );
    }

    #[test]
    fn store_to_non_pointer_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
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

    #[test]
    fn duplicate_block_id_errors() {
        let mut func = Function::new("test".into(), vec![], IrType::Void);
        let first = func.create_block("first");
        let _second = func.create_block("second");
        for block in &mut func.blocks {
            block.terminator = Some(Terminator::Return(None));
        }

        func.blocks[2].id = first;

        let errs = verify_function(&func);
        assert!(
            errs.iter().any(|error| error
                .msg
                .contains(&format!("duplicate block ID {}", first.0))),
            "expected duplicate block ID {first:?} to be rejected, got: {errs:?}",
        );
    }

    // ---- SIMD vector type / verify tests ----

    fn verify_vadd_with_type(ty: IrType) -> Vec<VerifyError> {
        let mut func = Function::new("vector_add".into(), vec![], IrType::Void);
        {
            let mut b = FuncBuilder::new(&mut func, crate::target::TargetLayout::LP64);
            let (lhs, rhs) = if matches!(&ty, IrType::Vector { .. }) {
                let lhs_ptr = b.alloca(ty.clone());
                let rhs_ptr = b.alloca(ty.clone());
                (b.vload(lhs_ptr, ty.clone()), b.vload(rhs_ptr, ty.clone()))
            } else {
                (b.const_i32(1), b.const_i32(2))
            };
            b.vadd(lhs, rhs);
            b.ret_void();
        }
        verify_function(&func)
    }

    #[test]
    fn vector_arithmetic_accepts_backend_dispatch_shapes() {
        for ty in [
            IrType::Vector {
                lanes: 4,
                elem: Box::new(IrType::Int(IntWidth::I32)),
            },
            IrType::Vector {
                lanes: 2,
                elem: Box::new(IrType::Int(IntWidth::I64)),
            },
            IrType::Vector {
                lanes: 4,
                elem: Box::new(IrType::Float(FloatWidth::F32)),
            },
            IrType::Vector {
                lanes: 2,
                elem: Box::new(IrType::Float(FloatWidth::F64)),
            },
        ] {
            let errs = verify_vadd_with_type(ty.clone());
            assert!(
                errs.is_empty(),
                "expected backend-dispatched vector-arithmetic shape {ty} to verify, got: {errs:?}",
            );
        }
    }

    #[test]
    fn vector_arithmetic_rejects_shapes_without_backend_dispatch() {
        for ty in [
            IrType::Vector {
                lanes: 16,
                elem: Box::new(IrType::Int(IntWidth::I8)),
            },
            IrType::Vector {
                lanes: 8,
                elem: Box::new(IrType::Int(IntWidth::I16)),
            },
            IrType::Int(IntWidth::I32),
        ] {
            let errs = verify_vadd_with_type(ty.clone());
            assert!(
                errs.iter().any(|error| {
                    error.msg.contains("vector arithmetic") && error.msg.contains(&ty.to_string())
                }),
                "expected unsupported vector-arithmetic shape {ty} to be rejected, got: {errs:?}",
            );
        }
    }

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
        assert_eq!(ty.size_bytes(&crate::target::TargetLayout::LP64), 16);
    }
}
