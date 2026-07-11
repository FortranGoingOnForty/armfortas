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
use crate::ir::types::IrType;

use super::loop_utils::{loop_defined_values, remap_inst_kind};
use super::pass::Pass;
use super::util::{find_natural_loops, predecessors, NaturalLoop};
use super::vec_analysis::{
    build_vector_plan, build_where_plan, detect_counted_loop, detect_reduction_plan,
    detect_where_loop, loop_values_escape, AccumulateSource, BinaryKind, BinopOperand, BodyOp,
    CountedLoop, ReductionKind, ReductionPlan, Statement, UnaryKind, VectorPlan, WhereLoop,
    WherePlan,
};

pub struct NeonVectorize {
    contract_fma: bool,
}

impl NeonVectorize {
    pub const fn new(contract_fma: bool) -> Self {
        Self { contract_fma }
    }
}

impl Pass for NeonVectorize {
    fn name(&self) -> &'static str {
        "neon_vectorize"
    }

    fn run(&self, module: &mut Module) -> bool {
        run_loop_vectorizer(module, &super::vec_isa::NEON, self.contract_fma)
    }
}

/// SSE2 driver (x10): identical analysis and rewrites, the
/// SSE2_BASELINE legality table. Loops the table refuses stay scalar.
pub struct SseVectorize;

impl Pass for SseVectorize {
    fn name(&self) -> &'static str {
        "sse2_vectorize"
    }

    fn run(&self, module: &mut Module) -> bool {
        run_loop_vectorizer(module, &super::vec_isa::SSE2_BASELINE, false)
    }
}

fn run_loop_vectorizer(
    module: &mut Module,
    isa: &super::vec_isa::VectorIsa,
    contract_fma: bool,
) -> bool {
    let mut changed = false;
    for func in &mut module.functions {
        while vectorize_one_loop(func, isa, contract_fma) {
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

fn vectorize_one_loop(
    func: &mut Function,
    isa: &super::vec_isa::VectorIsa,
    contract_fma: bool,
) -> bool {
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
                if let Some(plan) = build_vector_plan(func, &shape, &loop_defs, isa) {
                    apply_vector_plan(func, &shape, plan, contract_fma);
                    return true;
                }
            }
        }
        // WHERE-block diamond (4-block: header / body / then / incr).
        if let Some(shape) = detect_where_loop(func, lp, &preds) {
            if let Some(plan) = build_where_plan(func, &shape, isa) {
                apply_where_plan(func, &shape, plan);
                return true;
            }
        }
        // Fall back: reduction loop (one escaping accumulator).
        if let Some(plan) = detect_reduction_plan(func, lp, &preds, isa) {
            apply_reduction_plan(func, lp, plan, contract_fma);
            return true;
        }
    }
    false
}

fn vector_ty(elem: &IrType, lanes: u8) -> IrType {
    IrType::Vector {
        lanes,
        elem: Box::new(elem.clone()),
    }
}

/// Widen one element-wise statement — its array loads, arithmetic, and
/// the destination store — to vector form, in place. Shared by the pure
/// element-wise vectorizer (`apply_vector_plan`) and the reduction
/// vectorizer, which fuses element-wise stores carried in the same body
/// (`c(i)=a(i)*b(i)` alongside `dot=dot+a(i)*b(i)`) into the widened
/// loop. Every match arm fires only on the scalar op kind, so widening a
/// load or product the reduction already turned into a VLoad/VMul is a
/// no-op — the shared value is reused rather than re-widened.
fn widen_statement(
    func: &mut Function,
    body: BlockId,
    preheader: BlockId,
    v_ty: &IrType,
    span: crate::lexer::Span,
    stmt: &Statement,
    contract_fma: bool,
) {
    for op in op_operands(&stmt.op) {
        rewrite_array_load(func, body, op, v_ty);
    }
    let (lhs_subst, rhs_subst) = match &stmt.op {
        BodyOp::Copy { .. } | BodyOp::Unary { .. } => (None, None),
        BodyOp::Binop { lhs, rhs, .. } => (
            broadcast_if_invariant(func, preheader, lhs, v_ty, span),
            broadcast_if_invariant(func, preheader, rhs, v_ty, span),
        ),
        BodyOp::Fma { .. } => (None, None),
    };
    let fma_subst = if let BodyOp::Fma { a, b, c, .. } = &stmt.op {
        Some((
            broadcast_if_invariant(func, preheader, a, v_ty, span),
            broadcast_if_invariant(func, preheader, b, v_ty, span),
            broadcast_if_invariant(func, preheader, c, v_ty, span),
        ))
    } else {
        None
    };

    if let BodyOp::Unary {
        unary_id,
        kind: unary_kind,
        source,
    } = &stmt.op
    {
        // Integer abs folds from a `select(icmp, x, ineg(x))`, so its
        // unary_id is the Select, not an FAbs/INeg — take the abs
        // source from the classified operand (the load, already
        // rewritten to a VLoad above). The dead cmp/ineg/const left
        // behind are DCE'd, same as the min/max select's dead cmp.
        let src_load = match source {
            BinopOperand::ArrayLoad(id) => Some(*id),
            BinopOperand::InvariantScalar(_) => None,
        };
        let body_block = func.block_mut(body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == *unary_id) {
            let new_kind = match (inst.kind.clone(), unary_kind) {
                (InstKind::INeg(s), UnaryKind::Neg) | (InstKind::FNeg(s), UnaryKind::Neg) => {
                    InstKind::VNeg(s)
                }
                (InstKind::FAbs(s), UnaryKind::Abs) => InstKind::VAbs(s),
                (InstKind::FSqrt(s), UnaryKind::Sqrt) => InstKind::VSqrt(s),
                (InstKind::Select(..), UnaryKind::Abs) => {
                    InstKind::VAbs(src_load.expect("integer abs source must be an array load"))
                }
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
        let body_block = func.block_mut(body);
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
                (InstKind::Select(_, t, f), BinaryKind::Max) => {
                    InstKind::VMax(lhs_subst.unwrap_or(t), rhs_subst.unwrap_or(f))
                }
                (InstKind::Select(_, t, f), BinaryKind::Min) => {
                    InstKind::VMin(lhs_subst.unwrap_or(t), rhs_subst.unwrap_or(f))
                }
                _ => inst.kind.clone(),
            };
            inst.kind = new_kind;
            inst.ty = v_ty.clone();
        }
        func.register_type(*binop_id, v_ty.clone());
    }

    if let BodyOp::Fma {
        fmul_id, fadd_id, ..
    } = &stmt.op
    {
        let (a_subst, b_subst, c_subst) = fma_subst.unwrap();
        let body_block = func.block_mut(body);
        // Rewrite fmul to VMul. It stays live for strict vectorization
        // and becomes dead after Ofast contraction.
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == *fmul_id) {
            if let InstKind::FMul(l, r) = inst.kind {
                inst.kind = InstKind::VMul(a_subst.unwrap_or(l), b_subst.unwrap_or(r));
                inst.ty = v_ty.clone();
            }
        }
        func.register_type(*fmul_id, v_ty.clone());
        let fused_operands = contract_fma.then(|| {
            let fmul_inst = func
                .block(body)
                .insts
                .iter()
                .find(|inst| inst.id == *fmul_id)
                .expect("vectorized fmul should remain in its body");
            match fmul_inst.kind {
                InstKind::VMul(l, r) => (l, r),
                _ => unreachable!(),
            }
        });
        let body_block = func.block_mut(body);
        if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == *fadd_id) {
            if let InstKind::FAdd(l, r) = inst.kind {
                inst.kind = if let Some((a_v, b_v)) = fused_operands {
                    let c = if l == *fmul_id { r } else { l };
                    InstKind::VFma(a_v, b_v, c_subst.unwrap_or(c))
                } else {
                    let l = if l == *fmul_id {
                        l
                    } else {
                        c_subst.unwrap_or(l)
                    };
                    let r = if r == *fmul_id {
                        r
                    } else {
                        c_subst.unwrap_or(r)
                    };
                    InstKind::VAdd(l, r)
                };
                inst.ty = v_ty.clone();
            }
        }
        func.register_type(*fadd_id, v_ty.clone());
    }

    let body_block = func.block_mut(body);
    if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == stmt.store) {
        if let InstKind::Store(val, ptr) = inst.kind {
            inst.kind = InstKind::VStore(val, ptr);
        }
    }
}

fn apply_vector_plan(
    func: &mut Function,
    shape: &CountedLoop,
    plan: VectorPlan,
    contract_fma: bool,
) {
    let v_ty = vector_ty(&plan.elem_ty, plan.lanes);

    // 0. If we'll be peeling scalar tail iterations, snapshot the body's
    //    instruction list BEFORE we mutate it in place. The snapshot
    //    holds the original (scalar) Load/Store/Binop shape that the
    //    peel walks per remainder iteration. Take a clone of the Vec
    //    so subsequent in-place mutation doesn't disturb the snapshot.
    let body_snapshot: Option<Vec<Inst>> = if plan.tail_count > 0 {
        Some(func.block(shape.body).insts.clone())
    } else {
        None
    };
    // Identify the exit block (the false-dest of the header's
    // cond_br) so we know where to peel into.
    let exit_block_id: Option<BlockId> = if plan.tail_count > 0 {
        match &func.block(shape.header).terminator {
            Some(Terminator::CondBranch { false_dest, .. }) => Some(*false_dest),
            _ => None,
        }
    } else {
        None
    };

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
        widen_statement(
            func,
            shape.body,
            shape.preheader,
            &v_ty,
            plan.span,
            &stmt,
            contract_fma,
        );
    }

    // 3. Scalar tail. If `tail_count` remainder iterations live at the
    //    end of the loop, retarget the original icmp's bound to
    //    `iv_init + head_count - 1` and peel the remaining scalar
    //    iterations into the head of the exit block.
    if plan.tail_count > 0 {
        if let (Some(snapshot), Some(exit_block)) = (body_snapshot, exit_block_id) {
            apply_scalar_tail_peel(func, shape, &plan, &snapshot, exit_block);
        }
    }
}

/// Rewrite a WHERE diamond into a vectorized straight-line body:
///   body: vload a; vload b_old; v(f|i)cmp pred; vselect mask, va, vb_old;
///         vstore result, b_ptr; br incr_block
/// The original `then` block becomes unreachable.
fn apply_where_plan(func: &mut Function, shape: &WhereLoop, plan: WherePlan) {
    let v_ty = IrType::Vector {
        elem: Box::new(plan.elem_ty.clone()),
        lanes: plan.lanes,
    };
    let span = plan.span;

    // 1. Broadcast the threshold into the preheader (it's loop-invariant,
    //    typically a const). Use VBroadcast so vfcmp/vicmp gets a
    //    full vector lane.
    let bcast_id = {
        let preheader = func.block_mut(shape.preheader);
        let id = preheader
            .params
            .first()
            .map(|_| ())
            .map_or_else(|| 0, |_| 0);
        let _ = id;
        let new_id = func.next_value_id();
        func.register_type(new_id, v_ty.clone());
        let preheader = func.block_mut(shape.preheader);
        // Insert just before the terminator branch.
        let pos = preheader.insts.len();
        preheader.insts.insert(
            pos,
            Inst {
                id: new_id,
                kind: InstKind::VBroadcast(plan.threshold_v),
                ty: v_ty.clone(),
                span,
            },
        );
        new_id
    };

    // 2. Rewrite load_a (in body) to VLoad. Type changes from elem to vector.
    let load_a_ptr = {
        let body = func.block_mut(shape.body);
        let inst = body
            .insts
            .iter_mut()
            .find(|i| i.id == plan.load_a_id)
            .unwrap();
        let p = match inst.kind {
            InstKind::Load(p) => p,
            _ => unreachable!(),
        };
        inst.kind = InstKind::VLoad(p);
        inst.ty = v_ty.clone();
        p
    };
    func.register_type(plan.load_a_id, v_ty.clone());
    let _ = load_a_ptr;

    // 3. In body block, after load_a, emit:
    //    vload_b_old (only when no ELSEWHERE — we need the dest's
    //    prior value for the masked-off lanes), v(f|i)cmp, optional
    //    v-unary, vselect, vstore. The cmp+cond_br are dropped (we
    //    replace the terminator below).
    let vcmp_id = func.next_value_id();
    func.register_type(vcmp_id, v_ty.clone());
    let vsel_id = func.next_value_id();
    func.register_type(vsel_id, v_ty.clone());
    let vstore_id = func.next_value_id();
    func.register_type(vstore_id, IrType::Void);

    let mut new_insts: Vec<Inst> = Vec::new();
    // The false-mask arm: prefer ELSEWHERE-supplied values when
    // present, else reload the dest's prior value so masked-off lanes
    // are preserved.
    //   else_const     → VBroadcast(K) in preheader.
    //   else_load_ptr  → VLoad on a body-defined GEP (e.g. `c = d`).
    //   neither        → VLoad on dest_ptr_id (preserve old lanes).
    let false_arm_id = if let Some(else_v) = plan.else_const {
        let vk_id = func.next_value_id();
        func.register_type(vk_id, v_ty.clone());
        let preheader = func.block_mut(shape.preheader);
        let pos = preheader.insts.len();
        preheader.insts.insert(
            pos,
            Inst {
                id: vk_id,
                kind: InstKind::VBroadcast(else_v),
                ty: v_ty.clone(),
                span,
            },
        );
        vk_id
    } else if let Some(load_ptr) = plan.else_load_ptr {
        let vload_else_id = func.next_value_id();
        func.register_type(vload_else_id, v_ty.clone());
        new_insts.push(Inst {
            id: vload_else_id,
            kind: InstKind::VLoad(load_ptr),
            ty: v_ty.clone(),
            span,
        });
        // Apply unary or binop on the else load when present.
        if let Some(kind) = plan.else_unary {
            let vu_id = func.next_value_id();
            func.register_type(vu_id, v_ty.clone());
            let vu_kind = match kind {
                UnaryKind::Neg => InstKind::VNeg(vload_else_id),
                UnaryKind::Abs => InstKind::VAbs(vload_else_id),
                UnaryKind::Sqrt => InstKind::VSqrt(vload_else_id),
            };
            new_insts.push(Inst {
                id: vu_id,
                kind: vu_kind,
                ty: v_ty.clone(),
                span,
            });
            vu_id
        } else if let Some((kind, scalar_v, load_on_lhs)) = plan.else_binop {
            let vk_id = func.next_value_id();
            func.register_type(vk_id, v_ty.clone());
            let preheader = func.block_mut(shape.preheader);
            let pos = preheader.insts.len();
            preheader.insts.insert(
                pos,
                Inst {
                    id: vk_id,
                    kind: InstKind::VBroadcast(scalar_v),
                    ty: v_ty.clone(),
                    span,
                },
            );
            let (l_id, r_id) = if load_on_lhs {
                (vload_else_id, vk_id)
            } else {
                (vk_id, vload_else_id)
            };
            let vbin_id = func.next_value_id();
            func.register_type(vbin_id, v_ty.clone());
            let vbin_kind = match kind {
                BinaryKind::Add => InstKind::VAdd(l_id, r_id),
                BinaryKind::Sub => InstKind::VSub(l_id, r_id),
                BinaryKind::Mul => InstKind::VMul(l_id, r_id),
                BinaryKind::Div => InstKind::VDiv(l_id, r_id),
                BinaryKind::Min | BinaryKind::Max => InstKind::VAdd(l_id, r_id),
            };
            new_insts.push(Inst {
                id: vbin_id,
                kind: vbin_kind,
                ty: v_ty.clone(),
                span,
            });
            vbin_id
        } else {
            vload_else_id
        }
    } else {
        let vload_b_id = func.next_value_id();
        func.register_type(vload_b_id, v_ty.clone());
        new_insts.push(Inst {
            id: vload_b_id,
            kind: InstKind::VLoad(plan.dest_ptr_id),
            ty: v_ty.clone(),
            span,
        });
        vload_b_id
    };
    new_insts.push(Inst {
        id: vcmp_id,
        kind: if plan.cmp_is_float {
            if plan.load_on_lhs {
                InstKind::VFCmp(plan.cmp_op, plan.load_a_id, bcast_id)
            } else {
                InstKind::VFCmp(plan.cmp_op, bcast_id, plan.load_a_id)
            }
        } else if plan.load_on_lhs {
            InstKind::VICmp(plan.cmp_op, plan.load_a_id, bcast_id)
        } else {
            InstKind::VICmp(plan.cmp_op, bcast_id, plan.load_a_id)
        },
        ty: v_ty.clone(),
        span,
    });
    // If the WHERE body computes `b = unary(a)` or `b = a op K`,
    // emit the vector op on the vload_a value (broadcasting the
    // scalar K for the binop case) and use that as the vselect's
    // "true" arm.
    let true_arm_id = if let Some((_then_uid, kind, on_load_b)) = plan.then_unary {
        // The source vector for the unary: load_a (default) or a
        // VLoad on the b-array's ptr (`c = -d`).
        let src_vec_id = if on_load_b {
            let b_ptr = plan.b_ptr_id.expect("unary_on_load_b must have b_ptr_id");
            let vload_b_id = func.next_value_id();
            func.register_type(vload_b_id, v_ty.clone());
            new_insts.push(Inst {
                id: vload_b_id,
                kind: InstKind::VLoad(b_ptr),
                ty: v_ty.clone(),
                span,
            });
            vload_b_id
        } else {
            plan.load_a_id
        };
        let vu_id = func.next_value_id();
        func.register_type(vu_id, v_ty.clone());
        let vu_kind = match kind {
            UnaryKind::Neg => InstKind::VNeg(src_vec_id),
            UnaryKind::Abs => InstKind::VAbs(src_vec_id),
            UnaryKind::Sqrt => InstKind::VSqrt(src_vec_id),
        };
        new_insts.push(Inst {
            id: vu_id,
            kind: vu_kind,
            ty: v_ty.clone(),
            span,
        });
        vu_id
    } else if let Some(b) = plan.then_binop {
        // The "main load" is load_a unless `binop_on_load_b == true`,
        // in which case it's a fresh VLoad on b_ptr.
        let main_load_id = if b.binop_on_load_b {
            let b_ptr = plan.b_ptr_id.expect("binop_on_load_b must have b_ptr_id");
            let vload_b_id = func.next_value_id();
            func.register_type(vload_b_id, v_ty.clone());
            new_insts.push(Inst {
                id: vload_b_id,
                kind: InstKind::VLoad(b_ptr),
                ty: v_ty.clone(),
                span,
            });
            vload_b_id
        } else {
            plan.load_a_id
        };
        // Other operand: either a vload on the b-array's ptr (array+
        // array body) or a vbroadcast of a loop-invariant scalar.
        let other_v = if b.other_is_load_b {
            let b_ptr = plan.b_ptr_id.expect("load_b binop must have b_ptr_id");
            let vload_b_id = func.next_value_id();
            func.register_type(vload_b_id, v_ty.clone());
            new_insts.push(Inst {
                id: vload_b_id,
                kind: InstKind::VLoad(b_ptr),
                ty: v_ty.clone(),
                span,
            });
            vload_b_id
        } else {
            let vk_id = func.next_value_id();
            func.register_type(vk_id, v_ty.clone());
            let preheader = func.block_mut(shape.preheader);
            let pos = preheader.insts.len();
            preheader.insts.insert(
                pos,
                Inst {
                    id: vk_id,
                    kind: InstKind::VBroadcast(b.scalar_v),
                    ty: v_ty.clone(),
                    span,
                },
            );
            vk_id
        };
        // Compute the binop in body block, in original operand order.
        let (l_id, r_id) = if b.load_on_lhs {
            (main_load_id, other_v)
        } else {
            (other_v, main_load_id)
        };
        let vbin_id = func.next_value_id();
        func.register_type(vbin_id, v_ty.clone());
        let vbin_kind = match b.kind {
            BinaryKind::Add => InstKind::VAdd(l_id, r_id),
            BinaryKind::Sub => InstKind::VSub(l_id, r_id),
            BinaryKind::Mul => InstKind::VMul(l_id, r_id),
            BinaryKind::Div => InstKind::VDiv(l_id, r_id),
            // Min/Max not produced by then-binop walker (those are
            // recognized via Select, not directly).
            BinaryKind::Min | BinaryKind::Max => InstKind::VAdd(l_id, r_id),
        };
        new_insts.push(Inst {
            id: vbin_id,
            kind: vbin_kind,
            ty: v_ty.clone(),
            span,
        });
        vbin_id
    } else if let Some(k_scalar) = plan.then_const {
        // Broadcast the loop-invariant scalar in the preheader so the
        // vselect sees a full lane-vector of K's in its true arm.
        let vk_id = func.next_value_id();
        func.register_type(vk_id, v_ty.clone());
        let preheader = func.block_mut(shape.preheader);
        let pos = preheader.insts.len();
        preheader.insts.insert(
            pos,
            Inst {
                id: vk_id,
                kind: InstKind::VBroadcast(k_scalar),
                ty: v_ty.clone(),
                span,
            },
        );
        vk_id
    } else {
        plan.load_a_id
    };
    new_insts.push(Inst {
        id: vsel_id,
        kind: InstKind::VSelect(vcmp_id, true_arm_id, false_arm_id),
        ty: v_ty.clone(),
        span,
    });
    new_insts.push(Inst {
        id: vstore_id,
        kind: InstKind::VStore(vsel_id, plan.dest_ptr_id),
        ty: IrType::Void,
        span,
    });

    // Drop the original cmp inst from the body (it's the cond_id) —
    // it'll be dead. Drop everything *after* load_a that we don't
    // need (the original cmp). For simplicity, walk the body, keep
    // load_a + its dependency chain (gep ptrs), drop the cmp.
    {
        let body = func.block_mut(shape.body);
        body.insts.retain(|i| i.id != plan.cmp_id);
        // Append the new vector ops at the end of the body.
        body.insts.extend(new_insts);
        // Replace cond_br terminator with unconditional br to incr.
        body.terminator = Some(Terminator::Branch(shape.incr_block, vec![]));
    }

    // 4. Drop then-block: clear its insts and make it unreachable.
    //    prune_unreachable will remove the block after the pass.
    {
        let then = func.block_mut(shape.then_block);
        then.insts.clear();
        then.terminator = Some(Terminator::Branch(shape.incr_block, vec![]));
    }
    // Same for the else_block (when ELSEWHERE was present).
    if let Some(else_id) = shape.else_block {
        let else_blk = func.block_mut(else_id);
        else_blk.insts.clear();
        else_blk.terminator = Some(Terminator::Branch(shape.incr_block, vec![]));
    }

    // 5. Update the incr block's iadd to step by `lanes` instead of 1.
    let incr = func.block_mut(shape.incr_block);
    let step_id = match &incr.terminator {
        Some(Terminator::Branch(_, args)) if args.len() == 1 => args[0],
        _ => return,
    };
    let iadd_inst = incr.insts.iter().find(|i| i.id == step_id).cloned();
    let (iv_param, old_step_const, iv_int_width) = match iadd_inst {
        Some(inst) => match inst.kind {
            InstKind::IAdd(l, r) => {
                let (iv, k) = if l == shape.iv_param {
                    (l, r)
                } else if r == shape.iv_param {
                    (r, l)
                } else {
                    return;
                };
                let width = match inst.ty {
                    IrType::Int(w) => w,
                    _ => return,
                };
                (iv, k, width)
            }
            _ => return,
        },
        _ => return,
    };
    let _ = iv_param;
    // Allocate a fresh ConstInt for the new step.
    let new_step = func.next_value_id();
    func.register_type(new_step, IrType::Int(iv_int_width));
    let incr = func.block_mut(shape.incr_block);
    incr.insts.insert(
        0,
        Inst {
            id: new_step,
            kind: InstKind::ConstInt(plan.lanes as i128, iv_int_width),
            ty: IrType::Int(iv_int_width),
            span,
        },
    );
    if let Some(inst) = incr.insts.iter_mut().find(|i| i.id == step_id) {
        if let InstKind::IAdd(l, r) = inst.kind {
            if l == old_step_const {
                inst.kind = InstKind::IAdd(new_step, r);
            } else if r == old_step_const {
                inst.kind = InstKind::IAdd(l, new_step);
            }
        }
    }
}

/// Insert a fresh ConstInt for the head bound
/// (`iv_init + head_count - 1`) into the preheader and rewire the
/// original icmp's RHS to reference it; then peel `tail_count` scalar
/// copies of the body into the top of the exit block, with the IV
/// substituted by a constant per iteration.
fn apply_scalar_tail_peel(
    func: &mut Function,
    shape: &CountedLoop,
    plan: &VectorPlan,
    body_snapshot: &[Inst],
    exit_block: BlockId,
) {
    let int_ty = IrType::Int(plan.iv_int_width);

    // Insert the new bound const (iv_init + head_count - 1) at the
    // top of the preheader. It dominates the header's icmp.
    let new_bound = shape.iv_init + plan.head_count - 1;
    let new_bound_id = func.next_value_id();
    func.register_type(new_bound_id, int_ty.clone());
    let pre_block = func.block_mut(shape.preheader);
    pre_block.insts.insert(
        0,
        Inst {
            id: new_bound_id,
            kind: InstKind::ConstInt(new_bound as i128, plan.iv_int_width),
            ty: int_ty.clone(),
            span: plan.span,
        },
    );

    // Rewrite the icmp's RHS to point at the new bound const.
    let header_block = func.block_mut(shape.header);
    if let Some(inst) = header_block
        .insts
        .iter_mut()
        .find(|i| i.id == shape.cond_id)
    {
        if let InstKind::ICmp(_, _, rhs) = &mut inst.kind {
            if *rhs == shape.bound_const_id {
                *rhs = new_bound_id;
            }
        }
    }

    // Skip the step iadd in the snapshot: the peel doesn't need to
    // bump the IV.
    let step_inst_id = plan.step_iadd;

    // Build a vector of `(new_inst_id, new_kind, ty, span)` per peel
    // iteration, then prepend them to the exit block's insts.
    let mut peeled: Vec<Inst> = Vec::new();
    for t in 0..plan.tail_count {
        let tail_iv = shape.iv_init + plan.head_count + t;
        let tail_iv_const_id = func.next_value_id();
        func.register_type(tail_iv_const_id, int_ty.clone());
        peeled.push(Inst {
            id: tail_iv_const_id,
            kind: InstKind::ConstInt(tail_iv as i128, plan.iv_int_width),
            ty: int_ty.clone(),
            span: plan.span,
        });

        let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
        val_map.insert(shape.iv_param, tail_iv_const_id);

        for inst in body_snapshot {
            // Skip the step iadd — peel iterations don't bump the IV.
            if inst.id == step_inst_id {
                continue;
            }
            let new_id = func.next_value_id();
            func.register_type(new_id, inst.ty.clone());
            let new_kind = remap_inst_kind(&inst.kind, &val_map);
            val_map.insert(inst.id, new_id);
            peeled.push(Inst {
                id: new_id,
                kind: new_kind,
                ty: inst.ty.clone(),
                span: inst.span,
            });
        }
    }

    // Prepend peeled insts at the top of the exit block.
    let exit = func.block_mut(exit_block);
    let existing = std::mem::take(&mut exit.insts);
    let mut new_insts = peeled;
    new_insts.extend(existing);
    exit.insts = new_insts;
}

/// Iterate the operands of a body op (one for `Copy`/`Unary`, two
/// for `Binop`).
fn op_operands(op: &BodyOp) -> Vec<&BinopOperand> {
    match op {
        BodyOp::Copy { source } | BodyOp::Unary { source, .. } => vec![source],
        BodyOp::Binop { lhs, rhs, .. } => vec![lhs, rhs],
        BodyOp::Fma { a, b, c, .. } => vec![a, b, c],
    }
}

/// If `op` is an `ArrayLoad`, rewrite its scalar Load to a VLoad and
/// register the load's type as the vector type.
fn rewrite_array_load(func: &mut Function, body: BlockId, op: &BinopOperand, v_ty: &IrType) {
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

fn apply_reduction_plan(
    func: &mut Function,
    lp: &NaturalLoop,
    plan: ReductionPlan,
    contract_fma: bool,
) {
    let v_ty = vector_ty(&plan.elem_ty, plan.lanes);

    // 0. Snapshot the body before any in-place mutation. Used by the
    //    scalar-tail peel below (sum reductions only).
    let body_snapshot: Option<Vec<Inst>> = if plan.tail_count > 0 {
        Some(func.block(plan.body).insts.clone())
    } else {
        None
    };

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

    // 4. Rewrite the per-iteration source value:
    //    - Sum: one Load → VLoad.
    //    - SumWithUnary: Load → VLoad and unary → VNeg / VAbs.
    //    - Dot: two Loads → VLoad each, plus IMul/FMul → VMul.
    match plan.source.clone() {
        AccumulateSource::Sum { load_id } => {
            let body_block = func.block_mut(plan.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
                if let InstKind::Load(ptr) = inst.kind {
                    inst.kind = InstKind::VLoad(ptr);
                    inst.ty = v_ty.clone();
                }
            }
            func.register_type(load_id, v_ty.clone());
        }
        AccumulateSource::SumWithUnary {
            load_id,
            unary_id,
            kind,
        } => {
            let body_block = func.block_mut(plan.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
                if let InstKind::Load(ptr) = inst.kind {
                    inst.kind = InstKind::VLoad(ptr);
                    inst.ty = v_ty.clone();
                }
            }
            func.register_type(load_id, v_ty.clone());
            let body_block = func.block_mut(plan.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == unary_id) {
                let new_kind = match (inst.kind.clone(), kind) {
                    (InstKind::INeg(s), UnaryKind::Neg) | (InstKind::FNeg(s), UnaryKind::Neg) => {
                        InstKind::VNeg(s)
                    }
                    (InstKind::FAbs(s), UnaryKind::Abs) => InstKind::VAbs(s),
                    (other, _) => other,
                };
                inst.kind = new_kind;
                inst.ty = v_ty.clone();
            }
            func.register_type(unary_id, v_ty.clone());
        }
        AccumulateSource::Dot {
            imul_id,
            load_a,
            load_b,
        } => {
            for load_id in [load_a, load_b] {
                let body_block = func.block_mut(plan.body);
                if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
                    if let InstKind::Load(ptr) = inst.kind {
                        inst.kind = InstKind::VLoad(ptr);
                        inst.ty = v_ty.clone();
                    }
                }
                func.register_type(load_id, v_ty.clone());
            }
            let body_block = func.block_mut(plan.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == imul_id) {
                let new_kind = match inst.kind.clone() {
                    InstKind::IMul(l, r) | InstKind::FMul(l, r) => InstKind::VMul(l, r),
                    other => other,
                };
                inst.kind = new_kind;
                inst.ty = v_ty.clone();
            }
            func.register_type(imul_id, v_ty.clone());
        }
        AccumulateSource::SumOfDiff {
            sub_id,
            load_a,
            load_b,
        } => {
            for load_id in [load_a, load_b] {
                let body_block = func.block_mut(plan.body);
                if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == load_id) {
                    if let InstKind::Load(ptr) = inst.kind {
                        inst.kind = InstKind::VLoad(ptr);
                        inst.ty = v_ty.clone();
                    }
                }
                func.register_type(load_id, v_ty.clone());
            }
            let body_block = func.block_mut(plan.body);
            if let Some(inst) = body_block.insts.iter_mut().find(|i| i.id == sub_id) {
                let new_kind = match inst.kind.clone() {
                    InstKind::ISub(l, r) | InstKind::FSub(l, r) => InstKind::VSub(l, r),
                    other => other,
                };
                inst.kind = new_kind;
                inst.ty = v_ty.clone();
            }
            func.register_type(sub_id, v_ty.clone());
        }
    }

    // 5. Rewrite the accumulate inst:
    //    - Sum: IAdd/FAdd → VAdd
    //    - Max: select(icmp, acc, value) → VMax(acc, value)
    //    - Min: select(icmp, acc, value) → VMin(acc, value)
    //    For Min/Max we also drop the icmp predicate's type since
    //    its result is no longer used (regalloc will dead-code it).
    let body_block = func.block_mut(plan.body);
    if let Some(inst) = body_block
        .insts
        .iter_mut()
        .find(|i| i.id == plan.accumulate_id)
    {
        let acc_v = plan.acc_param;
        let new_kind = match (inst.kind.clone(), plan.reduce) {
            (InstKind::IAdd(l, r), ReductionKind::Sum)
            | (InstKind::FAdd(l, r), ReductionKind::Sum) => InstKind::VAdd(l, r),
            (InstKind::Select(_, t, f), ReductionKind::Max) => {
                // The detection guarantees one arm is acc, the
                // other is the value (now a vector). Use whichever
                // arm equals acc.
                if t == acc_v {
                    InstKind::VMax(t, f)
                } else {
                    InstKind::VMax(f, t)
                }
            }
            (InstKind::Select(_, t, f), ReductionKind::Min) => {
                if t == acc_v {
                    InstKind::VMin(t, f)
                } else {
                    InstKind::VMin(f, t)
                }
            }
            (other, _) => other,
        };
        inst.kind = new_kind;
        inst.ty = v_ty.clone();
    }
    func.register_type(plan.accumulate_id, v_ty.clone());

    // 5b. Widen element-wise stores fused into the reduction body
    //     (`c(i)=a(i)*b(i)` beside `dot=dot+a(i)*b(i)`). Runs after the
    //     reduction chain so a load/product shared with it is already a
    //     VLoad/VMul and gets reused rather than re-widened. The
    //     scalar-tail peel below replays the pre-mutation body snapshot,
    //     so any peeled iterations still run these stores scalar-ly at
    //     the correct index.
    for stmt in plan.elementwise_stores.clone() {
        widen_statement(
            func,
            plan.body,
            plan.preheader,
            &v_ty,
            plan.span,
            &stmt,
            contract_fma,
        );
    }

    // 6. Insert `acc_scalar = vreduce_*(acc_param)` at the top of
    //    the exit block, then walk every block NOT in the loop and
    //    rewrite acc_param → acc_scalar.
    let acc_scalar = func.next_value_id();
    func.register_type(acc_scalar, plan.elem_ty.clone());
    let reduce_kind = match plan.reduce {
        ReductionKind::Sum => InstKind::VReduceSum(plan.acc_param),
        ReductionKind::Max => InstKind::VReduceMax(plan.acc_param),
        ReductionKind::Min => InstKind::VReduceMin(plan.acc_param),
    };
    let exit_block = func.block_mut(plan.exit);
    exit_block.insts.insert(
        0,
        Inst {
            id: acc_scalar,
            kind: reduce_kind,
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

    // 7. Reduction scalar tail (sum only). Peel `tail_count` scalar
    //    iterations into the exit block so they accumulate from
    //    `acc_scalar` into a chained `final_acc`. Then retarget
    //    post-tail consumers of `acc_scalar` to `final_acc`.
    if plan.tail_count > 0 {
        if let Some(snapshot) = body_snapshot {
            apply_reduction_scalar_tail(func, &plan, &snapshot, acc_scalar, &lp_body);
        }
    }
}

/// Peel `plan.tail_count` scalar iterations of the body into the
/// exit block (just after the `vreduce_*`), each iteration chaining
/// from the previous accumulator. The first iteration's seed is
/// `acc_scalar`; the last produces `final_acc`. After peeling, every
/// non-loop, non-peel use of `acc_scalar` is rewritten to
/// `final_acc`.
fn apply_reduction_scalar_tail(
    func: &mut Function,
    plan: &ReductionPlan,
    body_snapshot: &[Inst],
    acc_scalar: ValueId,
    lp_body: &HashSet<BlockId>,
) {
    let int_ty = IrType::Int(plan.iv_int_width);

    // Insert the new head-bound const at the top of the preheader.
    let new_bound = plan.iv_init + plan.head_count - 1;
    let new_bound_id = func.next_value_id();
    func.register_type(new_bound_id, int_ty.clone());
    func.block_mut(plan.preheader).insts.insert(
        0,
        Inst {
            id: new_bound_id,
            kind: InstKind::ConstInt(new_bound as i128, plan.iv_int_width),
            ty: int_ty.clone(),
            span: plan.span,
        },
    );

    // Rewrite the original icmp's RHS to point at the new bound.
    if let Some(inst) = func
        .block_mut(plan.header)
        .insts
        .iter_mut()
        .find(|i| i.id == plan.cond_id)
    {
        if let InstKind::ICmp(_, _, rhs) = &mut inst.kind {
            if *rhs == plan.bound_const_id {
                *rhs = new_bound_id;
            }
        }
    }

    let step_inst_id = plan.step_iadd;
    let mut peeled: Vec<Inst> = Vec::new();
    let mut peel_ids: HashSet<ValueId> = HashSet::new();
    let mut current_acc = acc_scalar;

    for t in 0..plan.tail_count {
        let tail_iv = plan.iv_init + plan.head_count + t;
        let tail_iv_const_id = func.next_value_id();
        func.register_type(tail_iv_const_id, int_ty.clone());
        peeled.push(Inst {
            id: tail_iv_const_id,
            kind: InstKind::ConstInt(tail_iv as i128, plan.iv_int_width),
            ty: int_ty.clone(),
            span: plan.span,
        });
        peel_ids.insert(tail_iv_const_id);

        let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
        val_map.insert(plan.iv_param, tail_iv_const_id);
        val_map.insert(plan.acc_param, current_acc);

        for inst in body_snapshot {
            // Skip the IV step iadd — peel iterations don't bump iv.
            if inst.id == step_inst_id {
                continue;
            }
            let new_id = func.next_value_id();
            func.register_type(new_id, inst.ty.clone());
            let new_kind = remap_inst_kind(&inst.kind, &val_map);
            val_map.insert(inst.id, new_id);
            peel_ids.insert(new_id);
            peeled.push(Inst {
                id: new_id,
                kind: new_kind,
                ty: inst.ty.clone(),
                span: inst.span,
            });
        }
        current_acc = val_map[&plan.accumulate_id];
    }
    let final_acc = current_acc;

    // Splice peeled insts into the exit block, just after `acc_scalar`
    // (which is at exit[0]).
    let exit = func.block_mut(plan.exit);
    let acc_pos = exit
        .insts
        .iter()
        .position(|i| i.id == acc_scalar)
        .unwrap_or(0);
    let after = acc_pos + 1;
    let tail = exit.insts.split_off(after);
    exit.insts.extend(peeled);
    exit.insts.extend(tail);

    // Retarget non-loop, non-peel uses of acc_scalar → final_acc.
    if final_acc != acc_scalar {
        for block in func.blocks.iter_mut() {
            if lp_body.contains(&block.id) {
                continue;
            }
            for inst in &mut block.insts {
                if peel_ids.contains(&inst.id) || inst.id == acc_scalar {
                    continue;
                }
                substitute_in_inst(&mut inst.kind, acc_scalar, final_acc);
            }
            if let Some(term) = &mut block.terminator {
                substitute_in_terminator(term, acc_scalar, final_acc);
            }
        }
    }
}

fn substitute_in_inst(kind: &mut InstKind, from: ValueId, to: ValueId) {
    let replace = |v: &mut ValueId| {
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
    let replace = |v: &mut ValueId| {
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
    use crate::ir::types::{IntWidth, IrType};
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
        let mut module = Module::new("m".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let b = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let c = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
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
        let changed = NeonVectorize::new(false).run(&mut module);
        assert!(
            changed,
            "neon_vectorize should fire on a clean array-add loop"
        );

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
                    IrType::Vector {
                        lanes: 4,
                        elem: Box::new(IrType::Int(IntWidth::I32))
                    }
                );
            }
        }

        // The IV step should now use a ConstInt(4) somewhere in the body.
        let has_v_step = body_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::ConstInt(4, IntWidth::I32)));
        assert!(has_v_step, "step should now be ConstInt(4)");
    }

    /// Build `c(i) = a(i) + scale` over i32(32) where `scale` is a
    /// loop-invariant ConstInt defined in the entry/preheader. The
    /// vectorizer should classify `scale` as `InvariantScalar`, hoist
    /// a `VBroadcast` into the preheader, and rewrite the binop to
    /// consume the broadcast vector.
    fn build_array_add_scalar_loop() -> (Module, BlockId, BlockId) {
        let mut module = Module::new("m".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let c = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );

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
        let changed = NeonVectorize::new(false).run(&mut module);
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
        let mut module = Module::new("m".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let b = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let c = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
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
        let changed = NeonVectorize::new(false).run(&mut module);
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
    fn peels_scalar_tail_for_non_divisible_trip_count() {
        // length 31 → not divisible by V=4. The pass vectorizes 28
        // iterations (head_count = 7 × 4) and peels 3 scalar
        // iterations into the exit block.
        let mut module = Module::new("m".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);
        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 31);
        let arr_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let a = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let b = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
        );
        let c = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(arr_ty.clone()),
            arr_ptr_ty.clone(),
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

        let changed = NeonVectorize::new(false).run(&mut module);
        assert!(changed, "scalar tail should let the head vectorize");

        let func = &module.functions[0];
        let body_block = func.block(body);

        // Body has at least one VLoad and one VStore (vectorized head).
        let n_vload = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VLoad(_)))
            .count();
        assert!(n_vload >= 2, "two array loads should become VLoads");
        let n_vstore = body_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::VStore(..)))
            .count();
        assert_eq!(n_vstore, 1, "the destination store should become a VStore");

        // Exit block has 3 peeled scalar Stores (one per tail iter).
        let exit_block = func.block(exit);
        let exit_stores = exit_block
            .insts
            .iter()
            .filter(|i| matches!(i.kind, InstKind::Store(..)))
            .count();
        assert_eq!(
            exit_stores, 3,
            "three scalar stores should be peeled into the exit block"
        );
    }
}
