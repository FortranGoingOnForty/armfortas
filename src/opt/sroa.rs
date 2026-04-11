//! Scalar Replacement of Aggregates (SROA).
//!
//! Decomposes small aggregate allocas (`alloca [T x N]` where N ≤ 8)
//! into N individual scalar allocas. After SROA, mem2reg can promote
//! the scalars to SSA values in registers.
//!
//! Eligibility:
//! - Alloca type is `Array(elem, count)` with count ≤ SROA_MAX_FIELDS
//! - ALL uses of the alloca are GEP with a single constant index
//! - The alloca address is never passed to a Call (no escape)
//!
//! After SROA, a second Mem2Reg pass promotes the new scalar allocas.

use std::collections::HashMap;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::inst_uses;
use super::loop_utils::resolve_const_int;
use super::pass::Pass;

const SROA_MAX_FIELDS: u64 = 8;

pub struct Sroa;

impl Pass for Sroa {
    fn name(&self) -> &'static str { "sroa" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if sroa_function(func) { changed = true; }
        }
        changed
    }
}

fn sroa_function(func: &mut Function) -> bool {
    // Collect candidate allocas: Array type, small, all constant-index GEPs.
    let candidates = find_candidates(func);
    if candidates.is_empty() { return false; }

    let mut changed = false;
    for cand in candidates {
        if decompose_alloca(func, &cand) {
            changed = true;
        }
    }
    changed
}

struct SroaCandidate {
    alloca_id: ValueId,
    alloca_block: BlockId,
    alloca_inst_idx: usize,
    elem_ty: IrType,
    count: u64,
}

fn find_candidates(func: &Function) -> Vec<SroaCandidate> {
    let mut candidates = Vec::new();

    for block in &func.blocks {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            if let InstKind::Alloca(IrType::Array(ref elem, count)) = inst.kind {
                if count <= SROA_MAX_FIELDS && count > 0 {
                    // Check eligibility: all uses are constant-index GEPs, no escape.
                    if is_eligible(func, inst.id) {
                        candidates.push(SroaCandidate {
                            alloca_id: inst.id,
                            alloca_block: block.id,
                            alloca_inst_idx: inst_idx,
                            elem_ty: (**elem).clone(),
                            count,
                        });
                    }
                }
            }
        }
    }
    candidates
}

/// Check if all uses of an alloca are constant-index GEPs with no escape.
fn is_eligible(func: &Function, alloca_id: ValueId) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            let uses = inst_uses(&inst.kind);
            if !uses.contains(&alloca_id) { continue; }

            // Classify this use of the alloca.
            match &inst.kind {
                // GEP with the alloca as base: OK if single constant index
                // AND the result type matches the element type (not byte-level).
                InstKind::GetElementPtr(base, indices) if *base == alloca_id => {
                    if indices.len() != 1 { return false; }
                    if resolve_const_int(func, indices[0]).is_none() {
                        return false;
                    }
                    // Reject byte-level GEPs (ptr<i8>) — SROA only handles
                    // element-typed accesses. Byte-level GEPs from array
                    // constructors use raw byte offsets that don't map to
                    // element indices.
                    if let Some(IrType::Ptr(inner)) = func.value_type(inst.id) {
                        if matches!(*inner, IrType::Int(crate::ir::types::IntWidth::I8)) {
                            // Result is ptr<i8> — byte-level access, not element.
                            return false;
                        }
                    }
                    if gep_result_escapes(func, inst.id) {
                        return false;
                    }
                    // This use is fine — constant-index element access.
                }
                // Store where the alloca is the VALUE being stored = pointer escape.
                InstKind::Store(val, _) if *val == alloca_id => {
                    return false;
                }
                // Call/RuntimeCall with the alloca as an argument = escape.
                InstKind::Call(_, args) if args.contains(&alloca_id) => {
                    return false;
                }
                InstKind::RuntimeCall(_, args) if args.contains(&alloca_id) => {
                    return false;
                }
                // Any other instruction that uses the alloca = ineligible.
                // (Includes: direct store TO the aggregate base, arithmetic on the pointer, etc.)
                _ => {
                    return false;
                }
            }
        }
    }
    true
}

fn gep_result_escapes(func: &Function, gep_id: ValueId) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            let uses = inst_uses(&inst.kind);
            if !uses.contains(&gep_id) {
                continue;
            }
            match &inst.kind {
                InstKind::Load(ptr) if *ptr == gep_id => {}
                InstKind::Store(_, ptr) if *ptr == gep_id => {}
                _ => return true,
            }
        }
        if let Some(term) = &block.terminator {
            match term {
                Terminator::Return(Some(v)) if *v == gep_id => return true,
                Terminator::Branch(_, args) if args.contains(&gep_id) => return true,
                Terminator::CondBranch { cond, true_args, false_args, .. } => {
                    if *cond == gep_id || true_args.contains(&gep_id) || false_args.contains(&gep_id) {
                        return true;
                    }
                }
                Terminator::Switch { selector, .. } => {
                    if *selector == gep_id {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// Decompose one alloca into individual scalar allocas.
fn decompose_alloca(func: &mut Function, cand: &SroaCandidate) -> bool {
    // Create N individual allocas, one per field.
    let mut field_allocas: Vec<ValueId> = Vec::new();
    let span = func.block(cand.alloca_block).insts[cand.alloca_inst_idx].span;

    // Create field allocas and insert them right after the original alloca
    // so they dominate all subsequent uses.
    let insert_pos = cand.alloca_inst_idx + 1;
    for i in 0..cand.count {
        let new_id = func.next_value_id();
        let ptr_ty = IrType::Ptr(Box::new(cand.elem_ty.clone()));
        func.register_type(new_id, ptr_ty.clone());
        func.block_mut(cand.alloca_block).insts.insert(insert_pos + i as usize, Inst {
            id: new_id,
            kind: InstKind::Alloca(cand.elem_ty.clone()),
            ty: ptr_ty,
            span,
        });
        field_allocas.push(new_id);
    }

    // Build a GEP→field_alloca mapping: for each GEP(alloca, [const_idx]),
    // replace the GEP result with the corresponding field alloca.
    let mut gep_to_field: HashMap<ValueId, ValueId> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::GetElementPtr(base, indices) = &inst.kind {
                if *base == cand.alloca_id && indices.len() == 1 {
                    if let Some(idx) = resolve_const_int(func, indices[0]) {
                        if idx >= 0 && (idx as u64) < cand.count {
                            gep_to_field.insert(inst.id, field_allocas[idx as usize]);
                        }
                    }
                }
            }
        }
    }

    if gep_to_field.is_empty() { return false; }

    // Rewrite all uses of GEP results to use the field allocas.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            // Replace operands that reference GEP results.
            let mut new_kind = inst.kind.clone();
            let mut replaced = false;
            match &mut new_kind {
                InstKind::Load(ptr) => {
                    if let Some(&field) = gep_to_field.get(ptr) {
                        *ptr = field;
                        replaced = true;
                    }
                }
                InstKind::Store(_, ptr) => {
                    if let Some(&field) = gep_to_field.get(ptr) {
                        *ptr = field;
                        replaced = true;
                    }
                }
                _ => {}
            }
            if replaced {
                inst.kind = new_kind;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IntWidth;
    use crate::opt::pass::Pass;
    use crate::lexer::{Span, Position};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    #[test]
    fn sroa_no_op_on_scalars() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let a = f.next_value_id();
        f.register_type(a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: a, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), span: span(),
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Sroa;
        assert!(!pass.run(&mut m), "scalar alloca should not be decomposed");
    }

    #[test]
    fn sroa_rejects_gep_address_escape() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let arr_ty = IrType::Array(
            Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
            2,
        );
        let arr = f.next_value_id();
        f.register_type(arr, IrType::Ptr(Box::new(arr_ty.clone())));
        f.block_mut(f.entry).insts.push(Inst {
            id: arr,
            ty: IrType::Ptr(Box::new(arr_ty)),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(
                Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))),
                2,
            )),
        });

        let zero = f.next_value_id();
        f.register_type(zero, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: zero,
            ty: IrType::Int(IntWidth::I64),
            span: span(),
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });

        let gep = f.next_value_id();
        f.register_type(gep, IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))));
        f.block_mut(f.entry).insts.push(Inst {
            id: gep,
            ty: IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))),
            span: span(),
            kind: InstKind::GetElementPtr(arr, vec![zero]),
        });

        let sink = f.next_value_id();
        f.register_type(sink, IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))))));
        f.block_mut(f.entry).insts.push(Inst {
            id: sink,
            ty: IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))))),
            span: span(),
            kind: InstKind::Alloca(IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))))),
        });

        let escape_store = f.next_value_id();
        f.register_type(escape_store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: escape_store,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(gep, sink),
        });

        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Sroa.run(&mut m), "GEP addresses that escape should block SROA");
    }
}
