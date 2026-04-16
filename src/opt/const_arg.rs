//! Constant-argument specialization for contained procedures.
//!
//! When every internal call site passes the same compile-time constant
//! to a contained helper, rewrite the helper body to materialize that
//! constant directly. `DeadArgElim` then trims the now-unused dummy.

use std::collections::HashMap;

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};
use crate::lexer::{Position, Span};

use super::pass::Pass;
use super::util::substitute_uses;

pub struct ConstArgSpecialize;

impl Pass for ConstArgSpecialize {
    fn name(&self) -> &'static str {
        "const-arg-specialize"
    }

    fn run(&self, module: &mut Module) -> bool {
        let param_modes: Vec<Vec<Option<ParamMode>>> = module
            .functions
            .iter()
            .map(|func| {
                func.params
                    .iter()
                    .map(|param| classify_param(func, param))
                    .collect()
            })
            .collect();

        let plans = specialization_plans(module, &param_modes);
        if plans.is_empty() {
            return false;
        }

        for plan in plans {
            apply_plan(module, plan);
        }

        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamMode {
    ByValueScalar,
    ReadOnlyScalarPtr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarConst {
    Int(i128, IntWidth),
    Float(u64, FloatWidth),
    Bool(bool),
}

impl ScalarConst {
    fn ty(self) -> IrType {
        match self {
            Self::Int(_, width) => IrType::Int(width),
            Self::Float(_, width) => IrType::Float(width),
            Self::Bool(_) => IrType::Bool,
        }
    }

    fn kind(self) -> InstKind {
        match self {
            Self::Int(value, width) => InstKind::ConstInt(value, width),
            Self::Float(bits, width) => match width {
                FloatWidth::F32 => InstKind::ConstFloat(f32::from_bits(bits as u32) as f64, width),
                FloatWidth::F64 => InstKind::ConstFloat(f64::from_bits(bits), width),
            },
            Self::Bool(value) => InstKind::ConstBool(value),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SpecializedParam {
    mode: ParamMode,
    value: ScalarConst,
}

#[derive(Debug, Clone)]
struct SpecializationPlan {
    func_idx: usize,
    params: Vec<Option<SpecializedParam>>,
}

fn is_scalar_type(ty: &IrType) -> bool {
    matches!(ty, IrType::Bool | IrType::Int(_) | IrType::Float(_))
}

fn classify_param(func: &Function, param: &Param) -> Option<ParamMode> {
    if !func.internal_only {
        return None;
    }

    if !param.ty.is_ptr() {
        return is_scalar_type(&param.ty).then_some(ParamMode::ByValueScalar);
    }

    let IrType::Ptr(inner) = &param.ty else {
        return None;
    };
    if !is_scalar_type(inner.as_ref()) {
        return None;
    }
    pointer_param_is_direct_read_only_scalar(func, param.id).then_some(ParamMode::ReadOnlyScalarPtr)
}

fn pointer_param_is_direct_read_only_scalar(func: &Function, param_id: ValueId) -> bool {
    let mut saw_load = false;
    for block in &func.blocks {
        for inst in &block.insts {
            let uses = super::util::inst_uses(&inst.kind);
            if !uses.contains(&param_id) {
                continue;
            }
            match inst.kind {
                InstKind::Load(addr) if addr == param_id => {
                    saw_load = true;
                }
                _ => return false,
            }
        }
        if let Some(term) = &block.terminator {
            if super::util::terminator_uses(term).contains(&param_id) {
                return false;
            }
        }
    }
    saw_load
}

fn inst_map(func: &Function) -> HashMap<ValueId, &Inst> {
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| (inst.id, inst))
        .collect()
}

fn const_value_of(func: &Function, value: ValueId) -> Option<ScalarConst> {
    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    match inst.kind {
        InstKind::ConstInt(v, w) => Some(ScalarConst::Int(v, w)),
        InstKind::ConstFloat(v, w) => Some(ScalarConst::Float(
            match w {
                FloatWidth::F32 => (v as f32).to_bits() as u64,
                FloatWidth::F64 => v.to_bits(),
            },
            w,
        )),
        InstKind::ConstBool(v) => Some(ScalarConst::Bool(v)),
        _ => None,
    }
}

fn wrapper_const_value(
    caller: &Function,
    ptr_value: ValueId,
    param_modes: &[Vec<Option<ParamMode>>],
) -> Option<ScalarConst> {
    let defs = inst_map(caller);
    let def = defs.get(&ptr_value)?;
    match &def.kind {
        InstKind::Alloca(slot_ty) if is_scalar_type(slot_ty) => {}
        _ => return None,
    }

    let mut stored = None;
    let mut saw_call_use = false;

    for block in &caller.blocks {
        for inst in &block.insts {
            let uses = super::util::inst_uses(&inst.kind);
            if !uses.contains(&ptr_value) {
                continue;
            }
            match &inst.kind {
                InstKind::Store(value, addr) if *addr == ptr_value => {
                    let const_value = const_value_of(caller, *value)?;
                    if let Some(existing) = stored {
                        if existing != const_value {
                            return None;
                        }
                    } else {
                        stored = Some(const_value);
                    }
                }
                InstKind::Call(FuncRef::Internal(idx), args) => {
                    let valid = args.iter().enumerate().any(|(arg_idx, arg)| {
                        *arg == ptr_value
                            && param_modes
                                .get(*idx as usize)
                                .and_then(|modes| modes.get(arg_idx))
                                .copied()
                                .flatten()
                                == Some(ParamMode::ReadOnlyScalarPtr)
                    });
                    if !valid {
                        return None;
                    }
                    saw_call_use = true;
                }
                _ => return None,
            }
        }
        if let Some(term) = &block.terminator {
            if super::util::terminator_uses(term).contains(&ptr_value) {
                return None;
            }
        }
    }

    if saw_call_use {
        stored
    } else {
        None
    }
}

fn const_arg_value(
    caller: &Function,
    arg: ValueId,
    mode: ParamMode,
    param_modes: &[Vec<Option<ParamMode>>],
) -> Option<ScalarConst> {
    match mode {
        ParamMode::ByValueScalar => const_value_of(caller, arg),
        ParamMode::ReadOnlyScalarPtr => wrapper_const_value(caller, arg, param_modes),
    }
}

fn specialization_plans(
    module: &Module,
    param_modes: &[Vec<Option<ParamMode>>],
) -> Vec<SpecializationPlan> {
    let mut plans = Vec::new();

    for (func_idx, func) in module.functions.iter().enumerate() {
        if !func.internal_only || func.params.is_empty() {
            continue;
        }

        let mut params = vec![None; func.params.len()];

        for (param_idx, mode) in param_modes[func_idx].iter().copied().enumerate() {
            let Some(mode) = mode else {
                continue;
            };

            let mut agreed = None;
            let mut saw_call = false;
            let mut conflicted = false;

            'callers: for caller in &module.functions {
                for block in &caller.blocks {
                    for inst in &block.insts {
                        let InstKind::Call(FuncRef::Internal(idx), args) = &inst.kind else {
                            continue;
                        };
                        if *idx as usize != func_idx {
                            continue;
                        }
                        saw_call = true;
                        let Some(arg) = args.get(param_idx) else {
                            conflicted = true;
                            break 'callers;
                        };
                        let Some(value) = const_arg_value(caller, *arg, mode, param_modes) else {
                            conflicted = true;
                            break 'callers;
                        };
                        if let Some(existing) = agreed {
                            if existing != value {
                                conflicted = true;
                                break 'callers;
                            }
                        } else {
                            agreed = Some(value);
                        }
                    }
                }
            }

            if saw_call && !conflicted {
                if let Some(value) = agreed {
                    params[param_idx] = Some(SpecializedParam { mode, value });
                }
            }
        }

        if params.iter().any(|param| param.is_some()) {
            plans.push(SpecializationPlan { func_idx, params });
        }
    }

    plans
}

fn default_span(func: &Function) -> Span {
    if let Some(span) = func
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| inst.span)
        .next()
    {
        span
    } else {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }
}

fn apply_plan(module: &mut Module, plan: SpecializationPlan) {
    let func = &mut module.functions[plan.func_idx];
    let span = default_span(func);
    let mut new_consts = Vec::new();
    let mut substitutions = Vec::new();
    let mut remove_loads = Vec::new();

    for (param_idx, specialized) in plan.params.iter().enumerate() {
        let Some(specialized) = specialized else {
            continue;
        };
        let const_id = func.next_value_id();
        let const_ty = specialized.value.ty();
        func.register_type(const_id, const_ty.clone());
        new_consts.push(Inst {
            id: const_id,
            kind: specialized.value.kind(),
            ty: const_ty,
            span,
        });

        let param_id = func.params[param_idx].id;
        match specialized.mode {
            ParamMode::ByValueScalar => {
                substitutions.push((param_id, const_id));
            }
            ParamMode::ReadOnlyScalarPtr => {
                for block in &func.blocks {
                    for inst in &block.insts {
                        if matches!(inst.kind, InstKind::Load(addr) if addr == param_id) {
                            substitutions.push((inst.id, const_id));
                            remove_loads.push(inst.id);
                        }
                    }
                }
            }
        }
    }

    if new_consts.is_empty() {
        return;
    }

    let entry = func.entry;
    func.block_mut(entry).insts.splice(0..0, new_consts);
    for (old, new) in substitutions {
        substitute_uses(func, old, new);
    }
    if !remove_loads.is_empty() {
        for block in &mut func.blocks {
            block.insts.retain(|inst| !remove_loads.contains(&inst.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::lexer::{Position, Span};
    use crate::opt::dead_arg::DeadArgElim;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id,
            kind,
            ty: ty.clone(),
            span: span(),
        });
        f.register_type(id, ty);
        id
    }

    #[test]
    fn specializes_by_value_constant_dummy_and_enables_arg_elision() {
        let mut module = Module::new("t".into());

        let params = vec![
            Param {
                name: "x".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "step".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut callee = Function::new("helper".into(), params, IrType::Int(IntWidth::I32));
        callee.internal_only = true;
        let sum = push(
            &mut callee,
            InstKind::IAdd(ValueId(0), ValueId(1)),
            IrType::Int(IntWidth::I32),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(sum)));
        module.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let x1 = push(
            &mut caller,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let step1 = push(
            &mut caller,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c1 = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![x1, step1]),
            IrType::Int(IntWidth::I32),
        );
        let x2 = push(
            &mut caller,
            InstKind::ConstInt(9, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let step2 = push(
            &mut caller,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c2 = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![x2, step2]),
            IrType::Int(IntWidth::I32),
        );
        let total = push(
            &mut caller,
            InstKind::IAdd(c1, c2),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(total)));
        module.add_function(caller);

        assert!(ConstArgSpecialize.run(&mut module));
        assert!(DeadArgElim.run(&mut module));

        assert_eq!(
            module.functions[0].params.len(),
            1,
            "specialized helper should drop constant dummy"
        );
        let call_args_1 = match &module.functions[1].blocks[0].insts[2].kind {
            InstKind::Call(FuncRef::Internal(0), args) => args,
            other => panic!("expected call, got {:?}", other),
        };
        assert_eq!(
            call_args_1.len(),
            1,
            "call site should drop specialized constant arg"
        );
    }

    #[test]
    fn does_not_specialize_when_callsite_constants_disagree() {
        let mut module = Module::new("t".into());

        let params = vec![
            Param {
                name: "x".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "step".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut callee = Function::new("helper".into(), params, IrType::Int(IntWidth::I32));
        callee.internal_only = true;
        let sum = push(
            &mut callee,
            InstKind::IAdd(ValueId(0), ValueId(1)),
            IrType::Int(IntWidth::I32),
        );
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(sum)));
        module.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let x1 = push(
            &mut caller,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let step1 = push(
            &mut caller,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let _ = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![x1, step1]),
            IrType::Int(IntWidth::I32),
        );
        let x2 = push(
            &mut caller,
            InstKind::ConstInt(9, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let step2 = push(
            &mut caller,
            InstKind::ConstInt(4, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![x2, step2]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(call)));
        module.add_function(caller);

        assert!(!ConstArgSpecialize.run(&mut module));
        assert_eq!(module.functions[0].params.len(), 2);
    }
}
