//! True NEON loop vectorizer (Sprint 12 Stage 4 MVP).
//!
//! Detects counted DO loops with statically-known trip count divisible
//! by the NEON lane count V, and rewrites the inner body to consume and
//! produce vector IR (`VLoad`/`VAdd`/`VStore`/...). The downstream
//! `isel` pass then emits real NEON intrinsics.
//!
//! This is the *real* vectorizer. The older `vectorize.rs` (which
//! batches scalar dispatch through runtime kernel calls) remains as a
//! fallback for shapes this MVP does not yet handle: mismatched trip
//! counts (no scalar tail yet), multi-statement bodies, reductions,
//! WHERE masks. Stages 5–6 of the sprint plan extend this pass to those
//! cases.

use std::collections::{HashMap, HashSet};

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};

use super::loop_utils::{find_preheader, loop_defined_values, resolve_const_int};
use super::pass::Pass;
use super::util::{find_natural_loops, inst_uses, predecessors, terminator_uses, NaturalLoop};

pub struct NeonVectorize;

impl Pass for NeonVectorize {
    fn name(&self) -> &'static str {
        "neon_vectorize"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            while vectorize_one_loop(func) {
                changed = true;
            }
        }
        if changed {
            for func in &mut module.functions {
                func.rebuild_type_cache();
            }
        }
        changed
    }
}

#[derive(Debug, Clone, Copy)]
struct CountedLoop {
    preheader: BlockId,
    header: BlockId,
    body: BlockId,
    iv_param: ValueId,
    iv_init: i64,
    iv_bound: i64,
}

#[derive(Debug, Clone)]
struct ArrayAccess {
    base: ValueId,
    elem_ty: IrType,
    len: u64,
    lower: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryKind {
    Add,
    Sub,
    Mul,
}

/// Concrete plan: single-statement element-wise body with two array
/// loads plus one array store.
#[derive(Debug, Clone)]
struct VectorPlan {
    lanes: u8,
    elem_ty: IrType,
    /// Original Load instruction IDs to be rewritten to VLoad.
    loads: Vec<ValueId>,
    /// Original binop instruction ID and its op kind.
    binop: (ValueId, BinaryKind),
    /// Original Store instruction ID to be rewritten to VStore.
    store: ValueId,
    /// Original `iadd iv, 1` step instruction in the body.
    step_iadd: ValueId,
    /// The `1` ConstInt used as the step (for replacement with V).
    step_const: ValueId,
    /// Width of the IV ConstInt (i32 for typical 1..N loops).
    iv_int_width: IntWidth,
    /// Span to use for synthesised instructions.
    span: crate::lexer::Span,
}

fn vectorize_one_loop(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    if loops.is_empty() {
        return false;
    }
    let preds = predecessors(func);

    for lp in &loops {
        let Some(shape) = detect_counted_loop(func, lp, &preds) else {
            continue;
        };
        let loop_defs = loop_defined_values(func, lp);
        if loop_values_escape(func, lp, &loop_defs) {
            continue;
        }
        let Some(plan) = build_vector_plan(func, &shape, &loop_defs) else {
            continue;
        };
        apply_vector_plan(func, &shape, plan);
        return true;
    }
    false
}

fn detect_counted_loop(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<CountedLoop> {
    if lp.latches.len() != 1 || lp.body.len() != 2 {
        return None;
    }
    let header = lp.header;
    let body = lp.latches[0];
    if body == header {
        return None;
    }
    let header_block = func.block(header);
    if header_block.params.len() != 1 {
        return None;
    }
    let iv_param = header_block.params[0].id;
    if !matches!(header_block.params[0].ty, IrType::Int(_)) {
        return None;
    }
    let preheader = find_preheader(func, lp, preds)?;
    let iv_init = match &func.block(preheader).terminator {
        Some(Terminator::Branch(dest, args)) if *dest == header && args.len() == 1 => {
            resolve_const_int(func, args[0])?
        }
        _ => return None,
    };
    let (cond_id, true_dest, false_dest, true_args, false_args) = match &header_block.terminator {
        Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) => (*cond, *true_dest, *false_dest, true_args, false_args),
        _ => return None,
    };
    if !true_args.is_empty()
        || !false_args.is_empty()
        || true_dest != body
        || lp.body.contains(&false_dest)
    {
        return None;
    }
    let cond_inst = header_block.insts.iter().find(|inst| inst.id == cond_id)?;
    let iv_bound = match cond_inst.kind {
        InstKind::ICmp(CmpOp::Le, lhs, rhs) if lhs == iv_param => resolve_const_int(func, rhs)?,
        InstKind::ICmp(CmpOp::Lt, lhs, rhs) if lhs == iv_param => {
            resolve_const_int(func, rhs)?.checked_sub(1)?
        }
        _ => return None,
    };
    Some(CountedLoop {
        preheader,
        header,
        body,
        iv_param,
        iv_init,
        iv_bound,
    })
}

fn build_vector_plan(
    func: &Function,
    shape: &CountedLoop,
    loop_defs: &HashSet<ValueId>,
) -> Option<VectorPlan> {
    let body = func.block(shape.body);

    // Reject loops with calls in the body — too risky to vectorize.
    if body
        .insts
        .iter()
        .any(|inst| matches!(inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)))
    {
        return None;
    }

    // Find the unique store instruction.
    let mut stores = body.insts.iter().filter_map(|inst| match inst.kind {
        InstKind::Store(value, ptr) => Some((inst.id, inst.span, value, ptr)),
        _ => None,
    });
    let (store_id, span, stored_value, dest_ptr) = stores.next()?;
    if stores.next().is_some() {
        return None;
    }

    let dest = classify_array_access(func, dest_ptr, shape.iv_param)?;
    if !covers_full_array(shape, &dest) {
        return None;
    }
    let lanes = lane_count_for(&dest.elem_ty)?;
    if (dest.len as u64) % (lanes as u64) != 0 {
        return None;
    }
    let trip = shape
        .iv_bound
        .checked_sub(shape.iv_init)
        .and_then(|d| d.checked_add(1))?;
    if trip <= 0 || (trip as u64) % (lanes as u64) != 0 {
        return None;
    }

    // Decode `stored_value` as a binop on two array loads.
    let defs = inst_map(func);
    let binop = defs.get(&stored_value)?;
    let (kind, lhs_v, rhs_v) = match binop.kind {
        InstKind::IAdd(l, r) => (BinaryKind::Add, l, r),
        InstKind::ISub(l, r) => (BinaryKind::Sub, l, r),
        InstKind::IMul(l, r) => (BinaryKind::Mul, l, r),
        InstKind::FAdd(l, r) => (BinaryKind::Add, l, r),
        InstKind::FSub(l, r) => (BinaryKind::Sub, l, r),
        InstKind::FMul(l, r) => (BinaryKind::Mul, l, r),
        _ => return None,
    };

    let lhs_load = classify_loaded_array(func, lhs_v, shape.iv_param)?;
    let rhs_load = classify_loaded_array(func, rhs_v, shape.iv_param)?;
    if !arrays_compatible(&dest, &lhs_load.access) || !arrays_compatible(&dest, &rhs_load.access) {
        return None;
    }
    let _ = loop_defs; // suppression: helper is used to validate escape only above.

    // Find the iv-increment in the body.
    let body_term = match &body.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == shape.header && args.len() == 1 => {
            args[0]
        }
        _ => return None,
    };
    let step_inst = defs.get(&body_term)?;
    let (step_lhs, step_rhs) = match step_inst.kind {
        InstKind::IAdd(l, r) => (l, r),
        _ => return None,
    };
    let (step_const, _step_value) = if step_lhs == shape.iv_param {
        (step_rhs, resolve_const_int(func, step_rhs)?)
    } else if step_rhs == shape.iv_param {
        (step_lhs, resolve_const_int(func, step_lhs)?)
    } else {
        return None;
    };

    // Pull the IV's int width from the ConstInt that defines the step.
    let iv_int_width = match defs.get(&step_const)?.kind {
        InstKind::ConstInt(_, w) => w,
        _ => return None,
    };

    Some(VectorPlan {
        lanes,
        elem_ty: dest.elem_ty,
        loads: vec![lhs_load.load_id, rhs_load.load_id],
        binop: (stored_value, kind),
        store: store_id,
        step_iadd: step_inst.id,
        step_const,
        iv_int_width,
        span,
    })
}

#[derive(Debug, Clone)]
struct LoadedArray {
    load_id: ValueId,
    access: ArrayAccess,
}

fn classify_loaded_array(
    func: &Function,
    value: ValueId,
    iv_param: ValueId,
) -> Option<LoadedArray> {
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    let InstKind::Load(ptr) = inst.kind else {
        return None;
    };
    let access = classify_array_access(func, ptr, iv_param)?;
    Some(LoadedArray {
        load_id: inst.id,
        access,
    })
}

fn classify_array_access(func: &Function, ptr: ValueId, iv_param: ValueId) -> Option<ArrayAccess> {
    let defs = inst_map(func);
    let inst = defs.get(&ptr)?;
    let InstKind::GetElementPtr(base, ref indices) = inst.kind else {
        return None;
    };
    if indices.len() != 1 {
        return None;
    }
    let IrType::Ptr(inner) = func.value_type(base)? else {
        return None;
    };
    let IrType::Array(elem, len) = inner.as_ref() else {
        return None;
    };
    let lower = normalized_index_lower(func, indices[0], iv_param)?;
    Some(ArrayAccess {
        base,
        elem_ty: elem.as_ref().clone(),
        len: *len,
        lower,
    })
}

fn normalized_index_lower(func: &Function, value: ValueId, iv_param: ValueId) -> Option<i64> {
    if value == iv_param {
        return Some(0);
    }
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    match inst.kind {
        InstKind::IntExtend(src, IntWidth::I64, _) if src == iv_param => Some(0),
        InstKind::ISub(lhs, rhs) => {
            let lhs_lower = normalized_index_lower(func, lhs, iv_param)?;
            let rhs_const = resolve_const_int(func, rhs)?;
            lhs_lower.checked_add(rhs_const)
        }
        _ => None,
    }
}

fn arrays_compatible(dest: &ArrayAccess, other: &ArrayAccess) -> bool {
    dest.elem_ty == other.elem_ty && dest.len == other.len && dest.lower == other.lower
}

fn covers_full_array(shape: &CountedLoop, access: &ArrayAccess) -> bool {
    if access.len == 0 {
        return false;
    }
    let Some(upper) = access
        .lower
        .checked_add(access.len as i64)
        .and_then(|value| value.checked_sub(1))
    else {
        return false;
    };
    shape.iv_init == access.lower && shape.iv_bound == upper
}

fn loop_values_escape(func: &Function, lp: &NaturalLoop, loop_defs: &HashSet<ValueId>) -> bool {
    for block in &func.blocks {
        if lp.body.contains(&block.id) {
            continue;
        }
        if block.insts.iter().any(|inst| {
            inst_uses(&inst.kind)
                .into_iter()
                .any(|value| loop_defs.contains(&value))
        }) {
            return true;
        }
        if block.terminator.as_ref().is_some_and(|term| {
            terminator_uses(term)
                .into_iter()
                .any(|value| loop_defs.contains(&value))
        }) {
            return true;
        }
    }
    false
}

fn lane_count_for(elem: &IrType) -> Option<u8> {
    match elem {
        IrType::Int(IntWidth::I32) => Some(4),
        IrType::Int(IntWidth::I64) => Some(2),
        IrType::Float(FloatWidth::F32) => Some(4),
        IrType::Float(FloatWidth::F64) => Some(2),
        _ => None,
    }
}

fn vector_ty(elem: &IrType, lanes: u8) -> IrType {
    IrType::Vector {
        lanes,
        elem: Box::new(elem.clone()),
    }
}

fn apply_vector_plan(func: &mut Function, shape: &CountedLoop, plan: VectorPlan) {
    let v_ty = vector_ty(&plan.elem_ty, plan.lanes);

    // 1. Replace the step `iadd iv, 1` constant operand with V (using
    //    a fresh ConstInt to avoid clobbering shared `1` constants).
    let new_step_const = func.next_value_id();
    let step_const_ty = IrType::Int(plan.iv_int_width);
    func.register_type(new_step_const, step_const_ty.clone());
    let body_block = func.block_mut(shape.body);
    // Insert the new const at the top of the body block.
    body_block.insts.insert(
        0,
        Inst {
            id: new_step_const,
            kind: InstKind::ConstInt(plan.lanes as i128, plan.iv_int_width),
            ty: step_const_ty,
            span: plan.span,
        },
    );
    // Update the iadd to reference the new const.
    if let Some(step_inst) = body_block
        .insts
        .iter_mut()
        .find(|inst| inst.id == plan.step_iadd)
    {
        if let InstKind::IAdd(ref mut l, ref mut r) = step_inst.kind {
            if *l == plan.step_const {
                *l = new_step_const;
            }
            if *r == plan.step_const {
                *r = new_step_const;
            }
        }
    }

    // 2. Rewrite the two scalar Loads into VLoads with vector type.
    for &load_id in &plan.loads {
        let body_block = func.block_mut(shape.body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
            if let InstKind::Load(ptr) = inst.kind {
                inst.kind = InstKind::VLoad(ptr);
                inst.ty = v_ty.clone();
            }
        }
        func.register_type(load_id, v_ty.clone());
    }

    // 3. Rewrite the binop into the matching V-op.
    let (binop_id, binop_kind) = plan.binop;
    {
        let body_block = func.block_mut(shape.body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == binop_id) {
            let new_kind = match (inst.kind.clone(), binop_kind) {
                (InstKind::IAdd(l, r), BinaryKind::Add)
                | (InstKind::FAdd(l, r), BinaryKind::Add) => InstKind::VAdd(l, r),
                (InstKind::ISub(l, r), BinaryKind::Sub)
                | (InstKind::FSub(l, r), BinaryKind::Sub) => InstKind::VSub(l, r),
                (InstKind::IMul(l, r), BinaryKind::Mul)
                | (InstKind::FMul(l, r), BinaryKind::Mul) => InstKind::VMul(l, r),
                _ => inst.kind.clone(),
            };
            inst.kind = new_kind;
            inst.ty = v_ty.clone();
        }
    }
    func.register_type(binop_id, v_ty.clone());

    // 4. Rewrite the Store into a VStore.
    let body_block = func.block_mut(shape.body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == plan.store) {
        if let InstKind::Store(val, ptr) = inst.kind {
            inst.kind = InstKind::VStore(val, ptr);
        }
    }
}

fn inst_map(func: &Function) -> HashMap<ValueId, &Inst> {
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| (inst.id, inst))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

    fn dummy_span() -> Span {
        let p = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: p,
            end: p,
        }
    }

    fn push_inst(func: &mut Function, block: BlockId, kind: InstKind, ty: IrType) -> ValueId {
        let id = func.next_value_id();
        func.register_type(id, ty.clone());
        func.block_mut(block).insts.push(Inst {
            id,
            kind,
            ty,
            span: dummy_span(),
        });
        id
    }

    /// Build the canonical `c(i) = a(i) + b(i)` loop over i32 arrays of
    /// length 32 (trip count divisible by V=4).
    fn build_array_add_loop() -> (Module, BlockId) {
        let mut module = Module::new("m".into());
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());
        let b = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());
        let c = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());

        let one_i32 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let hi_i32 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(32, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let one_i64 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        func.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![one_i32]));

        let iv = func.next_value_id();
        func.register_type(iv, IrType::Int(IntWidth::I32));
        func.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        let cmp = push_inst(
            &mut func,
            header,
            InstKind::ICmp(CmpOp::Le, iv, hi_i32),
            IrType::Bool,
        );
        func.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let idx64 = push_inst(
            &mut func,
            body,
            InstKind::IntExtend(iv, IntWidth::I64, true),
            IrType::Int(IntWidth::I64),
        );
        let offset = push_inst(
            &mut func,
            body,
            InstKind::ISub(idx64, one_i64),
            IrType::Int(IntWidth::I64),
        );
        let elem_ptr_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)));
        let a_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(a, vec![offset]),
            elem_ptr_ty.clone(),
        );
        let a_val = push_inst(
            &mut func,
            body,
            InstKind::Load(a_ptr),
            IrType::Int(IntWidth::I32),
        );
        let b_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(b, vec![offset]),
            elem_ptr_ty.clone(),
        );
        let b_val = push_inst(
            &mut func,
            body,
            InstKind::Load(b_ptr),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut func,
            body,
            InstKind::IAdd(a_val, b_val),
            IrType::Int(IntWidth::I32),
        );
        let c_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(c, vec![offset]),
            elem_ptr_ty.clone(),
        );
        push_inst(&mut func, body, InstKind::Store(sum, c_ptr), IrType::Void);
        let next = push_inst(
            &mut func,
            body,
            InstKind::IAdd(iv, one_i32),
            IrType::Int(IntWidth::I32),
        );
        func.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next]));
        func.block_mut(exit).terminator = Some(Terminator::Return(None));
        module.add_function(func);
        (module, body)
    }

    #[test]
    fn rewrites_array_add_loop_to_vload_vadd_vstore() {
        let (mut module, body) = build_array_add_loop();
        let changed = NeonVectorize.run(&mut module);
        assert!(changed, "neon_vectorize should fire on a clean array-add loop");

        let func = &module.functions[0];
        let body_block = func.block(body);

        let n_vload = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VLoad(_)))
            .count();
        assert_eq!(n_vload, 2, "two scalar Loads should become VLoads");

        let n_vadd = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VAdd(..)))
            .count();
        assert_eq!(n_vadd, 1, "the IAdd should become a VAdd");

        let n_vstore = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VStore(..)))
            .count();
        assert_eq!(n_vstore, 1, "the Store should become a VStore");

        // The loaded values should now have vector type.
        for inst in &body_block.insts {
            if let InstKind::VLoad(_) = inst.kind {
                assert_eq!(
                    inst.ty,
                    IrType::Vector { lanes: 4, elem: Box::new(IrType::Int(IntWidth::I32)) }
                );
            }
        }

        // The IV step should now use a ConstInt(4) somewhere in the body.
        let has_v_step = body_block.insts.iter().any(|i| {
            matches!(i.kind, InstKind::ConstInt(4, IntWidth::I32))
        });
        assert!(has_v_step, "step should now be ConstInt(4)");
    }

    #[test]
    fn rejects_non_divisible_trip_count() {
        // length 31 → not divisible by V=4. The pass should bail out
        // and leave the loop alone.
        let mut module = Module::new("m".into());
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 31);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());
        let b = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());
        let c = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());

        let one_i32 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let hi_i32 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(31, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let one_i64 = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        func.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![one_i32]));

        let iv = func.next_value_id();
        func.register_type(iv, IrType::Int(IntWidth::I32));
        func.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        let cmp = push_inst(
            &mut func,
            header,
            InstKind::ICmp(CmpOp::Le, iv, hi_i32),
            IrType::Bool,
        );
        func.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let idx64 = push_inst(
            &mut func,
            body,
            InstKind::IntExtend(iv, IntWidth::I64, true),
            IrType::Int(IntWidth::I64),
        );
        let offset = push_inst(
            &mut func,
            body,
            InstKind::ISub(idx64, one_i64),
            IrType::Int(IntWidth::I64),
        );
        let elem_ptr_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)));
        let a_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(a, vec![offset]),
            elem_ptr_ty.clone(),
        );
        let a_val = push_inst(
            &mut func,
            body,
            InstKind::Load(a_ptr),
            IrType::Int(IntWidth::I32),
        );
        let b_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(b, vec![offset]),
            elem_ptr_ty.clone(),
        );
        let b_val = push_inst(
            &mut func,
            body,
            InstKind::Load(b_ptr),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut func,
            body,
            InstKind::IAdd(a_val, b_val),
            IrType::Int(IntWidth::I32),
        );
        let c_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(c, vec![offset]),
            elem_ptr_ty.clone(),
        );
        push_inst(&mut func, body, InstKind::Store(sum, c_ptr), IrType::Void);
        let next = push_inst(
            &mut func,
            body,
            InstKind::IAdd(iv, one_i32),
            IrType::Int(IntWidth::I32),
        );
        func.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next]));
        func.block_mut(exit).terminator = Some(Terminator::Return(None));
        module.add_function(func);

        let changed = NeonVectorize.run(&mut module);
        assert!(!changed, "trip count not divisible by V should not be vectorized");
    }
}
