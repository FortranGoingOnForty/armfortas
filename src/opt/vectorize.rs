//! Simple loop vectorization onto bulk runtime kernels.
//!
//! This pass recognizes a narrow but high-value class of ordinary
//! counted `DO` loops over fixed-size local arrays and rewrites them to
//! the existing SIMD-backed runtime bulk kernels used by whole-array
//! lowering. That makes `-O3` / `-Ofast`'s vectorization claim honest
//! even when the source used an explicit scalar loop instead of a whole-
//! array assignment or `DO CONCURRENT`.

use std::collections::{HashMap, HashSet};

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};
use crate::ir::walk::prune_unreachable;

use super::loop_utils::{
    find_preheader, loop_contains_volatile_memory, loop_defined_values, resolve_const_int,
};
use super::pass::Pass;
use super::util::{find_natural_loops, inst_uses, predecessors, terminator_uses, NaturalLoop};

pub struct Vectorize;

impl Pass for Vectorize {
    fn name(&self) -> &'static str {
        "vectorize"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if vectorize_function(func) {
                changed = true;
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
    exit: BlockId,
    iv_param: ValueId,
    iv_init: i64,
    iv_bound: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryKind {
    Add,
    Sub,
    Mul,
}

#[derive(Debug, Clone)]
struct ArrayAccess {
    base: ValueId,
    elem_ty: IrType,
    len: u64,
    lower: i64,
}

#[derive(Debug, Clone)]
enum OperandClass {
    Array(ArrayAccess),
    Scalar(ValueId),
}

#[derive(Debug, Clone, Copy)]
enum KernelPlan {
    Fill {
        kernel: &'static str,
        dest: ValueId,
        len: u64,
        scalar: ValueId,
    },
    ArrayBinary {
        kernel: &'static str,
        dest: ValueId,
        lhs: ValueId,
        rhs: ValueId,
        len: u64,
    },
    ArrayScalar {
        kernel: &'static str,
        dest: ValueId,
        src: ValueId,
        scalar: ValueId,
        len: u64,
    },
    ScalarArray {
        kernel: &'static str,
        dest: ValueId,
        scalar: ValueId,
        src: ValueId,
        len: u64,
    },
}

fn vectorize_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    if loops.is_empty() {
        return false;
    }
    let preds = predecessors(func);

    for lp in &loops {
        if loop_contains_volatile_memory(func, lp) {
            continue;
        }
        let Some(shape) = detect_counted_loop(func, lp, &preds) else {
            continue;
        };
        let loop_defs = loop_defined_values(func, lp);
        if loop_values_escape(func, lp, &loop_defs) {
            continue;
        }
        let Some((plan, span)) = build_kernel_plan(func, lp, &shape, &loop_defs) else {
            continue;
        };
        apply_kernel_plan(func, shape, plan, span);
        prune_unreachable(func);
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

    let body_block = func.block(body);
    let next = match &body_block.terminator {
        Some(Terminator::Branch(dest, args)) if *dest == header && args.len() == 1 => args[0],
        _ => return None,
    };
    if step_size(func, iv_param, next)? != 1 {
        return None;
    }

    Some(CountedLoop {
        preheader,
        header,
        body,
        exit: false_dest,
        iv_param,
        iv_init,
        iv_bound,
    })
}

fn step_size(func: &Function, iv_param: ValueId, next: ValueId) -> Option<i64> {
    if next == iv_param {
        return Some(0);
    }
    let defs = inst_map(func);
    let inst = defs.get(&next)?;
    match inst.kind {
        InstKind::IAdd(lhs, rhs) => {
            if lhs == iv_param {
                resolve_const_int(func, rhs)
            } else if rhs == iv_param {
                resolve_const_int(func, lhs)
            } else {
                None
            }
        }
        InstKind::ISub(lhs, rhs) if lhs == iv_param => resolve_const_int(func, rhs).map(|v| -v),
        _ => None,
    }
}

fn build_kernel_plan(
    func: &Function,
    lp: &NaturalLoop,
    shape: &CountedLoop,
    loop_defs: &HashSet<ValueId>,
) -> Option<(KernelPlan, crate::lexer::Span)> {
    let body = func.block(shape.body);
    if body
        .insts
        .iter()
        .any(|inst| matches!(inst.kind, InstKind::Call(..) | InstKind::RuntimeCall(..)))
    {
        return None;
    }

    let mut stores = body.insts.iter().filter_map(|inst| match inst.kind {
        InstKind::Store(value, ptr) => Some((inst.span, value, ptr)),
        _ => None,
    });
    let (span, stored_value, dest_ptr) = stores.next()?;
    if stores.next().is_some() {
        return None;
    }

    let dest = classify_array_access(func, dest_ptr, shape.iv_param)?;
    if !covers_full_array(shape, &dest) {
        return None;
    }

    let plan = if let Some(scalar) =
        classify_invariant_scalar(func, loop_defs, stored_value, &dest.elem_ty)
    {
        let kernel = fill_kernel_name(&dest.elem_ty)?;
        KernelPlan::Fill {
            kernel,
            dest: dest.base,
            len: dest.len,
            scalar,
        }
    } else {
        classify_store_value(func, lp, shape, loop_defs, stored_value, dest)?
    };

    Some((plan, span))
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

fn classify_store_value(
    func: &Function,
    _lp: &NaturalLoop,
    shape: &CountedLoop,
    loop_defs: &HashSet<ValueId>,
    value: ValueId,
    dest: ArrayAccess,
) -> Option<KernelPlan> {
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    let (op, lhs, rhs) = match inst.kind {
        InstKind::IAdd(lhs, rhs) => (BinaryKind::Add, lhs, rhs),
        InstKind::ISub(lhs, rhs) => (BinaryKind::Sub, lhs, rhs),
        InstKind::IMul(lhs, rhs) => (BinaryKind::Mul, lhs, rhs),
        InstKind::FAdd(lhs, rhs) => (BinaryKind::Add, lhs, rhs),
        InstKind::FSub(lhs, rhs) => (BinaryKind::Sub, lhs, rhs),
        InstKind::FMul(lhs, rhs) => (BinaryKind::Mul, lhs, rhs),
        _ => return None,
    };

    let lhs = classify_operand(func, loop_defs, lhs, &dest.elem_ty, shape.iv_param)?;
    let rhs = classify_operand(func, loop_defs, rhs, &dest.elem_ty, shape.iv_param)?;

    match (lhs, rhs) {
        (OperandClass::Array(lhs), OperandClass::Array(rhs))
            if arrays_compatible(&dest, &lhs) && arrays_compatible(&dest, &rhs) =>
        {
            let kernel = array_binary_kernel_name(op, &dest.elem_ty)?;
            Some(KernelPlan::ArrayBinary {
                kernel,
                dest: dest.base,
                lhs: lhs.base,
                rhs: rhs.base,
                len: dest.len,
            })
        }
        (OperandClass::Array(src), OperandClass::Scalar(scalar))
            if arrays_compatible(&dest, &src) =>
        {
            let kernel = array_scalar_kernel_name(op, &dest.elem_ty)?;
            Some(KernelPlan::ArrayScalar {
                kernel,
                dest: dest.base,
                src: src.base,
                scalar,
                len: dest.len,
            })
        }
        (OperandClass::Scalar(scalar), OperandClass::Array(src))
            if arrays_compatible(&dest, &src) =>
        {
            match op {
                BinaryKind::Add | BinaryKind::Mul => {
                    let kernel = array_scalar_kernel_name(op, &dest.elem_ty)?;
                    Some(KernelPlan::ArrayScalar {
                        kernel,
                        dest: dest.base,
                        src: src.base,
                        scalar,
                        len: dest.len,
                    })
                }
                BinaryKind::Sub => {
                    let kernel = scalar_array_kernel_name(op, &dest.elem_ty)?;
                    Some(KernelPlan::ScalarArray {
                        kernel,
                        dest: dest.base,
                        scalar,
                        src: src.base,
                        len: dest.len,
                    })
                }
            }
        }
        _ => None,
    }
}

fn classify_operand(
    func: &Function,
    loop_defs: &HashSet<ValueId>,
    value: ValueId,
    elem_ty: &IrType,
    iv_param: ValueId,
) -> Option<OperandClass> {
    if let Some(access) = classify_loaded_array(func, value, iv_param) {
        if access.elem_ty == *elem_ty {
            return Some(OperandClass::Array(access));
        }
    }
    classify_invariant_scalar(func, loop_defs, value, elem_ty).map(OperandClass::Scalar)
}

fn classify_invariant_scalar(
    func: &Function,
    loop_defs: &HashSet<ValueId>,
    value: ValueId,
    elem_ty: &IrType,
) -> Option<ValueId> {
    if loop_defs.contains(&value) {
        return None;
    }
    (func.value_type(value).as_ref() == Some(elem_ty)).then_some(value)
}

fn classify_loaded_array(
    func: &Function,
    value: ValueId,
    iv_param: ValueId,
) -> Option<ArrayAccess> {
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    let InstKind::Load(ptr) = inst.kind else {
        return None;
    };
    classify_array_access(func, ptr, iv_param)
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

fn apply_kernel_plan(
    func: &mut Function,
    shape: CountedLoop,
    plan: KernelPlan,
    span: crate::lexer::Span,
) {
    let n_id = ensure_i64_const(func, shape.preheader, kernel_len(plan) as i64, span);
    let (kernel, args) = match plan {
        KernelPlan::Fill {
            kernel,
            dest,
            scalar,
            ..
        } => (kernel, vec![dest, n_id, scalar]),
        KernelPlan::ArrayBinary {
            kernel,
            dest,
            lhs,
            rhs,
            ..
        } => (kernel, vec![dest, lhs, rhs, n_id]),
        KernelPlan::ArrayScalar {
            kernel,
            dest,
            src,
            scalar,
            ..
        } => (kernel, vec![dest, src, scalar, n_id]),
        KernelPlan::ScalarArray {
            kernel,
            dest,
            scalar,
            src,
            ..
        } => (kernel, vec![dest, scalar, src, n_id]),
    };

    let id = func.next_value_id();
    func.register_type(id, IrType::Void);
    let inst = Inst {
        id,
        kind: InstKind::Call(FuncRef::External(kernel.into()), args),
        ty: IrType::Void,
        span,
    };

    let preheader = func.block_mut(shape.preheader);
    preheader.insts.push(inst);
    preheader.terminator = Some(Terminator::Branch(shape.exit, vec![]));
}

fn kernel_len(plan: KernelPlan) -> u64 {
    match plan {
        KernelPlan::Fill { len, .. }
        | KernelPlan::ArrayBinary { len, .. }
        | KernelPlan::ArrayScalar { len, .. }
        | KernelPlan::ScalarArray { len, .. } => len,
    }
}

fn ensure_i64_const(
    func: &mut Function,
    block_id: BlockId,
    value: i64,
    span: crate::lexer::Span,
) -> ValueId {
    let id = func.next_value_id();
    let ty = IrType::Int(IntWidth::I64);
    func.register_type(id, ty.clone());
    func.block_mut(block_id).insts.push(Inst {
        id,
        kind: InstKind::ConstInt(value as i128, IntWidth::I64),
        ty,
        span,
    });
    id
}

fn inst_map(func: &Function) -> HashMap<ValueId, &Inst> {
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| (inst.id, inst))
        .collect()
}

fn fill_kernel_name(ty: &IrType) -> Option<&'static str> {
    match ty {
        IrType::Int(IntWidth::I32) => Some("afs_fill_i32"),
        IrType::Float(FloatWidth::F32) => Some("afs_fill_f32"),
        IrType::Float(FloatWidth::F64) => Some("afs_fill_f64"),
        _ => None,
    }
}

fn array_binary_kernel_name(kind: BinaryKind, ty: &IrType) -> Option<&'static str> {
    match (kind, ty) {
        (BinaryKind::Add, IrType::Int(IntWidth::I32)) => Some("afs_array_add_i32"),
        (BinaryKind::Add, IrType::Float(FloatWidth::F32)) => Some("afs_array_add_f32"),
        (BinaryKind::Add, IrType::Float(FloatWidth::F64)) => Some("afs_array_add_f64"),
        (BinaryKind::Sub, IrType::Int(IntWidth::I32)) => Some("afs_array_sub_i32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_array_sub_f32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_array_sub_f64"),
        (BinaryKind::Mul, IrType::Int(IntWidth::I32)) => Some("afs_array_mul_i32"),
        (BinaryKind::Mul, IrType::Float(FloatWidth::F32)) => Some("afs_array_mul_f32"),
        (BinaryKind::Mul, IrType::Float(FloatWidth::F64)) => Some("afs_array_mul_f64"),
        _ => None,
    }
}

fn array_scalar_kernel_name(kind: BinaryKind, ty: &IrType) -> Option<&'static str> {
    match (kind, ty) {
        (BinaryKind::Add, IrType::Int(IntWidth::I32)) => Some("afs_array_add_scalar_i32"),
        (BinaryKind::Add, IrType::Float(FloatWidth::F32)) => Some("afs_array_add_scalar_f32"),
        (BinaryKind::Add, IrType::Float(FloatWidth::F64)) => Some("afs_array_add_scalar_f64"),
        (BinaryKind::Sub, IrType::Int(IntWidth::I32)) => Some("afs_array_sub_scalar_i32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_array_sub_scalar_f32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_array_sub_scalar_f64"),
        (BinaryKind::Mul, IrType::Int(IntWidth::I32)) => Some("afs_array_mul_scalar_i32"),
        (BinaryKind::Mul, IrType::Float(FloatWidth::F32)) => Some("afs_array_mul_scalar_f32"),
        (BinaryKind::Mul, IrType::Float(FloatWidth::F64)) => Some("afs_array_mul_scalar_f64"),
        _ => None,
    }
}

fn scalar_array_kernel_name(kind: BinaryKind, ty: &IrType) -> Option<&'static str> {
    match (kind, ty) {
        (BinaryKind::Sub, IrType::Int(IntWidth::I32)) => Some("afs_scalar_sub_array_i32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F32)) => Some("afs_scalar_sub_array_f32"),
        (BinaryKind::Sub, IrType::Float(FloatWidth::F64)) => Some("afs_scalar_sub_array_f64"),
        _ => None,
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

    #[test]
    fn vectorizes_simple_array_add_loop() {
        let mut module = Module::new("m".into(), crate::target::TargetLayout::LP64);
        let mut func = Function::new("__prog_vec".into(), vec![], IrType::Void);

        let entry = func.entry;
        let header = func.create_block("do_check");
        let body = func.create_block("do_body");
        let exit = func.create_block("do_exit");

        let a = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32)),
            IrType::Ptr(Box::new(IrType::Array(
                Box::new(IrType::Int(IntWidth::I32)),
                32,
            ))),
        );
        let b = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32)),
            IrType::Ptr(Box::new(IrType::Array(
                Box::new(IrType::Int(IntWidth::I32)),
                32,
            ))),
        );
        let c = push_inst(
            &mut func,
            entry,
            InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 32)),
            IrType::Ptr(Box::new(IrType::Array(
                Box::new(IrType::Int(IntWidth::I32)),
                32,
            ))),
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
        let a_ptr = push_inst(
            &mut func,
            body,
            InstKind::GetElementPtr(a, vec![offset]),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
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
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
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
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
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

        let changed = Vectorize.run(&mut module);
        assert!(
            changed,
            "vectorize should rewrite the counted array-add loop"
        );

        let func = &module.functions[0];
        let entry_block = func.block(entry);
        assert!(
            entry_block.insts.iter().any(|inst| matches!(
                &inst.kind,
                InstKind::Call(FuncRef::External(name), _args) if name == "afs_array_add_i32"
            )),
            "entry should now contain a bulk add kernel call: {:?}",
            entry_block.insts
        );
        assert!(
            func.try_block(header).is_none() || !func.blocks.iter().any(|block| block.id == header),
            "loop header should be pruned after vectorization"
        );
    }
}
