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
    /// Float-only — NEON has no integer vector divide.
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnaryKind {
    Neg,
    Abs,
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
    /// `dest(i) = -src` or `dest(i) = abs(src)` — single-operand
    /// element-wise op. `src` must be an `ArrayLoad` (negating an
    /// invariant scalar would be a constant fill).
    Unary {
        source: BinopOperand,
        unary_id: ValueId,
        kind: UnaryKind,
    },
    /// `dest(i) = lhs op rhs` — a single element-wise binop with at
    /// least one array load.
    Binop {
        lhs: BinopOperand,
        rhs: BinopOperand,
        binop_id: ValueId,
        kind: BinaryKind,
    },
}

/// One element-wise statement (one store) inside a multi-statement
/// vectorizable body. All statements in a `VectorPlan` share the
/// same lane count and element type.
#[derive(Debug, Clone)]
struct Statement {
    /// What expression feeds the store.
    op: BodyOp,
    /// Original Store instruction ID to be rewritten to VStore.
    store: ValueId,
}

/// Concrete plan: one or more element-wise statements (each up to
/// two array loads or one load + one invariant scalar plus one
/// array store) sharing the same iteration space and element type.
#[derive(Debug, Clone)]
struct VectorPlan {
    lanes: u8,
    elem_ty: IrType,
    /// Every statement that feeds a store in the body.
    statements: Vec<Statement>,
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
        // Try element-wise vectorization first (no escaping values).
        if let Some(shape) = detect_counted_loop(func, lp, &preds) {
            let loop_defs = loop_defined_values(func, lp);
            if !loop_values_escape(func, lp, &loop_defs) {
                if let Some(plan) = build_vector_plan(func, &shape, &loop_defs) {
                    apply_vector_plan(func, &shape, plan);
                    return true;
                }
            }
        }
        // Fall back: reduction loop (one escaping accumulator).
        if let Some(plan) = detect_reduction_plan(func, lp, &preds) {
            apply_reduction_plan(func, lp, plan);
            return true;
        }
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

    // Walk every store. Each one must have a destination access that
    // covers the full iteration space and an element type identical
    // to the first store's. Each statement is classified independently
    // (Copy or Binop) and contributes one entry to `statements`.
    let stores: Vec<(ValueId, crate::lexer::Span, ValueId, ValueId)> = body
        .insts
        .iter()
        .filter_map(|inst| match inst.kind {
            InstKind::Store(value, ptr) => Some((inst.id, inst.span, value, ptr)),
            _ => None,
        })
        .collect();
    if stores.is_empty() {
        return None;
    }

    // Pin the lane shape on the first destination; reject any later
    // store whose dest disagrees.
    let first_dest = classify_array_access(func, stores[0].3, shape.iv_param)?;
    if !covers_full_array(shape, &first_dest) {
        return None;
    }
    let lanes = lane_count_for(&first_dest.elem_ty)?;
    if (first_dest.len as u64) % (lanes as u64) != 0 {
        return None;
    }
    let trip = shape
        .iv_bound
        .checked_sub(shape.iv_init)
        .and_then(|d| d.checked_add(1))?;
    if trip <= 0 || (trip as u64) % (lanes as u64) != 0 {
        return None;
    }
    let elem_ty = first_dest.elem_ty.clone();
    let span = stores[0].1;

    let defs = inst_map(func);
    let mut statements = Vec::with_capacity(stores.len());
    for (store_id, _, stored_value, dest_ptr) in &stores {
        let dest = classify_array_access(func, *dest_ptr, shape.iv_param)?;
        if !covers_full_array(shape, &dest) {
            return None;
        }
        if dest.elem_ty != elem_ty {
            return None;
        }
        let stored_inst = defs.get(stored_value)?;
        let op = classify_body_op(*stored_value, &stored_inst.kind, func, shape, &dest, loop_defs)?;
        statements.push(Statement {
            op,
            store: *store_id,
        });
    }

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
        elem_ty,
        statements,
        step_iadd: step_inst.id,
        step_const,
        iv_int_width,
        span,
    })
}

/// Classify the expression `stored_value = ...` feeding one store as
/// either a `Copy` (pure load), a `Unary` (neg/abs), or a `Binop`.
/// Returns `None` for any shape we don't yet vectorize.
fn classify_body_op(
    stored_value: ValueId,
    kind: &InstKind,
    func: &Function,
    shape: &CountedLoop,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
) -> Option<BodyOp> {
    match kind {
        InstKind::Load(_) => {
            let source =
                classify_binop_operand(func, stored_value, shape.iv_param, dest, loop_defs)?;
            match source {
                BinopOperand::ArrayLoad(_) => Some(BodyOp::Copy { source }),
                BinopOperand::InvariantScalar(_) => None,
            }
        }
        InstKind::INeg(src) | InstKind::FNeg(src) => {
            unary_body(stored_value, UnaryKind::Neg, *src, func, shape, dest, loop_defs)
        }
        InstKind::FAbs(src) => {
            unary_body(stored_value, UnaryKind::Abs, *src, func, shape, dest, loop_defs)
        }
        InstKind::IAdd(l, r) => {
            binop_body(stored_value, BinaryKind::Add, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::ISub(l, r) => {
            binop_body(stored_value, BinaryKind::Sub, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::IMul(l, r) => {
            binop_body(stored_value, BinaryKind::Mul, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::FAdd(l, r) => {
            binop_body(stored_value, BinaryKind::Add, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::FSub(l, r) => {
            binop_body(stored_value, BinaryKind::Sub, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::FMul(l, r) => {
            binop_body(stored_value, BinaryKind::Mul, *l, *r, func, shape, dest, loop_defs)
        }
        InstKind::FDiv(l, r) => {
            // Integer divide has no NEON form; only floats. The
            // binop_body classifier doesn't itself check element
            // type, but `lane_count_for` already required the dest
            // to be Float for this code path to reach here.
            if !matches!(dest.elem_ty, IrType::Float(_)) {
                return None;
            }
            binop_body(stored_value, BinaryKind::Div, *l, *r, func, shape, dest, loop_defs)
        }
        _ => None,
    }
}

/// Classify a body that is `dest(i) = -src` or `dest(i) = abs(src)`.
/// `src` must be an array load (a unary on an invariant scalar would
/// just be a constant fill).
fn unary_body(
    unary_id: ValueId,
    kind: UnaryKind,
    src_v: ValueId,
    func: &Function,
    shape: &CountedLoop,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
) -> Option<BodyOp> {
    let source = classify_binop_operand(func, src_v, shape.iv_param, dest, loop_defs)?;
    match source {
        BinopOperand::ArrayLoad(_) => Some(BodyOp::Unary {
            source,
            unary_id,
            kind,
        }),
        BinopOperand::InvariantScalar(_) => None,
    }
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

    // 2. For each statement, vectorize: rewrite array loads to VLoads,
    //    emit any required preheader VBroadcasts, rewrite the binop
    //    into a v-op, and finally rewrite the store into a VStore.
    for stmt in plan.statements.clone() {
        for op in op_operands(&stmt.op) {
            rewrite_array_load(func, shape.body, op, &v_ty);
        }
        let (lhs_subst, rhs_subst) = match &stmt.op {
            BodyOp::Copy { .. } | BodyOp::Unary { .. } => (None, None),
            BodyOp::Binop { lhs, rhs, .. } => (
                broadcast_if_invariant(func, shape.preheader, lhs, &v_ty, plan.span),
                broadcast_if_invariant(func, shape.preheader, rhs, &v_ty, plan.span),
            ),
        };

        if let BodyOp::Unary {
            unary_id,
            kind: unary_kind,
            ..
        } = &stmt.op
        {
            let body_block = func.block_mut(shape.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == *unary_id) {
                let new_kind = match (inst.kind.clone(), unary_kind) {
                    (InstKind::INeg(s), UnaryKind::Neg)
                    | (InstKind::FNeg(s), UnaryKind::Neg) => InstKind::VNeg(s),
                    (InstKind::FAbs(s), UnaryKind::Abs) => InstKind::VAbs(s),
                    _ => inst.kind.clone(),
                };
                inst.kind = new_kind;
                inst.ty = v_ty.clone();
            }
            func.register_type(*unary_id, v_ty.clone());
        }

        if let BodyOp::Binop {
            binop_id,
            kind: binop_kind,
            ..
        } = &stmt.op
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
                    (InstKind::FDiv(l, r), BinaryKind::Div) => {
                        InstKind::VDiv(lhs_subst.unwrap_or(l), rhs_subst.unwrap_or(r))
                    }
                    _ => inst.kind.clone(),
                };
                inst.kind = new_kind;
                inst.ty = v_ty.clone();
            }
            func.register_type(*binop_id, v_ty.clone());
        }

        let body_block = func.block_mut(shape.body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == stmt.store) {
            if let InstKind::Store(val, ptr) = inst.kind {
                inst.kind = InstKind::VStore(val, ptr);
            }
        }
    }
}

/// Iterate the operands of a body op (one for `Copy`/`Unary`, two
/// for `Binop`).
fn op_operands(op: &BodyOp) -> Vec<&BinopOperand> {
    match op {
        BodyOp::Copy { source } | BodyOp::Unary { source, .. } => vec![source],
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

/// A sum-reduction loop:
///   `do i = lo, hi; s = s + a(i); end do` (or fadd for floats).
///
/// The loop header carries the IV and a scalar accumulator as block
/// params. The accumulator escapes the loop and is reduced to a
/// scalar `vreduce_sum` after the vectorized body.
#[derive(Debug, Clone)]
struct ReductionPlan {
    preheader: BlockId,
    header: BlockId,
    body: BlockId,
    /// Block reachable when the loop exits (false-dest of header
    /// cond_br).
    exit: BlockId,
    /// Block param indices in the header: `[iv_idx, acc_idx]`.
    iv_param: ValueId,
    acc_param: ValueId,
    acc_param_idx: usize,
    /// Scalar accumulator init value passed in the preheader's branch.
    acc_init: ValueId,
    /// Original Load instruction in the body.
    load_id: ValueId,
    /// Original `acc' = acc + load` instruction.
    accumulate_id: ValueId,
    /// Original `iv' = iv + 1` instruction.
    step_iadd: ValueId,
    /// The `1` ConstInt operand of the iv step.
    step_const: ValueId,
    /// IV ConstInt width.
    iv_int_width: IntWidth,
    /// Element type (i32 / i64 / f32 / f64).
    elem_ty: IrType,
    lanes: u8,
    span: crate::lexer::Span,
}

fn detect_reduction_plan(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<ReductionPlan> {
    if lp.latches.len() != 1 || lp.body.len() != 2 {
        return None;
    }
    let header = lp.header;
    let body = lp.latches[0];
    if body == header {
        return None;
    }

    let header_block = func.block(header);
    if header_block.params.len() != 2 {
        return None;
    }
    // Identify which param is the IV (int type, used as gep index)
    // and which is the accumulator. We require the IV to be param 0
    // in this MVP — Fortran's lowered form always emits IV first.
    let iv_param = header_block.params[0].id;
    let acc_param = header_block.params[1].id;
    let iv_int_width = match header_block.params[0].ty {
        IrType::Int(w) => w,
        _ => return None,
    };
    let acc_ty = header_block.params[1].ty.clone();
    let elem_ty = match acc_ty.clone() {
        IrType::Int(_) | IrType::Float(_) => acc_ty,
        _ => return None,
    };
    let lanes = lane_count_for(&elem_ty)?;

    let preheader = find_preheader(func, lp, preds)?;
    let (iv_init, acc_init) = match &func.block(preheader).terminator {
        Some(Terminator::Branch(dest, args)) if *dest == header && args.len() == 2 => {
            (resolve_const_int(func, args[0])?, args[1])
        }
        _ => return None,
    };

    // Header cond_br shape: `iv <= bound` → body, exit.
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
    let exit = false_dest;
    let cond_inst = header_block.insts.iter().find(|inst| inst.id == cond_id)?;
    let iv_bound = match cond_inst.kind {
        InstKind::ICmp(CmpOp::Le, lhs, rhs) if lhs == iv_param => resolve_const_int(func, rhs)?,
        InstKind::ICmp(CmpOp::Lt, lhs, rhs) if lhs == iv_param => {
            resolve_const_int(func, rhs)?.checked_sub(1)?
        }
        _ => return None,
    };

    let trip = iv_bound.checked_sub(iv_init).and_then(|d| d.checked_add(1))?;
    if trip <= 0 || (trip as u64) % (lanes as u64) != 0 {
        return None;
    }

    // Body shape: load + iadd(acc, load) + iadd(iv, 1) + branch back.
    let body_block = func.block(body);
    if body_block
        .insts
        .iter()
        .any(|inst| matches!(inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)))
    {
        return None;
    }
    let body_term_arg_iv;
    let body_term_arg_acc;
    match &body_block.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == header && args.len() == 2 => {
            body_term_arg_iv = args[0];
            body_term_arg_acc = args[1];
        }
        _ => return None,
    }

    let defs = inst_map(func);
    // The acc-update is `acc' = acc + load_or_value`. Match either
    // IAdd or FAdd depending on element type.
    let accumulate_inst = defs.get(&body_term_arg_acc)?;
    let (acc_lhs, acc_rhs) = match (&elem_ty, &accumulate_inst.kind) {
        (IrType::Int(_), InstKind::IAdd(l, r)) => (*l, *r),
        (IrType::Float(_), InstKind::FAdd(l, r)) => (*l, *r),
        _ => return None,
    };
    // One side must be the acc_param, the other a load from the
    // counted iteration space.
    let (accumulate_id, load_id) = if acc_lhs == acc_param {
        (accumulate_inst.id, acc_rhs)
    } else if acc_rhs == acc_param {
        (accumulate_inst.id, acc_lhs)
    } else {
        return None;
    };
    let load_inst = defs.get(&load_id)?;
    let InstKind::Load(load_ptr) = load_inst.kind else {
        return None;
    };
    let access = classify_array_access(func, load_ptr, iv_param)?;
    if access.elem_ty != elem_ty {
        return None;
    }
    // Require full-array coverage for the load too.
    let upper = access
        .lower
        .checked_add(access.len as i64)
        .and_then(|v| v.checked_sub(1))?;
    if iv_init != access.lower || iv_bound != upper {
        return None;
    }

    // The iv step.
    let step_inst = defs.get(&body_term_arg_iv)?;
    let (step_lhs, step_rhs) = match step_inst.kind {
        InstKind::IAdd(l, r) => (l, r),
        _ => return None,
    };
    let (step_const, _) = if step_lhs == iv_param {
        (step_rhs, resolve_const_int(func, step_rhs)?)
    } else if step_rhs == iv_param {
        (step_lhs, resolve_const_int(func, step_lhs)?)
    } else {
        return None;
    };
    let step_iadd = step_inst.id;

    // Validate that `acc_param` doesn't have any *other* uses inside
    // the loop besides the accumulate inst — otherwise the rewrite
    // gets ambiguous.
    let acc_extra_uses: usize = func
        .blocks
        .iter()
        .filter(|b| lp.body.contains(&b.id))
        .flat_map(|b| b.insts.iter())
        .filter(|inst| inst.id != accumulate_id)
        .filter(|inst| inst_uses(&inst.kind).contains(&acc_param))
        .count();
    if acc_extra_uses != 0 {
        return None;
    }

    // The accumulate_inst result must not be used inside the loop
    // (other than as the body terminator's arg). All in-loop uses
    // would conflict with our vector rewrite.
    let acc_result_extra_uses: usize = func
        .blocks
        .iter()
        .filter(|b| lp.body.contains(&b.id))
        .flat_map(|b| b.insts.iter())
        .filter(|inst| inst_uses(&inst.kind).contains(&accumulate_id))
        .count();
    if acc_result_extra_uses != 0 {
        return None;
    }

    Some(ReductionPlan {
        preheader,
        header,
        body,
        exit,
        iv_param,
        acc_param,
        acc_param_idx: 1,
        acc_init,
        load_id: load_inst.id,
        accumulate_id,
        step_iadd,
        step_const,
        iv_int_width,
        elem_ty,
        lanes,
        span: accumulate_inst.span,
    })
}

fn apply_reduction_plan(func: &mut Function, lp: &NaturalLoop, plan: ReductionPlan) {
    let v_ty = vector_ty(&plan.elem_ty, plan.lanes);

    // 1. Insert `vacc_init = vbroadcast(acc_init)` at the end of the
    //    preheader, before its branch terminator.
    let vacc_init = func.next_value_id();
    func.register_type(vacc_init, v_ty.clone());
    let pre_block = func.block_mut(plan.preheader);
    let pos = pre_block.insts.len();
    pre_block.insts.insert(
        pos,
        Inst {
            id: vacc_init,
            kind: InstKind::VBroadcast(plan.acc_init),
            ty: v_ty.clone(),
            span: plan.span,
        },
    );
    // 1b. Update the preheader branch arg slot for the accumulator.
    if let Some(Terminator::Branch(_, args)) = &mut pre_block.terminator {
        if let Some(slot) = args.get_mut(plan.acc_param_idx) {
            *slot = vacc_init;
        }
    }

    // 2. Update the header's accumulator block param type to the
    //    vector type.
    let header_block = func.block_mut(plan.header);
    if let Some(param) = header_block.params.get_mut(plan.acc_param_idx) {
        param.ty = v_ty.clone();
    }
    func.register_type(plan.acc_param, v_ty.clone());

    // 3. Insert a fresh ConstInt(V) for the iv step (avoid clobbering
    //    a shared `1`).
    let new_step_const = func.next_value_id();
    let step_const_ty = IrType::Int(plan.iv_int_width);
    func.register_type(new_step_const, step_const_ty.clone());
    let body_block = func.block_mut(plan.body);
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

    // 4. Rewrite the scalar Load → VLoad (vector type).
    let body_block = func.block_mut(plan.body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == plan.load_id) {
        if let InstKind::Load(ptr) = inst.kind {
            inst.kind = InstKind::VLoad(ptr);
            inst.ty = v_ty.clone();
        }
    }
    func.register_type(plan.load_id, v_ty.clone());

    // 5. Rewrite the accumulate IAdd/FAdd → VAdd (both operands now
    //    vectors: the acc_param block param is vector, the load is
    //    vector).
    let body_block = func.block_mut(plan.body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == plan.accumulate_id) {
        let new_kind = match inst.kind.clone() {
            InstKind::IAdd(l, r) | InstKind::FAdd(l, r) => InstKind::VAdd(l, r),
            other => other,
        };
        inst.kind = new_kind;
        inst.ty = v_ty.clone();
    }
    func.register_type(plan.accumulate_id, v_ty.clone());

    // 6. Insert `acc_scalar = vreduce_sum(acc_param)` at the top of
    //    the exit block, then walk every block NOT in the loop and
    //    rewrite acc_param → acc_scalar.
    let acc_scalar = func.next_value_id();
    func.register_type(acc_scalar, plan.elem_ty.clone());
    let exit_block = func.block_mut(plan.exit);
    exit_block.insts.insert(
        0,
        Inst {
            id: acc_scalar,
            kind: InstKind::VReduceSum(plan.acc_param),
            ty: plan.elem_ty.clone(),
            span: plan.span,
        },
    );
    let lp_body: HashSet<BlockId> = lp.body.iter().copied().collect();
    for block in func.blocks.iter_mut() {
        if lp_body.contains(&block.id) {
            continue;
        }
        for inst in &mut block.insts {
            // Skip the vreduce we just inserted — its sole purpose
            // is to consume the (now-vector) acc_param.
            if inst.id == acc_scalar {
                continue;
            }
            substitute_in_inst(&mut inst.kind, plan.acc_param, acc_scalar);
        }
        if let Some(term) = &mut block.terminator {
            substitute_in_terminator(term, plan.acc_param, acc_scalar);
        }
    }
}

fn substitute_in_inst(kind: &mut InstKind, from: ValueId, to: ValueId) {
    let mut replace = |v: &mut ValueId| {
        if *v == from {
            *v = to;
        }
    };
    match kind {
        InstKind::Load(p) => replace(p),
        InstKind::Store(v, p) => {
            replace(v);
            replace(p);
        }
        InstKind::IAdd(a, b)
        | InstKind::ISub(a, b)
        | InstKind::IMul(a, b)
        | InstKind::IDiv(a, b)
        | InstKind::FAdd(a, b)
        | InstKind::FSub(a, b)
        | InstKind::FMul(a, b)
        | InstKind::FDiv(a, b) => {
            replace(a);
            replace(b);
        }
        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
            for a in args {
                replace(a);
            }
        }
        InstKind::ICmp(_, a, b) | InstKind::FCmp(_, a, b) => {
            replace(a);
            replace(b);
        }
        InstKind::IntExtend(v, _, _) | InstKind::IntTrunc(v, _) => replace(v),
        InstKind::IntToFloat(v, _) | InstKind::FloatToInt(v, _) => replace(v),
        InstKind::FloatExtend(v, _) | InstKind::FloatTrunc(v, _) => replace(v),
        InstKind::INeg(v) | InstKind::FNeg(v) | InstKind::FAbs(v) => replace(v),
        _ => {
            // Conservative fallback: walk inst_uses and replace where
            // possible. The exact set varies; for the limited cases
            // we hit (post-loop scalar use of `acc_param`), the
            // explicit arms above are usually enough.
        }
    }
}

fn substitute_in_terminator(term: &mut Terminator, from: ValueId, to: ValueId) {
    let mut replace = |v: &mut ValueId| {
        if *v == from {
            *v = to;
        }
    };
    match term {
        Terminator::Return(Some(v)) => replace(v),
        Terminator::Return(None) => {}
        Terminator::Branch(_, args) => {
            for a in args {
                replace(a);
            }
        }
        Terminator::CondBranch {
            cond,
            true_args,
            false_args,
            ..
        } => {
            replace(cond);
            for a in true_args {
                replace(a);
            }
            for a in false_args {
                replace(a);
            }
        }
        _ => {}
    }
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
