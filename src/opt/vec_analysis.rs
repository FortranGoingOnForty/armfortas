//! Target-neutral loop analysis extracted from the NEON vectorizer (x10).
//!
//! Detects counted loops, WHERE diamonds, and reduction loops, and builds
//! the vector / where / reduction plans the per-target vectorizer drivers
//! apply. Knows nothing about any ISA beyond the lane count it computes.

use std::collections::{HashMap, HashSet};

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};

use super::loop_utils::{find_preheader, resolve_const_int};
use super::util::{inst_uses, terminator_uses, NaturalLoop};

#[derive(Debug, Clone, Copy)]
pub(crate) struct CountedLoop {
    pub(crate) preheader: BlockId,
    pub(crate) header: BlockId,
    pub(crate) body: BlockId,
    pub(crate) iv_param: ValueId,
    pub(crate) iv_init: i64,
    pub(crate) iv_bound: i64,
    /// The header's `icmp le|lt iv, hi_const` instruction id. Needed
    /// when scalar-tail peeling has to retarget the loop's bound to
    /// `iv_init + head_count - 1`.
    pub(crate) cond_id: ValueId,
    /// The ConstInt feeding the icmp's RHS. `apply_vector_plan` does
    /// not mutate it in place (could be aliased) but inserts a fresh
    /// const and rewires the icmp.
    pub(crate) bound_const_id: ValueId,
}

#[derive(Debug, Clone)]
pub(crate) struct ArrayAccess {
    pub(crate) base: ValueId,
    pub(crate) elem_ty: IrType,
    pub(crate) len: u64,
    pub(crate) lower: i64,
}

/// A counted WHERE-block loop. The natural-loop body is a 4-block
/// diamond: header (cmp + cond_br exit/body) → body (load + cmp +
/// cond_br then/incr) → then (conditional store + br incr) → incr
/// (iv += 1 + br header). The vectorizer rewrites this into:
///
///   body': vload a; vload b_old; v(f|i)cmp predicate; vselect mask, va, vb_old; vstore;
///          drop the `then` block, branch body' → incr unconditionally.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WhereLoop {
    pub(crate) preheader: BlockId,
    pub(crate) header: BlockId,
    /// The body block holding the per-iteration cmp and cond_br
    /// to `then` / `incr`.
    pub(crate) body: BlockId,
    /// The "then" arm with the conditional store(s).
    pub(crate) then_block: BlockId,
    /// The "else" arm, when WHERE/ELSEWHERE is used. The block
    /// branches unconditionally to `incr_block`. When `None`, the
    /// body's false branch goes directly to `incr_block` (single-arm
    /// WHERE).
    pub(crate) else_block: Option<BlockId>,
    /// The latch / incr block (iv + 1, br header).
    pub(crate) incr_block: BlockId,
    pub(crate) iv_param: ValueId,
    pub(crate) iv_init: i64,
    pub(crate) iv_bound: i64,
    /// Header `icmp ge|gt iv, hi` (body on FALSE branch).
    pub(crate) cond_id: ValueId,
    pub(crate) bound_const_id: ValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryKind {
    Add,
    Sub,
    Mul,
    /// Float-only — NEON has no integer vector divide.
    Div,
    /// Element-wise `max(lhs, rhs)`. IR shape is
    /// `select(cmp ge|gt lhs, rhs, lhs, rhs)`.
    Max,
    /// Element-wise `min(lhs, rhs)`. IR shape is
    /// `select(cmp le|lt lhs, rhs, lhs, rhs)`.
    Min,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryKind {
    Neg,
    Abs,
    Sqrt,
}

/// One operand of the body's binop, classified as either an array
/// `Load` that becomes a `VLoad` or a loop-invariant scalar that
/// becomes a `VBroadcast` hoisted into the preheader.
#[derive(Debug, Clone)]
pub(crate) enum BinopOperand {
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
pub(crate) enum BodyOp {
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
    /// `dest(i) = a*b + c` — element-wise FMA. Float-only (NEON has
    /// `fmla.4s` / `fmla.2d` for floats; integer `mla.4s` exists but
    /// VFma in our IR is float). At least one of {a,b,c} must be
    /// an array load; the others can be invariant scalars (broadcast).
    Fma {
        a: BinopOperand,
        b: BinopOperand,
        c: BinopOperand,
        fmul_id: ValueId,
        fadd_id: ValueId,
    },
}

/// One element-wise statement (one store) inside a multi-statement
/// vectorizable body. All statements in a `VectorPlan` share the
/// same lane count and element type.
#[derive(Debug, Clone)]
pub(crate) struct Statement {
    /// What expression feeds the store.
    pub(crate) op: BodyOp,
    /// Original Store instruction ID to be rewritten to VStore.
    pub(crate) store: ValueId,
}

/// Concrete plan: one or more element-wise statements (each up to
/// two array loads or one load + one invariant scalar plus one
/// array store) sharing the same iteration space and element type.
#[derive(Debug, Clone)]
pub(crate) struct VectorPlan {
    pub(crate) lanes: u8,
    pub(crate) elem_ty: IrType,
    /// Every statement that feeds a store in the body.
    pub(crate) statements: Vec<Statement>,
    /// Original `iadd iv, 1` step instruction in the body.
    pub(crate) step_iadd: ValueId,
    /// The `1` ConstInt used as the step (for replacement with V).
    pub(crate) step_const: ValueId,
    /// Width of the IV ConstInt (i32 for typical 1..N loops).
    pub(crate) iv_int_width: IntWidth,
    /// Number of vector iterations × `lanes` = head iteration count.
    /// When `tail_count == 0` the loop fully vectorizes; otherwise we
    /// peel `tail_count` scalar iterations into the exit block.
    pub(crate) head_count: i64,
    /// Remaining iterations after the head (always `< lanes`).
    pub(crate) tail_count: i64,
    /// Span to use for synthesised instructions.
    pub(crate) span: crate::lexer::Span,
}

pub(crate) fn detect_counted_loop(
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
    let (iv_bound, bound_const_id) = match cond_inst.kind {
        InstKind::ICmp(CmpOp::Le, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?, rhs)
        }
        InstKind::ICmp(CmpOp::Lt, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?.checked_sub(1)?, rhs)
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
        cond_id,
        bound_const_id,
    })
}

/// Detect a counted WHERE-block diamond:
///   header(iv): icmp ge iv, hi; cond_br c, exit, body
///   body: load + cmp + cond_br mask, then, incr
///   then: store(s) + br incr
///   incr: iv+1 + br header(iv+1)
pub(crate) fn detect_where_loop(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<WhereLoop> {
    // 4 blocks: header / body / then / incr (single-arm WHERE).
    // 5 blocks: header / body / then / else / incr (WHERE/ELSEWHERE).
    if lp.latches.len() != 1 || (lp.body.len() != 4 && lp.body.len() != 5) {
        return None;
    }
    let header = lp.header;
    let incr_block = lp.latches[0];
    if incr_block == header {
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
    // Header terminator: cond_br with body on FALSE (exit on TRUE).
    let (cond_id, true_dest, false_dest) = match &header_block.terminator {
        Some(Terminator::CondBranch {
            cond,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) if true_args.is_empty() && false_args.is_empty() => (*cond, *true_dest, *false_dest),
        _ => return None,
    };
    if lp.body.contains(&true_dest) || !lp.body.contains(&false_dest) {
        return None;
    }
    let body = false_dest;
    // Header cmp: `icmp ge iv, hi` (or `gt`, in which case bound is hi-1).
    let cond_inst = header_block.insts.iter().find(|inst| inst.id == cond_id)?;
    let (iv_bound, bound_const_id) = match cond_inst.kind {
        InstKind::ICmp(CmpOp::Ge, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?.checked_sub(1)?, rhs)
        }
        InstKind::ICmp(CmpOp::Gt, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?, rhs)
        }
        _ => return None,
    };
    // Body terminator: cond_br to {then, incr}, with incr being the
    // latch and `then` being a 4th body block.
    let body_block = func.block(body);
    let (then_block, body_else) = match &body_block.terminator {
        Some(Terminator::CondBranch {
            cond: _,
            true_dest,
            true_args,
            false_dest,
            false_args,
        }) if true_args.is_empty() && false_args.is_empty() => (*true_dest, *false_dest),
        _ => return None,
    };
    if !lp.body.contains(&then_block) || !lp.body.contains(&body_else) {
        return None;
    }
    if then_block == header || then_block == incr_block || then_block == body {
        return None;
    }
    // body_else may be either incr_block (single-arm WHERE) or a
    // distinct else_block that itself branches to incr_block
    // (WHERE/ELSEWHERE two-arm).
    let else_block = if body_else == incr_block {
        None
    } else {
        if body_else == header || body_else == body || body_else == then_block {
            return None;
        }
        let else_blk = func.block(body_else);
        match &else_blk.terminator {
            Some(Terminator::Branch(d, args)) if *d == incr_block && args.is_empty() => {}
            _ => return None,
        }
        Some(body_else)
    };
    // The then block must br unconditionally to incr.
    let then_blk = func.block(then_block);
    match &then_blk.terminator {
        Some(Terminator::Branch(d, args)) if *d == incr_block && args.is_empty() => {}
        _ => return None,
    }
    // The incr block must be `iadd iv, 1; br header(iv+1)`.
    let incr_blk = func.block(incr_block);
    match &incr_blk.terminator {
        Some(Terminator::Branch(d, args)) if *d == header && args.len() == 1 => {}
        _ => return None,
    }
    Some(WhereLoop {
        preheader,
        header,
        body,
        then_block,
        else_block,
        incr_block,
        iv_param,
        iv_init,
        iv_bound,
        cond_id,
        bound_const_id,
    })
}

pub(crate) fn build_vector_plan(
    func: &Function,
    shape: &CountedLoop,
    loop_defs: &HashSet<ValueId>,
    isa: &super::vec_isa::VectorIsa,
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
    let trip = shape
        .iv_bound
        .checked_sub(shape.iv_init)
        .and_then(|d| d.checked_add(1))?;
    if trip <= 0 {
        return None;
    }
    // Head count is the largest multiple of `lanes` that fits within
    // the trip; the remainder runs as scalar tail peeled into the
    // exit block.
    let head_count = trip - (trip % lanes as i64);
    if head_count == 0 {
        // Not a single full vector iteration would run — bail.
        return None;
    }
    let tail_count = trip - head_count;
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
        let op = classify_body_op(
            *stored_value,
            &stored_inst.kind,
            func,
            shape,
            &dest,
            loop_defs,
            isa,
        )?;
        statements.push(Statement {
            op,
            store: *store_id,
        });
    }

    // Find the iv-increment in the body.
    let body_term = match &body.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == shape.header && args.len() == 1 => args[0],
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
        head_count,
        tail_count,
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
pub(crate) fn classify_body_op(
    stored_value: ValueId,
    kind: &InstKind,
    func: &Function,
    shape: &CountedLoop,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
    isa: &super::vec_isa::VectorIsa,
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
        InstKind::INeg(src) | InstKind::FNeg(src) => unary_body(
            stored_value,
            UnaryKind::Neg,
            *src,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::FAbs(src) => unary_body(
            stored_value,
            UnaryKind::Abs,
            *src,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::FSqrt(src) => {
            // sqrt is float-only.
            if !matches!(dest.elem_ty, IrType::Float(_)) {
                return None;
            }
            unary_body(
                stored_value,
                UnaryKind::Sqrt,
                *src,
                func,
                shape,
                dest,
                loop_defs,
            )
        }
        InstKind::IAdd(l, r) => binop_body(
            stored_value,
            BinaryKind::Add,
            *l,
            *r,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::ISub(l, r) => binop_body(
            stored_value,
            BinaryKind::Sub,
            *l,
            *r,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::IMul(l, r) => {
            // Integer lane multiply is per-ISA (NEON mul.4s; SSE2
            // has none at baseline).
            if !isa.int_mul {
                return None;
            }
            binop_body(
                stored_value,
                BinaryKind::Mul,
                *l,
                *r,
                func,
                shape,
                dest,
                loop_defs,
            )
        }
        InstKind::FAdd(l, r) => {
            // Detect element-wise FMA: `c(i) = a(i)*b(i) + d(i)`.
            // The store value is FAdd whose one operand is an FMul of
            // two operands (each load or invariant scalar). NEON
            // FMLA is float-only, so gate on a Float dest.
            if matches!(dest.elem_ty, IrType::Float(_)) {
                if let Some(fma) = fma_body(stored_value, *l, *r, func, shape, dest, loop_defs) {
                    return Some(fma);
                }
            }
            binop_body(
                stored_value,
                BinaryKind::Add,
                *l,
                *r,
                func,
                shape,
                dest,
                loop_defs,
            )
        }
        InstKind::FSub(l, r) => binop_body(
            stored_value,
            BinaryKind::Sub,
            *l,
            *r,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::FMul(l, r) => binop_body(
            stored_value,
            BinaryKind::Mul,
            *l,
            *r,
            func,
            shape,
            dest,
            loop_defs,
        ),
        InstKind::FDiv(l, r) => {
            // Integer divide has no NEON form; only floats. The
            // binop_body classifier doesn't itself check element
            // type, but `lane_count_for` already required the dest
            // to be Float for this code path to reach here.
            if !matches!(dest.elem_ty, IrType::Float(_)) {
                return None;
            }
            binop_body(
                stored_value,
                BinaryKind::Div,
                *l,
                *r,
                func,
                shape,
                dest,
                loop_defs,
            )
        }
        // Element-wise `c(i) = max(a(i), b(i))` and `min(...)`. The
        // IR shape is `select(cmp(la, lb), t, f)` where {t, f} is
        // {la, lb}. Only fires when the cmp's operands match the
        // select's true/false arms (in some order); all four slots
        // must be classifiable as load or invariant scalar.
        InstKind::Select(cmp_v, t, f) => {
            let defs = inst_map(func);
            let cmp_inst = defs.get(cmp_v)?;
            let (cmp_op, cmp_a, cmp_b) = match cmp_inst.kind {
                InstKind::ICmp(op, a, b) | InstKind::FCmp(op, a, b) => (op, a, b),
                _ => return None,
            };
            // The select's arms must be exactly the cmp's operands in
            // some order so the result is `max` or `min` of them.
            let bk = if cmp_a == *t && cmp_b == *f {
                match cmp_op {
                    CmpOp::Ge | CmpOp::Gt => BinaryKind::Max,
                    CmpOp::Le | CmpOp::Lt => BinaryKind::Min,
                    _ => return None,
                }
            } else if cmp_a == *f && cmp_b == *t {
                match cmp_op {
                    CmpOp::Ge | CmpOp::Gt => BinaryKind::Min,
                    CmpOp::Le | CmpOp::Lt => BinaryKind::Max,
                    _ => return None,
                }
            } else {
                return None;
            };
            // Integer lane min/max is per-ISA (NEON smax/smin.4s;
            // SSE2 baseline has no i32 form).
            if matches!(dest.elem_ty, IrType::Int(_)) && !isa.int_min_max {
                return None;
            }
            binop_body(stored_value, bk, *t, *f, func, shape, dest, loop_defs)
        }
        _ => None,
    }
}

/// Classify a body that is `dest(i) = -src` or `dest(i) = abs(src)`.
/// `src` must be an array load (a unary on an invariant scalar would
/// just be a constant fill).
pub(crate) fn unary_body(
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
pub(crate) fn binop_body(
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

/// Classify a body that's `dest(i) = (a*b) + c` (or `c + (a*b)`).
/// `fadd_id` is the FAdd's value id; `lhs_v` and `rhs_v` are its
/// operands. One of them must itself be an `FMul` whose two operands
/// are each load-or-invariant-scalar; the other operand is `c`.
pub(crate) fn fma_body(
    fadd_id: ValueId,
    lhs_v: ValueId,
    rhs_v: ValueId,
    func: &Function,
    shape: &CountedLoop,
    dest: &ArrayAccess,
    loop_defs: &HashSet<ValueId>,
) -> Option<BodyOp> {
    let defs = inst_map(func);
    let try_fmul = |fmul_v: ValueId, other_v: ValueId| -> Option<BodyOp> {
        let fmul_inst = defs.get(&fmul_v)?;
        let (a_v, b_v) = match fmul_inst.kind {
            InstKind::FMul(a, b) => (a, b),
            _ => return None,
        };
        let a = classify_binop_operand(func, a_v, shape.iv_param, dest, loop_defs)?;
        let b = classify_binop_operand(func, b_v, shape.iv_param, dest, loop_defs)?;
        let c = classify_binop_operand(func, other_v, shape.iv_param, dest, loop_defs)?;
        // At least one operand must be an array load — otherwise
        // there's no per-iteration data to vectorize.
        if matches!(a, BinopOperand::InvariantScalar(_))
            && matches!(b, BinopOperand::InvariantScalar(_))
            && matches!(c, BinopOperand::InvariantScalar(_))
        {
            return None;
        }
        Some(BodyOp::Fma {
            a,
            b,
            c,
            fmul_id: fmul_v,
            fadd_id,
        })
    };
    if let Some(op) = try_fmul(lhs_v, rhs_v) {
        return Some(op);
    }
    try_fmul(rhs_v, lhs_v)
}

/// Classify one operand of the body's binop as either a load from
/// the destination array's iteration space (which becomes a `VLoad`)
/// or a value defined entirely outside the loop (which becomes a
/// preheader `VBroadcast`).
pub(crate) fn classify_binop_operand(
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
pub(crate) struct LoadedArray {
    pub(crate) load_id: ValueId,
    pub(crate) access: ArrayAccess,
}

pub(crate) fn classify_loaded_array(
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

pub(crate) fn classify_array_access(
    func: &Function,
    ptr: ValueId,
    iv_param: ValueId,
) -> Option<ArrayAccess> {
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
    let lower = normalized_index_lower(func, indices[0], iv_param)
        .or_else(|| byte_stride_lower(func, indices[0], iv_param, elem.as_ref()))?;
    Some(ArrayAccess {
        base,
        elem_ty: elem.as_ref().clone(),
        len: *len,
        lower,
    })
}

/// Recognize the byte-stride form `shl(iv, log2(elem_bytes))` (with
/// an optional `IntExtend` between iv and shl). Returns the lower
/// bound (currently only `0`, since the matcher requires the access
/// to start at iv_init = the array's lower bound).
pub(crate) fn byte_stride_lower(
    func: &Function,
    value: ValueId,
    iv_param: ValueId,
    elem_ty: &IrType,
) -> Option<i64> {
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    let (lhs, rhs) = match inst.kind {
        InstKind::Shl(l, r) => (l, r),
        _ => return None,
    };
    let shift = resolve_const_int(func, rhs)?;
    let bytes = elem_size_bytes(elem_ty)?;
    if bytes <= 0 || (1i64 << shift) != bytes {
        return None;
    }
    if lhs == iv_param {
        return Some(0);
    }
    let inner = defs.get(&lhs)?;
    if let InstKind::IntExtend(src, _, _) = inner.kind {
        if src == iv_param {
            return Some(0);
        }
    }
    None
}

pub(crate) fn normalized_index_lower(
    func: &Function,
    value: ValueId,
    iv_param: ValueId,
) -> Option<i64> {
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

pub(crate) fn arrays_compatible(dest: &ArrayAccess, other: &ArrayAccess) -> bool {
    dest.elem_ty == other.elem_ty && dest.len == other.len && dest.lower == other.lower
}

pub(crate) fn covers_full_array(shape: &CountedLoop, access: &ArrayAccess) -> bool {
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

pub(crate) fn loop_values_escape(
    func: &Function,
    lp: &NaturalLoop,
    loop_defs: &HashSet<ValueId>,
) -> bool {
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

pub(crate) fn lane_count_for(elem: &IrType) -> Option<u8> {
    match elem {
        IrType::Int(IntWidth::I32) => Some(4),
        IrType::Int(IntWidth::I64) => Some(2),
        IrType::Float(FloatWidth::F32) => Some(4),
        IrType::Float(FloatWidth::F64) => Some(2),
        _ => None,
    }
}

/// Size of a scalar IR type in bytes. Only used to recognize
/// byte-stride GEP indexing in WHERE-block lowering, where a gep
/// index of `shl(iv, log2(elem_size))` denotes the i-th element.
pub(crate) fn elem_size_bytes(elem: &IrType) -> Option<i64> {
    match elem {
        IrType::Int(w) => Some(w.bytes() as i64),
        IrType::Float(w) => Some((w.bits() / 8) as i64),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ThenBinop {
    pub(crate) inst_id: ValueId,
    pub(crate) kind: BinaryKind,
    /// When `binop_on_load_b == false`: the non-load_a operand. With
    /// `other_is_load_b == false`, this is a loop-invariant scalar
    /// to broadcast; with `other_is_load_b == true`, this is the
    /// second array's `then_load_b` value id (the b-array load
    /// defined in the then_block).
    /// When `binop_on_load_b == true`: this is the loop-invariant
    /// scalar paired with the b-array load (load_a is unused; e.g.
    /// `c = K + d` where d is the second array).
    pub(crate) scalar_v: ValueId,
    /// Whether the "main" load is on the LHS of the binop. The main
    /// load is load_a unless `binop_on_load_b == true`, in which
    /// case it is load_b.
    pub(crate) load_on_lhs: bool,
    /// True iff the non-load_a operand is a second array load (`c = a + b`).
    /// Always false when `binop_on_load_b == true`.
    pub(crate) other_is_load_b: bool,
    /// True iff the binop's main load is load_b (`c = K + d` where d
    /// is the second array). When false, the main load is load_a
    /// (current default — `c = a + K` or `c = a + b`).
    pub(crate) binop_on_load_b: bool,
}

/// Vectorizable WHERE-block plan: one conditional store guarded by
/// a scalar fcmp/icmp predicate. Only the simplest shape is handled
/// for now: the store value is a load of the same pointer used in
/// the predicate (`b(i) = a(i)` under `where (a(i) op K)`).
#[derive(Debug, Clone)]
pub(crate) struct WherePlan {
    pub(crate) lanes: u8,
    pub(crate) elem_ty: IrType,
    /// The load in the body block that feeds the predicate.
    pub(crate) load_a_id: ValueId,
    /// The cmp inst id (in body block).
    pub(crate) cmp_id: ValueId,
    /// Whether the cmp is fcmp (true) or icmp (false).
    pub(crate) cmp_is_float: bool,
    /// The cmp's CmpOp.
    pub(crate) cmp_op: CmpOp,
    /// The threshold operand of the cmp (the other side, not load_a).
    /// Must be loop-invariant.
    pub(crate) threshold_v: ValueId,
    /// Whether `load_a` is on the LHS of the cmp.
    pub(crate) load_on_lhs: bool,
    /// The conditional Store in the then_block.
    pub(crate) store_id: ValueId,
    /// The redundant Load in the then_block (same ptr as load_a) —
    /// will be dropped during rewrite.
    pub(crate) then_load_id: Option<ValueId>,
    /// Optional unary applied to the loaded value before storing
    /// (`b = -a`, `b = abs(a)`, `b = sqrt(a)`). When `Some(uid, kind,
    /// on_load_b)`, `uid` is the inst id in then_block, `kind` is
    /// the unary kind, and `on_load_b == true` means the unary is
    /// applied to the second-array load (`c = -d`) rather than
    /// load_a.
    pub(crate) then_unary: Option<(ValueId, UnaryKind, bool)>,
    /// Optional binop applied to the loaded value with a loop-invariant
    /// scalar (`b = a + K`, `b = a * scale`, etc.). The `scalar_v` is
    /// the invariant operand (will be broadcast in preheader);
    /// `load_on_lhs` indicates whether the load is the LHS of the
    /// binop (to preserve operand order for non-commutative ops like
    /// Sub/Div).
    pub(crate) then_binop: Option<ThenBinop>,
    /// Optional loop-invariant scalar that is stored directly
    /// (`where (cond) b = K`). When set, the true arm of the vselect
    /// is `VBroadcast(K)`; no load_a is consumed by the store.
    pub(crate) then_const: Option<ValueId>,
    /// When `then_binop.other_is_load_b == true`, this holds the
    /// b-array's GEP ptr (so the apply path can emit a VLoad on it).
    pub(crate) b_ptr_id: Option<ValueId>,
    /// When the WHERE has an ELSEWHERE arm, the value to store in
    /// the false-mask lanes. Currently supports a loop-invariant
    /// scalar (`elsewhere; b = K; end where`) — broadcast in the
    /// preheader and used as vselect's false arm in lieu of the
    /// dest's prior value.
    pub(crate) else_const: Option<ValueId>,
    /// When ELSEWHERE loads from a different array
    /// (`elsewhere; c = d; end where`), this is the body-defined
    /// GEP ptr to load via VLoad for the false-mask lanes.
    pub(crate) else_load_ptr: Option<ValueId>,
    /// Unary lifted from `elsewhere; b = -d` / `abs(d)` / `sqrt(d)`.
    /// When `Some`, the apply path applies a V-unary to the
    /// else_load_ptr's vload before feeding vselect's false arm.
    pub(crate) else_unary: Option<UnaryKind>,
    /// Binop lifted from `elsewhere; b = d + K` / `d * scale`.
    /// `(BinaryKind, scalar_v, load_on_lhs)`.
    pub(crate) else_binop: Option<(BinaryKind, ValueId, bool)>,
    /// The dest pointer GEP (computed in body block).
    pub(crate) dest_ptr_id: ValueId,
    /// Source array's access shape (where load_a reads from).
    pub(crate) src_access: ArrayAccess,
    /// Destination array's access shape (where the store writes to).
    pub(crate) dest_access: ArrayAccess,
    pub(crate) span: crate::lexer::Span,
}

pub(crate) fn build_where_plan(
    func: &Function,
    shape: &WhereLoop,
    isa: &super::vec_isa::VectorIsa,
) -> Option<WherePlan> {
    let body_block = func.block(shape.body);
    let then_block = func.block(shape.then_block);
    // Reject calls in body or then.
    if body_block
        .insts
        .iter()
        .chain(then_block.insts.iter())
        .any(|inst| matches!(inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)))
    {
        return None;
    }
    // Find the body's cmp (terminator's cond) and its associated load_a.
    let cond_id = match &body_block.terminator {
        Some(Terminator::CondBranch { cond, .. }) => *cond,
        _ => return None,
    };
    let cmp_inst = body_block.insts.iter().find(|i| i.id == cond_id)?;
    let (cmp_op, lhs_v, rhs_v, cmp_is_float) = match cmp_inst.kind {
        InstKind::FCmp(op, l, r) => (op, l, r, true),
        InstKind::ICmp(op, l, r) => (op, l, r, false),
        _ => return None,
    };
    // One of {lhs_v, rhs_v} must be a Load in body block; the other
    // must be loop-invariant (typically a ConstFloat / ConstInt).
    let body_loads: Vec<&Inst> = body_block
        .insts
        .iter()
        .filter(|i| matches!(i.kind, InstKind::Load(_)))
        .collect();
    if body_loads.len() != 1 {
        return None;
    }
    let load_a = body_loads[0];
    let load_a_id = load_a.id;
    let load_a_ptr = match load_a.kind {
        InstKind::Load(p) => p,
        _ => return None,
    };
    let (threshold_v, load_on_lhs) = if lhs_v == load_a_id {
        (rhs_v, true)
    } else if rhs_v == load_a_id {
        (lhs_v, false)
    } else {
        return None;
    };
    // Threshold must be defined OUTSIDE the loop body (loop-invariant).
    // Conservative: require it to be a Const* in the function (any block),
    // not defined in body, then, or incr.
    let body_ids: HashSet<ValueId> = body_block.insts.iter().map(|i| i.id).collect();
    let then_ids: HashSet<ValueId> = then_block.insts.iter().map(|i| i.id).collect();
    let incr_ids: HashSet<ValueId> = func
        .block(shape.incr_block)
        .insts
        .iter()
        .map(|i| i.id)
        .collect();
    if body_ids.contains(&threshold_v)
        || then_ids.contains(&threshold_v)
        || incr_ids.contains(&threshold_v)
    {
        return None;
    }
    // Source array access.
    let src_access = classify_array_access(func, load_a_ptr, shape.iv_param)?;
    // Find the dest pointer GEP (computed in body block) and the store.
    // The then block has the conditional store; dest_ptr must be a
    // GEP defined in body.
    let body_geps: Vec<&Inst> = body_block
        .insts
        .iter()
        .filter(|i| matches!(i.kind, InstKind::GetElementPtr(..)))
        .collect();
    if body_geps.is_empty() {
        return None;
    }
    // Walk then-block for the store. Expect optionally a redundant
    // Load (same ptr as load_a), optionally a second array Load (a
    // different body-defined ptr — array+array body), optionally a
    // unary (FNeg/FAbs/FSqrt/INeg) OR a binop with an invariant
    // scalar OR a binop with the second array load, and exactly one
    // Store.
    let body_gep_ids: HashSet<ValueId> = body_geps.iter().map(|i| i.id).collect();
    let mut store_id = None;
    let mut then_load_id = None;
    let mut then_load_b: Option<(ValueId, ValueId)> = None;
    // Unary tracking: (inst_id, kind, on_load_b). on_load_b == true
    // means the unary is applied to the second-array load (`c = -d`)
    // rather than load_a.
    let mut then_unary: Option<(ValueId, UnaryKind, bool)> = None;
    let mut then_binop: Option<ThenBinop> = None;
    let mut store_value = None;
    let mut store_ptr = None;
    for inst in &then_block.insts {
        let is_load_alias = |v: ValueId| v == load_a_id || Some(v) == then_load_id;
        let is_load_b = |v: ValueId| then_load_b.map(|(id, _)| id) == Some(v);
        match inst.kind {
            InstKind::Load(p) if p == load_a_ptr => {
                if then_load_id.is_some() {
                    return None;
                }
                then_load_id = Some(inst.id);
            }
            InstKind::Load(p) if body_gep_ids.contains(&p) => {
                // Second array load — array+array body (`c = a + b`).
                // Must be a different ptr than load_a's. We accept at
                // most one such load.
                if then_load_b.is_some() {
                    return None;
                }
                then_load_b = Some((inst.id, p));
            }
            InstKind::FNeg(src) | InstKind::INeg(src) => {
                if then_unary.is_some() || then_binop.is_some() {
                    return None;
                }
                let on_load_b = if is_load_alias(src) {
                    false
                } else if is_load_b(src) {
                    true
                } else {
                    return None;
                };
                then_unary = Some((inst.id, UnaryKind::Neg, on_load_b));
            }
            InstKind::FAbs(src) => {
                if then_unary.is_some() || then_binop.is_some() {
                    return None;
                }
                let on_load_b = if is_load_alias(src) {
                    false
                } else if is_load_b(src) {
                    true
                } else {
                    return None;
                };
                then_unary = Some((inst.id, UnaryKind::Abs, on_load_b));
            }
            InstKind::FSqrt(src) => {
                if then_unary.is_some() || then_binop.is_some() {
                    return None;
                }
                let on_load_b = if is_load_alias(src) {
                    false
                } else if is_load_b(src) {
                    true
                } else {
                    return None;
                };
                then_unary = Some((inst.id, UnaryKind::Sqrt, on_load_b));
            }
            InstKind::IAdd(l, r)
            | InstKind::ISub(l, r)
            | InstKind::IMul(l, r)
            | InstKind::FAdd(l, r)
            | InstKind::FSub(l, r)
            | InstKind::FMul(l, r)
            | InstKind::FDiv(l, r) => {
                if then_unary.is_some() || then_binop.is_some() {
                    return None;
                }
                let kind = match inst.kind {
                    InstKind::IAdd(..) | InstKind::FAdd(..) => BinaryKind::Add,
                    InstKind::ISub(..) | InstKind::FSub(..) => BinaryKind::Sub,
                    InstKind::IMul(..) if !isa.int_mul => return None,
                    InstKind::IMul(..) | InstKind::FMul(..) => BinaryKind::Mul,
                    InstKind::FDiv(..) => BinaryKind::Div,
                    _ => unreachable!(),
                };
                // Three accepted shapes:
                //   (i)  binop(load_a, scalar)  — current default
                //   (ii) binop(load_a, load_b)  — array+array body
                //   (iii) binop(load_b, scalar) — `c = K + d` where d
                //         is a second array (load_a only feeds cmp).
                let (load_on_lhs, scalar_v, binop_on_load_b) = if is_load_alias(l) {
                    (true, r, false)
                } else if is_load_alias(r) {
                    (false, l, false)
                } else if is_load_b(l) {
                    (true, r, true)
                } else if is_load_b(r) {
                    (false, l, true)
                } else {
                    return None;
                };
                let other_is_load_b = if binop_on_load_b {
                    false
                } else {
                    is_load_b(scalar_v)
                };
                then_binop = Some(ThenBinop {
                    inst_id: inst.id,
                    kind,
                    scalar_v,
                    load_on_lhs,
                    other_is_load_b,
                    binop_on_load_b,
                });
            }
            InstKind::Store(v, p) => {
                if store_id.is_some() {
                    return None;
                }
                store_id = Some(inst.id);
                store_value = Some(v);
                store_ptr = Some(p);
            }
            _ => return None,
        }
    }
    let store_id = store_id?;
    let store_value = store_value?;
    let store_ptr = store_ptr?;
    // The store value must be either: load_a, the redundant then-load,
    // the then-block unary, the then-block binop, or a loop-invariant
    // scalar (typically a literal constant — `where (cond) b = K`).
    let unary_id = then_unary.map(|(id, _, _)| id);
    let binop_id = then_binop.map(|b| b.inst_id);
    let mut then_const: Option<ValueId> = None;
    if store_value != load_a_id
        && Some(store_value) != then_load_id
        && Some(store_value) != unary_id
        && Some(store_value) != binop_id
    {
        // Accept iff the store value is loop-invariant (defined
        // outside body / then / incr). The scalar will be broadcast
        // in the preheader and routed through vselect's true arm.
        if body_ids.contains(&store_value)
            || then_ids.contains(&store_value)
            || incr_ids.contains(&store_value)
        {
            return None;
        }
        // Element type of the store value must match the dest array
        // element type — defer the check until dest_access is known
        // (validated below alongside src/dest type match).
        then_const = Some(store_value);
    }
    // For binop: scalar operand must be loop-invariant (not defined
    // in body, then, or incr). The `other_is_load_b == true` case
    // (a + b) doesn't have a scalar; everything else does. FDiv is
    // float-only.
    if let Some(b) = then_binop {
        if !b.other_is_load_b
            && (body_ids.contains(&b.scalar_v)
                || then_ids.contains(&b.scalar_v)
                || incr_ids.contains(&b.scalar_v))
        {
            return None;
        }
        if matches!(b.kind, BinaryKind::Div) && !matches!(src_access.elem_ty, IrType::Float(_)) {
            return None;
        }
    }
    // FSqrt is float-only; INeg is int-only. The dest elem type
    // must match.
    if let Some((_, k, _)) = then_unary {
        match (&src_access.elem_ty, k) {
            (IrType::Float(_), UnaryKind::Neg)
            | (IrType::Float(_), UnaryKind::Abs)
            | (IrType::Float(_), UnaryKind::Sqrt)
            | (IrType::Int(_), UnaryKind::Neg) => {}
            _ => return None,
        }
    }
    let dest_access = classify_array_access(func, store_ptr, shape.iv_param)?;
    // Both src and dest must cover the full array.
    let trip = shape.iv_bound.checked_sub(shape.iv_init)?.checked_add(1)?;
    let src_upper = src_access
        .lower
        .checked_add(src_access.len as i64)
        .and_then(|v| v.checked_sub(1))?;
    let dest_upper = dest_access
        .lower
        .checked_add(dest_access.len as i64)
        .and_then(|v| v.checked_sub(1))?;
    if shape.iv_init != src_access.lower
        || shape.iv_init != dest_access.lower
        || shape.iv_bound != src_upper
        || shape.iv_bound != dest_upper
    {
        return None;
    }
    if src_access.elem_ty != dest_access.elem_ty {
        return None;
    }
    // ELSEWHERE arm: walk the else_block (when present). Shapes:
    //   (a) `Store(invariant_const, dest_ptr)` — broadcast-in-preheader.
    //   (b) `Load(body_gep_ptr); Store(load_val, dest_ptr)` — VLoad
    //       on the body-defined ptr (e.g., `elsewhere; c = d`).
    //   (c) `Load(p); FNeg/FAbs/FSqrt/INeg(load); Store(unary, dest)`.
    //   (d) `Load(p); binop(load, K); Store(binop, dest)` where K is
    //       loop-invariant.
    type ElseArmInfo = (
        Option<ValueId>,
        Option<ValueId>,
        Option<UnaryKind>,
        Option<(BinaryKind, ValueId, bool)>,
    );
    let (else_const, else_load_ptr, else_unary, else_binop): ElseArmInfo =
        if let Some(else_blk_id) = shape.else_block {
            let else_blk = func.block(else_blk_id);
            if else_blk
                .insts
                .iter()
                .any(|inst| matches!(inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)))
            {
                return None;
            }
            let mut else_load: Option<(ValueId, ValueId)> = None;
            let mut e_unary: Option<(ValueId, UnaryKind)> = None;
            let mut e_binop: Option<(ValueId, BinaryKind, ValueId, bool)> = None;
            let mut else_store: Option<(ValueId, ValueId)> = None;
            for inst in &else_blk.insts {
                let is_else_load = |v: ValueId| else_load.map(|(id, _)| id) == Some(v);
                match inst.kind {
                    InstKind::Load(p) if body_gep_ids.contains(&p) => {
                        if else_load.is_some() {
                            return None;
                        }
                        else_load = Some((inst.id, p));
                    }
                    InstKind::FNeg(src) | InstKind::INeg(src) => {
                        if e_unary.is_some() || e_binop.is_some() || !is_else_load(src) {
                            return None;
                        }
                        e_unary = Some((inst.id, UnaryKind::Neg));
                    }
                    InstKind::FAbs(src) => {
                        if e_unary.is_some() || e_binop.is_some() || !is_else_load(src) {
                            return None;
                        }
                        e_unary = Some((inst.id, UnaryKind::Abs));
                    }
                    InstKind::FSqrt(src) => {
                        if e_unary.is_some() || e_binop.is_some() || !is_else_load(src) {
                            return None;
                        }
                        e_unary = Some((inst.id, UnaryKind::Sqrt));
                    }
                    InstKind::IAdd(l, r)
                    | InstKind::ISub(l, r)
                    | InstKind::IMul(l, r)
                    | InstKind::FAdd(l, r)
                    | InstKind::FSub(l, r)
                    | InstKind::FMul(l, r)
                    | InstKind::FDiv(l, r) => {
                        if e_unary.is_some() || e_binop.is_some() {
                            return None;
                        }
                        let kind = match inst.kind {
                            InstKind::IAdd(..) | InstKind::FAdd(..) => BinaryKind::Add,
                            InstKind::ISub(..) | InstKind::FSub(..) => BinaryKind::Sub,
                            InstKind::IMul(..) if !isa.int_mul => return None,
                            InstKind::IMul(..) | InstKind::FMul(..) => BinaryKind::Mul,
                            InstKind::FDiv(..) => BinaryKind::Div,
                            _ => unreachable!(),
                        };
                        let (load_on_lhs, scalar_v) = if is_else_load(l) {
                            (true, r)
                        } else if is_else_load(r) {
                            (false, l)
                        } else {
                            return None;
                        };
                        e_binop = Some((inst.id, kind, scalar_v, load_on_lhs));
                    }
                    InstKind::Store(v, p) => {
                        if else_store.is_some() {
                            return None;
                        }
                        else_store = Some((v, p));
                    }
                    _ => return None,
                }
            }
            let (else_v, else_p) = else_store?;
            if else_p != store_ptr {
                return None;
            }
            let else_ids: HashSet<ValueId> = else_blk.insts.iter().map(|i| i.id).collect();
            let unary_id = e_unary.map(|(id, _)| id);
            let binop_id = e_binop.map(|(id, _, _, _)| id);
            // Determine the case via the store_value.
            if let Some((load_id, load_ptr)) = else_load {
                // Validate the load's access shape covers the full span.
                let acc = classify_array_access(func, load_ptr, shape.iv_param)?;
                let upper = acc
                    .lower
                    .checked_add(acc.len as i64)
                    .and_then(|v| v.checked_sub(1))?;
                if shape.iv_init != acc.lower
                    || shape.iv_bound != upper
                    || acc.elem_ty != src_access.elem_ty
                {
                    return None;
                }
                if Some(else_v) == unary_id {
                    // Case (c): unary on load.
                    let (_, kind) = e_unary.unwrap();
                    match (&src_access.elem_ty, kind) {
                        (IrType::Float(_), UnaryKind::Neg)
                        | (IrType::Float(_), UnaryKind::Abs)
                        | (IrType::Float(_), UnaryKind::Sqrt)
                        | (IrType::Int(_), UnaryKind::Neg) => {}
                        _ => return None,
                    }
                    (None, Some(load_ptr), Some(kind), None)
                } else if Some(else_v) == binop_id {
                    // Case (d): binop on (load, invariant_scalar).
                    let (_, kind, scalar_v, load_on_lhs) = e_binop.unwrap();
                    if body_ids.contains(&scalar_v)
                        || then_ids.contains(&scalar_v)
                        || else_ids.contains(&scalar_v)
                        || incr_ids.contains(&scalar_v)
                    {
                        return None;
                    }
                    if matches!(kind, BinaryKind::Div)
                        && !matches!(src_access.elem_ty, IrType::Float(_))
                    {
                        return None;
                    }
                    (
                        None,
                        Some(load_ptr),
                        None,
                        Some((kind, scalar_v, load_on_lhs)),
                    )
                } else if else_v == load_id {
                    // Case (b): identity load.
                    (None, Some(load_ptr), None, None)
                } else {
                    return None;
                }
            } else {
                // No load in else_block — Case (a): invariant constant.
                if body_ids.contains(&else_v)
                    || then_ids.contains(&else_v)
                    || else_ids.contains(&else_v)
                    || incr_ids.contains(&else_v)
                {
                    return None;
                }
                (Some(else_v), None, None, None)
            }
        } else {
            (None, None, None, None)
        };
    // If the binop's other operand is a b-array load, OR the binop's
    // main load is on b, OR the unary is applied to a b-array load,
    // validate that b's access shape covers the same span and elem
    // type.
    let unary_on_b = then_unary.map(|(_, _, on_b)| on_b).unwrap_or(false);
    let binop_on_b_pair = then_binop
        .map(|b| b.other_is_load_b || b.binop_on_load_b)
        .unwrap_or(false);
    let b_ptr_id = if unary_on_b || binop_on_b_pair {
        let (_b_load_id, b_ptr) = then_load_b?;
        let b_access = classify_array_access(func, b_ptr, shape.iv_param)?;
        let b_upper = b_access
            .lower
            .checked_add(b_access.len as i64)
            .and_then(|v| v.checked_sub(1))?;
        if shape.iv_init != b_access.lower
            || shape.iv_bound != b_upper
            || b_access.elem_ty != src_access.elem_ty
        {
            return None;
        }
        Some(b_ptr)
    } else {
        None
    };
    let elem_ty = src_access.elem_ty.clone();
    let lanes = lane_count_for(&elem_ty)?;
    // Skip tail for v0: require trip divisible by lanes.
    if trip % (lanes as i64) != 0 {
        return None;
    }
    // FCmp requires float dest; ICmp requires int dest.
    match (&elem_ty, cmp_is_float) {
        (IrType::Float(_), true) | (IrType::Int(_), false) => {}
        _ => return None,
    }
    Some(WherePlan {
        lanes,
        elem_ty,
        load_a_id,
        cmp_id: cond_id,
        cmp_is_float,
        cmp_op,
        threshold_v,
        load_on_lhs,
        store_id,
        then_load_id,
        then_unary,
        then_binop,
        then_const,
        b_ptr_id,
        else_const,
        else_load_ptr,
        else_unary,
        else_binop,
        dest_ptr_id: store_ptr,
        src_access,
        dest_access,
        span: cmp_inst.span,
    })
}

pub(crate) fn inst_map(func: &Function) -> HashMap<ValueId, &Inst> {
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| (inst.id, inst))
        .collect()
}

/// What feeds the accumulator on each iteration. `Sum` is a single
/// load; `Dot` multiplies two loads, then adds the product to the
/// accumulator (i.e. dot-product fold).
#[derive(Debug, Clone)]
pub(crate) enum AccumulateSource {
    Sum {
        load_id: ValueId,
    },
    /// `acc' = acc + neg(load)` or `acc' = acc + abs(load)`. The
    /// pre-existing `Sum` rewriter rewrites the load → vload; we
    /// also rewrite the unary `INeg`/`FNeg` → `VNeg` and
    /// `FAbs` → `VAbs` so the pre-fold value flows through the
    /// vector lanes.
    SumWithUnary {
        load_id: ValueId,
        unary_id: ValueId,
        kind: UnaryKind,
    },
    Dot {
        imul_id: ValueId,
        load_a: ValueId,
        load_b: ValueId,
    },
    /// `acc' = acc + (a(i) - b(i))` — sum of differences (variance,
    /// MSE, L1-distance numerator). The body has two loads and one
    /// `ISub`/`FSub` feeding `IAdd`/`FAdd` into the accumulator.
    SumOfDiff {
        sub_id: ValueId,
        load_a: ValueId,
        load_b: ValueId,
    },
}

/// What kind of accumulator combine the body performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReductionKind {
    /// `acc' = acc + value` (or `acc - value`, treated as Sum after
    /// negation — not yet supported).
    Sum,
    /// `acc' = max(acc, load)` lowered as `select(icmp ge, acc, load)`.
    Max,
    /// `acc' = min(acc, load)` lowered as `select(icmp le, acc, load)`.
    Min,
}

/// A sum-reduction loop:
///   `do i = lo, hi; s = s + a(i); end do` (or fadd for floats).
/// or a dot-product fold:
///   `do i = lo, hi; s = s + a(i)*b(i); end do`.
///
/// The loop header carries the IV and a scalar accumulator as block
/// params. The accumulator escapes the loop and is reduced to a
/// scalar `vreduce_sum` after the vectorized body.
#[derive(Debug, Clone)]
pub(crate) struct ReductionPlan {
    pub(crate) preheader: BlockId,
    pub(crate) header: BlockId,
    pub(crate) body: BlockId,
    /// Block reachable when the loop exits (false-dest of header
    /// cond_br).
    pub(crate) exit: BlockId,
    /// Block param indices in the header: `[iv_idx, acc_idx]`.
    pub(crate) iv_param: ValueId,
    pub(crate) acc_param: ValueId,
    pub(crate) acc_param_idx: usize,
    /// Scalar accumulator init value passed in the preheader's branch.
    pub(crate) acc_init: ValueId,
    /// What computes the per-iteration value to fold into `acc`.
    pub(crate) source: AccumulateSource,
    /// What combine op the body performs (sum / min / max).
    pub(crate) reduce: ReductionKind,
    /// Original `acc' = ...` instruction (the IAdd, FAdd, or
    /// `select(icmp, ...)` whose result feeds back into the header).
    /// For min/max the icmp is part of the rewrite too — we hoist
    /// the `select+icmp` pair into a single `vmin`/`vmax`.
    pub(crate) accumulate_id: ValueId,
    /// For Min/Max, the icmp instruction we'll discard during the
    /// rewrite (its result is dead once the select becomes vmin/vmax).
    pub(crate) cmp_id: Option<ValueId>,
    /// Original `iv' = iv + 1` instruction.
    pub(crate) step_iadd: ValueId,
    /// The `1` ConstInt operand of the iv step.
    pub(crate) step_const: ValueId,
    /// IV ConstInt width.
    pub(crate) iv_int_width: IntWidth,
    /// Element type (i32 / i64 / f32 / f64).
    pub(crate) elem_ty: IrType,
    pub(crate) lanes: u8,
    /// IV's lower bound (preheader passes this as the initial iv).
    pub(crate) iv_init: i64,
    /// Number of vector iterations × `lanes`. When `tail_count > 0`,
    /// the head runs vectorized for `head_count` iterations and the
    /// remaining `tail_count` iterations are peeled as scalar code
    /// after the post-loop `vreduce_*`.
    pub(crate) head_count: i64,
    pub(crate) tail_count: i64,
    /// Header's `icmp le|lt iv, hi_const` instruction id, plus the
    /// const id feeding its RHS. Needed when `tail_count > 0` to
    /// retarget the bound to `iv_init + head_count - 1`.
    pub(crate) cond_id: ValueId,
    pub(crate) bound_const_id: ValueId,
    pub(crate) span: crate::lexer::Span,
}

pub(crate) fn detect_reduction_plan(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
    isa: &super::vec_isa::VectorIsa,
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
    let (iv_bound, bound_const_id) = match cond_inst.kind {
        InstKind::ICmp(CmpOp::Le, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?, rhs)
        }
        InstKind::ICmp(CmpOp::Lt, lhs, rhs) if lhs == iv_param => {
            (resolve_const_int(func, rhs)?.checked_sub(1)?, rhs)
        }
        _ => return None,
    };

    let trip = iv_bound
        .checked_sub(iv_init)
        .and_then(|d| d.checked_add(1))?;
    if trip <= 0 {
        return None;
    }
    let head_count = trip - (trip % lanes as i64);
    if head_count == 0 {
        return None;
    }
    let tail_count = trip - head_count;

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
    // The acc-update is one of:
    //   acc' = acc + value         (Sum / Dot, IAdd or FAdd)
    //   acc' = select(icmp_ge_or_gt acc, value, acc, value)  (Max)
    //   acc' = select(icmp_le_or_lt acc, value, acc, value)  (Min)
    let accumulate_inst = defs.get(&body_term_arg_acc)?;
    let (reduce, cmp_id, acc_lhs, acc_rhs) = match (&elem_ty, &accumulate_inst.kind) {
        (IrType::Int(_), InstKind::IAdd(l, r)) => (ReductionKind::Sum, None, *l, *r),
        (IrType::Float(_), InstKind::FAdd(l, r)) => (ReductionKind::Sum, None, *l, *r),
        (_, InstKind::Select(c, t, f)) => {
            // Look at the predicate: it must be `icmp <op> acc, load`
            // (or `icmp <op> load, acc`). The arms must be `(acc,
            // load)` for max/min (so the select picks acc when the
            // predicate is true).
            let cmp_inst = defs.get(c)?;
            let (cmp_op, cmp_a, cmp_b) = match cmp_inst.kind {
                InstKind::ICmp(op, a, b) => (op, a, b),
                InstKind::FCmp(op, a, b) => (op, a, b),
                _ => return None,
            };
            // Identify which side of the icmp / select is the acc
            // and infer Max vs Min.
            //
            //   select(acc >= value, acc, value) → max(acc, value)
            //   select(acc <= value, acc, value) → min(acc, value)
            //   select(value >= acc, value, acc) → max(acc, value)
            //   select(value <= acc, value, acc) → min(acc, value)
            //
            // For NEON SmaxV4S/SminV4S the operand order doesn't
            // matter (commutative).
            let (kind, acc_side, value_side) = if cmp_a == acc_param && *t == acc_param {
                let kind = match cmp_op {
                    CmpOp::Ge | CmpOp::Gt => ReductionKind::Max,
                    CmpOp::Le | CmpOp::Lt => ReductionKind::Min,
                    _ => return None,
                };
                (kind, *t, *f)
            } else if cmp_b == acc_param && *t != acc_param && *f == acc_param {
                let kind = match cmp_op {
                    CmpOp::Le | CmpOp::Lt => ReductionKind::Max,
                    CmpOp::Ge | CmpOp::Gt => ReductionKind::Min,
                    _ => return None,
                };
                (kind, *f, *t)
            } else {
                return None;
            };
            (kind, Some(cmp_inst.id), acc_side, value_side)
        }
        _ => return None,
    };
    // For Sum / Dot (IAdd/FAdd), one operand must be acc_param. For
    // Max/Min the `acc_lhs` is already the acc and `acc_rhs` is the
    // value (set up by the match arm above).
    let (accumulate_id, value_v) = if matches!(reduce, ReductionKind::Sum) {
        if acc_lhs == acc_param {
            (accumulate_inst.id, acc_rhs)
        } else if acc_rhs == acc_param {
            (accumulate_inst.id, acc_lhs)
        } else {
            return None;
        }
    } else {
        (accumulate_inst.id, acc_rhs)
    };
    let value_inst = defs.get(&value_v)?;
    // Classify `value_v` as a load (Sum / Min / Max), an
    // unary-of-load (Sum / Min / Max — folds to VNeg / VAbs), or
    // an imul/fmul of two loads (Sum-only Dot fold). The
    // dot-product fold is meaningless under min/max.
    if !matches!(reduce, ReductionKind::Sum)
        && !matches!(
            value_inst.kind,
            InstKind::Load(_) | InstKind::INeg(_) | InstKind::FNeg(_) | InstKind::FAbs(_)
        )
    {
        return None;
    }
    let source = match (&elem_ty, &value_inst.kind) {
        (_, InstKind::Load(load_ptr)) => {
            let access = classify_array_access(func, *load_ptr, iv_param)?;
            if access.elem_ty != elem_ty {
                return None;
            }
            let upper = access
                .lower
                .checked_add(access.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            if iv_init != access.lower || iv_bound != upper {
                return None;
            }
            AccumulateSource::Sum {
                load_id: value_inst.id,
            }
        }
        // Reductions over `acc + (-load)` / `acc + abs(load)` (Sum)
        // or `max/min(acc, abs(load))` (Min/Max) — the unary
        // applies per-element and lifts cleanly to VNeg / VAbs.
        (IrType::Int(_), InstKind::INeg(load_v))
        | (IrType::Float(_), InstKind::FNeg(load_v))
        | (IrType::Float(_), InstKind::FAbs(load_v)) => {
            let unary_kind = match value_inst.kind {
                InstKind::FAbs(_) => UnaryKind::Abs,
                _ => UnaryKind::Neg,
            };
            let load_inst = defs.get(load_v)?;
            let load_ptr = match load_inst.kind {
                InstKind::Load(p) => p,
                _ => return None,
            };
            let access = classify_array_access(func, load_ptr, iv_param)?;
            if access.elem_ty != elem_ty {
                return None;
            }
            let upper = access
                .lower
                .checked_add(access.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            if iv_init != access.lower || iv_bound != upper {
                return None;
            }
            AccumulateSource::SumWithUnary {
                load_id: load_inst.id,
                unary_id: value_inst.id,
                kind: unary_kind,
            }
        }
        // The integer dot fold multiplies lanes element-wise before
        // the sum, so it needs the ISA's integer lane multiply.
        (IrType::Int(_), InstKind::IMul(..)) if !isa.int_mul => return None,
        (IrType::Int(_), InstKind::IMul(la, lb)) | (IrType::Float(_), InstKind::FMul(la, lb)) => {
            let load_a_inst = defs.get(la)?;
            let load_b_inst = defs.get(lb)?;
            let InstKind::Load(ptr_a) = load_a_inst.kind else {
                return None;
            };
            let InstKind::Load(ptr_b) = load_b_inst.kind else {
                return None;
            };
            let acc_a = classify_array_access(func, ptr_a, iv_param)?;
            let acc_b = classify_array_access(func, ptr_b, iv_param)?;
            if acc_a.elem_ty != elem_ty || acc_b.elem_ty != elem_ty {
                return None;
            }
            let upper_a = acc_a
                .lower
                .checked_add(acc_a.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            let upper_b = acc_b
                .lower
                .checked_add(acc_b.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            if iv_init != acc_a.lower
                || iv_bound != upper_a
                || iv_init != acc_b.lower
                || iv_bound != upper_b
            {
                return None;
            }
            AccumulateSource::Dot {
                imul_id: value_inst.id,
                load_a: load_a_inst.id,
                load_b: load_b_inst.id,
            }
        }
        // `acc + (a(i) - b(i))` — sum of differences. Two loads, one
        // sub feeding the accumulator's add.
        (IrType::Int(_), InstKind::ISub(la, lb)) | (IrType::Float(_), InstKind::FSub(la, lb)) => {
            let load_a_inst = defs.get(la)?;
            let load_b_inst = defs.get(lb)?;
            let InstKind::Load(ptr_a) = load_a_inst.kind else {
                return None;
            };
            let InstKind::Load(ptr_b) = load_b_inst.kind else {
                return None;
            };
            let acc_a = classify_array_access(func, ptr_a, iv_param)?;
            let acc_b = classify_array_access(func, ptr_b, iv_param)?;
            if acc_a.elem_ty != elem_ty || acc_b.elem_ty != elem_ty {
                return None;
            }
            let upper_a = acc_a
                .lower
                .checked_add(acc_a.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            let upper_b = acc_b
                .lower
                .checked_add(acc_b.len as i64)
                .and_then(|v| v.checked_sub(1))?;
            if iv_init != acc_a.lower
                || iv_bound != upper_a
                || iv_init != acc_b.lower
                || iv_bound != upper_b
            {
                return None;
            }
            AccumulateSource::SumOfDiff {
                sub_id: value_inst.id,
                load_a: load_a_inst.id,
                load_b: load_b_inst.id,
            }
        }
        _ => return None,
    };

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
    // the loop besides the accumulate inst (and, for Min/Max, the
    // companion icmp that we'll discard during rewrite).
    let acc_extra_uses: usize = func
        .blocks
        .iter()
        .filter(|b| lp.body.contains(&b.id))
        .flat_map(|b| b.insts.iter())
        .filter(|inst| inst.id != accumulate_id && Some(inst.id) != cmp_id)
        .filter(|inst| inst_uses(&inst.kind).contains(&acc_param))
        .count();
    if acc_extra_uses != 0 {
        return None;
    }
    // Across-lane Min/Max legality is per-ISA: NEON wires i32
    // (smaxv/sminv.4s + umov.s), f32 (fmaxv/fminv.4s), and f64
    // (fmaxp/fminp.2d); SSE2 reduces only the float forms via a
    // shuffle tree.
    let reduce_min_max_legal = match elem_ty {
        IrType::Int(IntWidth::I32) => isa.reduce_min_max_i32,
        IrType::Float(FloatWidth::F32) => isa.reduce_min_max_f32,
        IrType::Float(FloatWidth::F64) => isa.reduce_min_max_f64,
        _ => false,
    };
    if !matches!(reduce, ReductionKind::Sum) && !reduce_min_max_legal {
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
        source,
        reduce,
        accumulate_id,
        cmp_id,
        step_iadd,
        step_const,
        iv_int_width,
        elem_ty,
        lanes,
        iv_init,
        head_count,
        tail_count,
        cond_id,
        bound_const_id,
        span: accumulate_inst.span,
    })
}
