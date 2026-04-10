//! Return propagation for trivial contained helpers.
//!
//! Replaces calls to internal-only helpers that always return the same
//! scalar source with that source directly at the call site. This can
//! erase tiny identity wrappers and constant-result helpers without
//! requiring inlining.

use std::collections::{HashMap, HashSet};

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};
use crate::lexer::{Position, Span};

use super::pass::Pass;
use super::util::substitute_uses;

pub struct ReturnPropagate;

impl Pass for ReturnPropagate {
    fn name(&self) -> &'static str { "return-prop" }

    fn run(&self, module: &mut Module) -> bool {
        let plans = collect_plans(module);
        if plans.iter().all(Option::is_none) {
            return false;
        }

        let mut changed = false;
        for caller_idx in 0..module.functions.len() {
            if rewrite_caller(module, caller_idx, &plans) {
                changed = true;
            }
        }

        changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ScalarConst {
    Int(i64, IntWidth),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReturnSource {
    Param(usize),
    LoadParam(usize),
    Const(ScalarConst),
}

fn is_scalar_type(ty: &IrType) -> bool {
    matches!(ty, IrType::Bool | IrType::Int(_) | IrType::Float(_))
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

fn side_effect_free(kind: &InstKind) -> bool {
    !matches!(kind, InstKind::Store(..) | InstKind::Call(..) | InstKind::RuntimeCall(..))
}

fn classify_return_source(func: &Function, value: ValueId) -> Option<ReturnSource> {
    if let Some((idx, param)) = func.params.iter().enumerate().find(|(_, param)| param.id == value) {
        if !param.ty.is_ptr() && is_scalar_type(&param.ty) {
            return Some(ReturnSource::Param(idx));
        }
    }

    let defs = inst_map(func);
    let inst = defs.get(&value)?;
    match inst.kind {
        InstKind::Load(addr) => {
            let (idx, param) = func.params.iter().enumerate().find(|(_, param)| param.id == addr)?;
            let IrType::Ptr(inner) = &param.ty else {
                return None;
            };
            is_scalar_type(inner.as_ref()).then_some(ReturnSource::LoadParam(idx))
        }
        InstKind::ConstInt(..) | InstKind::ConstFloat(..) | InstKind::ConstBool(..) => {
            const_value_of(func, value).map(ReturnSource::Const)
        }
        _ => None,
    }
}

fn collect_plans(module: &Module) -> Vec<Option<ReturnSource>> {
    let mut plans = vec![None; module.functions.len()];

    for (func_idx, func) in module.functions.iter().enumerate() {
        if !func.internal_only || matches!(func.return_type, IrType::Void) {
            continue;
        }
        if func.blocks.iter().flat_map(|block| block.insts.iter()).any(|inst| !side_effect_free(&inst.kind)) {
            continue;
        }

        let mut agreed = None;
        let mut saw_return = false;
        let mut failed = false;

        for block in &func.blocks {
            match &block.terminator {
                Some(Terminator::Return(Some(value))) => {
                    saw_return = true;
                    let Some(source) = classify_return_source(func, *value) else {
                        failed = true;
                        break;
                    };
                    if let Some(existing) = agreed {
                        if existing != source {
                            failed = true;
                            break;
                        }
                    } else {
                        agreed = Some(source);
                    }
                }
                Some(Terminator::Return(None)) => {
                    failed = true;
                    break;
                }
                _ => {}
            }
        }

        if saw_return && !failed {
            plans[func_idx] = agreed;
        }
    }

    plans
}

fn ensure_const_in_entry(
    func: &mut Function,
    cache: &mut HashMap<ScalarConst, ValueId>,
    value: ScalarConst,
) -> ValueId {
    if let Some(&id) = cache.get(&value) {
        return id;
    }

    let id = func.next_value_id();
    let ty = value.ty();
    func.register_type(id, ty.clone());
    let inst = Inst {
        id,
        kind: value.kind(),
        ty,
        span: default_span(func),
    };
    let entry = func.entry;
    func.block_mut(entry).insts.insert(0, inst);
    cache.insert(value, id);
    id
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
        Span { file_id: 0, start: pos, end: pos }
    }
}

fn wrapper_value_for_call(func: &Function, ptr: ValueId, call_id: ValueId) -> Option<ValueId> {
    let defs = inst_map(func);
    let def = defs.get(&ptr)?;
    match &def.kind {
        InstKind::Alloca(slot_ty) if is_scalar_type(slot_ty) => {}
        _ => return None,
    }

    let mut stored = None;
    for block in &func.blocks {
        for inst in &block.insts {
            let uses = super::util::inst_uses(&inst.kind);
            if !uses.contains(&ptr) {
                continue;
            }
            match &inst.kind {
                InstKind::Store(value, addr) if *addr == ptr => {
                    if stored.replace(*value).is_some() {
                        return None;
                    }
                }
                InstKind::Call(_, args) if inst.id == call_id && args.contains(&ptr) => {}
                _ => return None,
            }
        }
        if let Some(term) = &block.terminator {
            if super::util::terminator_uses(term).contains(&ptr) {
                return None;
            }
        }
    }

    stored
}

fn rewrite_caller(module: &mut Module, caller_idx: usize, plans: &[Option<ReturnSource>]) -> bool {
    let mut scheduled = Vec::new();
    {
        let caller = &module.functions[caller_idx];
        for block in &caller.blocks {
            for inst in &block.insts {
                let InstKind::Call(FuncRef::Internal(callee_idx), args) = &inst.kind else {
                    continue;
                };
                let Some(plan) = plans.get(*callee_idx as usize).copied().flatten() else {
                    continue;
                };
                scheduled.push((inst.id, args.clone(), plan));
            }
        }
    }

    if scheduled.is_empty() {
        return false;
    }

    let caller = &mut module.functions[caller_idx];
    let mut cache = HashMap::new();
    let mut rewrites = HashMap::new();
    let mut remove_ids = HashSet::new();

    for (call_id, args, plan) in scheduled {
        let replacement = match plan {
            ReturnSource::Param(param_idx) => args.get(param_idx).copied(),
            ReturnSource::LoadParam(param_idx) => {
                args.get(param_idx)
                    .copied()
                    .and_then(|ptr| wrapper_value_for_call(caller, ptr, call_id))
            }
            ReturnSource::Const(value) => Some(ensure_const_in_entry(caller, &mut cache, value)),
        };
        let Some(replacement) = replacement else {
            continue;
        };
        rewrites.insert(call_id, replacement);
        remove_ids.insert(call_id);
    }

    if rewrites.is_empty() {
        return false;
    }

    let keys: Vec<ValueId> = rewrites.keys().copied().collect();
    for key in keys {
        let mut cur = rewrites[&key];
        let mut hops = 0usize;
        while let Some(&next) = rewrites.get(&cur) {
            if next == cur || hops > rewrites.len() {
                break;
            }
            cur = next;
            hops += 1;
        }
        rewrites.insert(key, cur);
    }

    for (old, new) in &rewrites {
        substitute_uses(caller, *old, *new);
    }
    for block in &mut caller.blocks {
        block.insts.retain(|inst| !remove_ids.contains(&inst.id));
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst { id, kind, ty: ty.clone(), span: span() });
        f.register_type(id, ty);
        id
    }

    #[test]
    fn propagates_loaded_scalar_dummy_through_wrapper_callsite() {
        let mut module = Module::new("t".into());

        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut callee = Function::new("passthrough".into(), params, IrType::Int(IntWidth::I32));
        callee.internal_only = true;
        let loaded = push(&mut callee, InstKind::Load(ValueId(0)), IrType::Int(IntWidth::I32));
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(loaded)));
        module.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let c7 = push(&mut caller, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let slot = push(
            &mut caller,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let _ = push(&mut caller, InstKind::Store(c7, slot), IrType::Void);
        let call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![slot]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(call)));
        module.add_function(caller);

        assert!(ReturnPropagate.run(&mut module));
        let caller_insts = &module.functions[1].blocks[0].insts;
        assert!(
            !caller_insts.iter().any(|inst| matches!(inst.kind, InstKind::Call(..))),
            "call should be removed after return propagation"
        );
        assert!(
            matches!(module.functions[1].blocks[0].terminator, Some(Terminator::Return(Some(v))) if v == c7),
            "caller should return the wrapped scalar directly"
        );
    }

    #[test]
    fn propagates_constant_result_helper() {
        let mut module = Module::new("t".into());

        let mut callee = Function::new("const_fn".into(), vec![], IrType::Int(IntWidth::I32));
        callee.internal_only = true;
        let c42 = push(&mut callee, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let callee_entry = callee.entry;
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(c42)));
        module.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let call = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let caller_entry = caller.entry;
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(call)));
        module.add_function(caller);

        assert!(ReturnPropagate.run(&mut module));
        let caller_insts = &module.functions[1].blocks[0].insts;
        assert!(
            !caller_insts.iter().any(|inst| matches!(inst.kind, InstKind::Call(..))),
            "constant helper call should be removed"
        );
        assert!(
            caller_insts.iter().any(|inst| matches!(inst.kind, InstKind::ConstInt(42, IntWidth::I32))),
            "caller should materialize the propagated constant"
        );
    }
}
