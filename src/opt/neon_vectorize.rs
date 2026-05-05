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

/// One operand of the body's binop, classified as either an array
/// `Load` that becomes a `VLoad` or a loop-invariant scalar that
/// becomes a `VBroadcast` hoisted into the preheader.
#[derive(Debug, Clone)]
enum BinopOperand {
    /// A scalar `Load` whose pointer is `gep base, [iv-derived]`.
    /// `load_id` is the original IR Load instruction we'll rewrite
    /// to a VLoad.
    ArrayLoad(ValueId),
    /// A loop-invariant scalar value defined outside the loop. We
    /// will emit a `VBroadcast` in the preheader to splat it across
    /// every lane and rewrite the binop to consume that vector.
    InvariantScalar(ValueId),
}

/// What kind of element-wise op the loop body computes.
#[derive(Debug, Clone)]
enum BodyOp {
    /// `dest(i) = source` — a pure copy of one array load (no
    /// arithmetic). The Load inst gets rewritten to VLoad and its
    /// result is stored directly. `InvariantScalar` is rejected
    /// here: a constant fill goes through the older bulk path.
    Copy { source: BinopOperand },
    /// `dest(i) = lhs op rhs` — a single element-wise binop with at
    /// least one array load.
    Binop {
        lhs: BinopOperand,
        rhs: BinopOperand,
        binop_id: ValueId,
        kind: BinaryKind,
    },
}

/// Concrete plan: single-statement element-wise body with up to
/// two array loads (or one load + one invariant scalar) plus one
/// array store.
#[derive(Debug, Clone)]
struct VectorPlan {
    lanes: u8,
    elem_ty: IrType,
    /// What expression feeds the store.
    op: BodyOp,
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

    // Decode `stored_value` as either a pure load (copy form) or a
    // binop on two operands (which may each be an array load or an
    // invariant scalar).
    let defs = inst_map(func);
    let stored_inst = defs.get(&stored_value)?;
    let op = match stored_inst.kind {
        InstKind::Load(_) => {
            let source = classify_binop_operand(
                func,
                stored_value,
                shape.iv_param,
                &dest,
                loop_defs,
            )?;
            // A copy must have a real array load on the right; an
            // invariant scalar splat goes through the older bulk
            // path.
            match source {
                BinopOperand::ArrayLoad(_) => BodyOp::Copy { source },
                BinopOperand::InvariantScalar(_) => return None,
            }
        }
        InstKind::IAdd(l, r) => {
            binop_body(stored_value, BinaryKind::Add, l, r, func, shape, &dest, loop_defs)?
        }
        InstKind::ISub(l, r) => {
            binop_body(stored_value, BinaryKind::Sub, l, r, func, shape, &dest, loop_defs)?
        }
        InstKind::IMul(l, r) => {
            binop_body(stored_value, BinaryKind::Mul, l, r, func, shape, &dest, loop_defs)?
        }
        InstKind::FAdd(l, r) => {
            binop_body(stored_value, BinaryKind::Add, l, r, func, shape, &dest, loop_defs)?
        }
        InstKind::FSub(l, r) => {
            binop_body(stored_value, BinaryKind::Sub, l, r, func, shape, &dest, loop_defs)?
        }
        InstKind::FMul(l, r) => {
            binop_body(stored_value, BinaryKind::Mul, l, r, func, shape, &dest, loop_defs)?
        }
        _ => return None,
    };

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
        op,
        store: store_id,
        step_iadd: step_inst.id,
        step_const,
        iv_int_width,
        span,
    })
}

/// Classify a body that is `dest(i) = lhs op rhs`. At least one
/// side must be an array load — the all-scalar form has no business
/// being a vectorizable counted loop.
fn binop_body(
    binop_id: ValueId,
    kind: BinaryKind,
    lhs_v: ValueId,
    rhs_v: ValueId,
    func: &Function,
    shape: &CountedLoop,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
) -> Option<BodyOp> {
    let lhs_op = classify_binop_operand(func, lhs_v, shape.iv_param, dest, loop_defs)?;
    let rhs_op = classify_binop_operand(func, rhs_v, shape.iv_param, dest, loop_defs)?;
    if matches!(lhs_op, BinopOperand::InvariantScalar(_))
        && matches!(rhs_op, BinopOperand::InvariantScalar(_))
    {
        return None;
    }
    Some(BodyOp::Binop {
        lhs: lhs_op,
        rhs: rhs_op,
        binop_id,
        kind,
    })
}

/// Classify one operand of the body's binop as either a load from
/// the destination array's iteration space (which becomes a `VLoad`)
/// or a value defined entirely outside the loop (which becomes a
/// preheader `VBroadcast`).
fn classify_binop_operand(
    func: &Function,
    value: ValueId,
    iv_param: ValueId,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
) -> Option<BinopOperand> {
    if let Some(load) = classify_loaded_array(func, value, iv_param) {
        if !arrays_compatible(dest, &load.access) {
            return None;
        }
        return Some(BinopOperand::ArrayLoad(load.load_id));
    }
    // Not an array load: only valid if it is loop-invariant.
    if loop_defs.contains(&value) {
        return None;
    }
    // Type must match the destination element type so the broadcast
    // produces a vector compatible with the rewritten binop.
    let ty = func.value_type(value)?;
    if ty != dest.elem_ty {
        return None;
    }
    Some(BinopOperand::InvariantScalar(value))
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

    // 2. Walk every operand of the body op. For `ArrayLoad`s, rewrite
    //    the scalar Load to a VLoad in place. For `InvariantScalar`s,
    //    emit a `VBroadcast` in the preheader and remember the new
    //    vector value so we can swap it into the binop.
    for op in op_operands(&plan.op) {
        rewrite_array_load(func, shape.body, op, &v_ty);
    }
    let (lhs_subst, rhs_subst) = match &plan.op {
        BodyOp::Copy { .. } => (None, None),
        BodyOp::Binop { lhs, rhs, .. } => (
            broadcast_if_invariant(func, shape.preheader, lhs, &v_ty, plan.span),
            broadcast_if_invariant(func, shape.preheader, rhs, &v_ty, plan.span),
        ),
    };

    // 3. Rewrite the binop into the matching V-op, swapping in the
    //    broadcast vectors for any invariant-scalar operands. Pure
    //    `Copy` form has no binop to rewrite.
    if let BodyOp::Binop {
        binop_id,
        kind: binop_kind,
        ..
    } = &plan.op
    {
        let body_block = func.block_mut(shape.body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == *binop_id) {
            let new_kind = match (inst.kind.clone(), binop_kind) {
                (InstKind::IAdd(l, r), BinaryKind::Add)
                | (InstKind::FAdd(l, r), BinaryKind::Add) => {
                    InstKind::VAdd(lhs_subst.unwrap_or(l), rhs_subst.unwrap_or(r))
                }
                (InstKind::ISub(l, r), BinaryKind::Sub)
                | (InstKind::FSub(l, r), BinaryKind::Sub) => {
                    InstKind::VSub(lhs_subst.unwrap_or(l), rhs_subst.unwrap_or(r))
                }
                (InstKind::IMul(l, r), BinaryKind::Mul)
                | (InstKind::FMul(l, r), BinaryKind::Mul) => {
                    InstKind::VMul(lhs_subst.unwrap_or(l), rhs_subst.unwrap_or(r))
                }
                _ => inst.kind.clone(),
            };
            inst.kind = new_kind;
            inst.ty = v_ty.clone();
        }
        func.register_type(*binop_id, v_ty.clone());
    }

    // 4. Rewrite the Store into a VStore.
    let body_block = func.block_mut(shape.body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == plan.store) {
        if let InstKind::Store(val, ptr) = inst.kind {
            inst.kind = InstKind::VStore(val, ptr);
        }
    }
}

/// Iterate the operands of a body op (one for `Copy`, two for `Binop`).
fn op_operands(op: &BodyOp) -> Vec<&BinopOperand> {
    match op {
        BodyOp::Copy { source } => vec![source],
        BodyOp::Binop { lhs, rhs, .. } => vec![lhs, rhs],
    }
}

/// If `op` is an `ArrayLoad`, rewrite its scalar Load to a VLoad and
/// register the load's type as the vector type.
fn rewrite_array_load(
    func: &mut Function,
    body: BlockId,
    op: &BinopOperand,
    v_ty: &IrType,
) {
    let load_id = match op {
        BinopOperand::ArrayLoad(id) => *id,
        BinopOperand::InvariantScalar(_) => return,
    };
    let body_block = func.block_mut(body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
        if let InstKind::Load(ptr) = inst.kind {
            inst.kind = InstKind::VLoad(ptr);
            inst.ty = v_ty.clone();
        }
    }
    func.register_type(load_id, v_ty.clone());
}

/// If `op` is an `InvariantScalar`, append a `VBroadcast` to the
/// loop's preheader (just before its terminator) and return the
/// resulting vector value. Returns `None` for `ArrayLoad` operands —
/// those are rewritten in place by the load loop.
fn broadcast_if_invariant(
    func: &mut Function,
    preheader: BlockId,
    op: &BinopOperand,
    v_ty: &IrType,
    span: crate::lexer::Span,
) -> Option<ValueId> {
    let scalar = match op {
        BinopOperand::InvariantScalar(v) => *v,
        BinopOperand::ArrayLoad(_) => return None,
    };
    let new_id = func.next_value_id();
    func.register_type(new_id, v_ty.clone());
    let pre_block = func.block_mut(preheader);
    // Insert the broadcast just before the preheader's terminator
    // (which is the unconditional branch into the header).
    let pos = pre_block.insts.len();
    pre_block.insts.insert(
        pos,
        Inst {
            id: new_id,
            kind: InstKind::VBroadcast(scalar),
            ty: v_ty.clone(),
            span,
        },
    );
    Some(new_id)
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

    /// Build `c(i) = a(i) + scale` over i32(32) where `scale` is a
    /// loop-invariant ConstInt defined in the entry/preheader. The
    /// vectorizer should classify `scale` as `InvariantScalar`, hoist
    /// a `VBroadcast` into the preheader, and rewrite the binop to
    /// consume the broadcast vector.
    fn build_array_add_scalar_loop() -> (Module, BlockId, BlockId) {
        let mut module = Module::new("m".into());
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());
        let c = push_inst(&mut func, entry, InstKind::Alloca(arr_ty.clone()), arr_ptr_ty.clone());

        let scale = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
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
        let sum = push_inst(
            &mut func,
            body,
            InstKind::IAdd(a_val, scale),
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
        (module, entry, body)
    }

    #[test]
    fn broadcasts_invariant_scalar_into_preheader() {
        let (mut module, preheader, body) = build_array_add_scalar_loop();
        let changed = NeonVectorize.run(&mut module);
        assert!(
            changed,
            "neon_vectorize should fire on a(i) + invariant scalar"
        );

        let func = &module.functions[0];
        let pre_block = func.block(preheader);
        let body_block = func.block(body);

        let n_vbroadcast = pre_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VBroadcast(_)))
            .count();
        assert_eq!(
            n_vbroadcast, 1,
            "the invariant scalar should be broadcast once in the preheader"
        );

        let n_vload = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VLoad(_)))
            .count();
        assert_eq!(n_vload, 1, "only the array operand becomes a VLoad");

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
    }

    /// Build `c(i) = b(i)` over i32(32) — a pure array copy with no
    /// arithmetic between the load and the store.
    fn build_array_copy_loop() -> (Module, BlockId) {
        let mut module = Module::new("m".into());
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
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
        let c_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(c, vec![offset]),
            elem_ptr_ty.clone(),
        );
        push_inst(&mut func, body, InstKind::Store(b_val, c_ptr), IrType::Void);
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
    fn rewrites_pure_array_copy_to_vload_vstore() {
        let (mut module, body) = build_array_copy_loop();
        let changed = NeonVectorize.run(&mut module);
        assert!(changed, "neon_vectorize should fire on a pure copy loop");

        let func = &module.functions[0];
        let body_block = func.block(body);

        let n_vload = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VLoad(_)))
            .count();
        assert_eq!(n_vload, 1, "the single Load becomes a VLoad");

        let n_vstore = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VStore(..)))
            .count();
        assert_eq!(n_vstore, 1, "the Store becomes a VStore");

        // No binop should appear — pure copy has none.
        let n_binop = body_block
            .insts
            .iter()
            .filter(|i| {
                matches!(
                    i.kind,
                    InstKind::VAdd(..) | InstKind::VSub(..) | InstKind::VMul(..)
                )
            })
            .count();
        assert_eq!(n_binop, 0, "pure copy must not introduce a v-binop");
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
