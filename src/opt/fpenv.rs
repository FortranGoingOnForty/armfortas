//! Floating-point environment effect analysis shared by scalar optimizers.

use crate::ir::inst::{FuncRef, Function, Inst, InstKind, Module};
use crate::ir::types::IrType;
use std::collections::VecDeque;

// Calls that read, clear, restore, or change process-wide floating-point
// environment state. They are barriers for every scalar FP optimizer even
// when a particular entry point (for example `afs_ieee_test_flag`) does not
// itself alter the rounding mode.
const FPENV_BARRIER_EXTERNALS: &[&str] = &[
    "afs_ieee_get_rounding",
    "afs_ieee_set_rounding",
    "afs_ieee_test_flag",
    "afs_ieee_set_flag",
    "afs_ieee_get_status",
    "afs_ieee_set_status",
];

// External calls whose ABI contract preserves the caller's rounding mode.
// Everything else remains conservative, including symbols that merely use
// the runtime's `afs_` prefix.
const ROUNDING_PRESERVING_EXTERNALS: &[&str] = &[
    "afs_write_int",
    "afs_write_newline",
    "memcpy",
    "memset",
    "sinf",
    "cosf",
    "tanf",
    "asinf",
    "acosf",
    "atanf",
    "atan2f",
    "sinhf",
    "coshf",
    "tanhf",
    "expf",
    "expm1f",
    "logf",
    "log2f",
    "log10f",
    "log1pf",
    "sqrtf",
    "cbrtf",
    "fabsf",
    "ceilf",
    "floorf",
    "roundf",
    "truncf",
    "powf",
    "fmodf",
    "hypotf",
    "copysignf",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sinh",
    "cosh",
    "tanh",
    "exp",
    "expm1",
    "log",
    "log2",
    "log10",
    "log1p",
    "sqrt",
    "cbrt",
    "fabs",
    "ceil",
    "floor",
    "round",
    "trunc",
    "pow",
    "fmod",
    "hypot",
    "copysign",
    "j0",
    "j1",
    "y0",
    "y1",
];

fn external_requires_fpenv_barrier(name: &str) -> bool {
    if FPENV_BARRIER_EXTERNALS.contains(&name) {
        return true;
    }
    !ROUNDING_PRESERVING_EXTERNALS.contains(&name)
}

pub(super) struct FpEnvEffects {
    /// The function or one of its callees may change the rounding mode, use an
    /// explicit FP-environment accessor, or cross an unknown external call.
    pub(super) may_cross_fpenv_barrier: Vec<bool>,
    /// The function may execute in the downward closure of that environment.
    pub(super) may_run_in_dynamic_fpenv: Vec<bool>,
}

/// Compute conservative interprocedural FP-environment effects for scalar
/// optimizers.
///
/// Barrier effects propagate to callers. The resulting dynamic environment
/// then propagates to callees, since their arithmetic and status effects are
/// observed in the caller's environment. An indirect call can target any
/// function in the module.
pub(super) fn analyze_fpenv_effects(module: &Module) -> FpEnvEffects {
    let function_count = module.functions.len();
    let mut callers = vec![Vec::new(); function_count];
    let mut callees = vec![Vec::new(); function_count];
    let mut has_indirect_call = vec![false; function_count];
    let mut may_cross_fpenv_barrier = vec![false; function_count];

    for (caller_idx, function) in module.functions.iter().enumerate() {
        for inst in function.blocks.iter().flat_map(|block| block.insts.iter()) {
            let InstKind::Call(callee, _) = &inst.kind else {
                continue;
            };
            match callee {
                FuncRef::Internal(callee_idx) => {
                    if let Some(callee_callers) = callers.get_mut(*callee_idx as usize) {
                        callee_callers.push(caller_idx);
                        callees[caller_idx].push(*callee_idx as usize);
                    } else {
                        may_cross_fpenv_barrier[caller_idx] = true;
                    }
                }
                FuncRef::External(name) => {
                    if external_requires_fpenv_barrier(name) {
                        may_cross_fpenv_barrier[caller_idx] = true;
                    }
                }
                FuncRef::Indirect(_) => {
                    may_cross_fpenv_barrier[caller_idx] = true;
                    has_indirect_call[caller_idx] = true;
                }
            }
        }
    }

    let mut worklist: VecDeque<usize> = may_cross_fpenv_barrier
        .iter()
        .enumerate()
        .filter_map(|(idx, changes)| changes.then_some(idx))
        .collect();
    while let Some(callee_idx) = worklist.pop_front() {
        for &caller_idx in &callers[callee_idx] {
            if !may_cross_fpenv_barrier[caller_idx] {
                may_cross_fpenv_barrier[caller_idx] = true;
                worklist.push_back(caller_idx);
            }
        }
    }

    let mut may_run_in_dynamic_fpenv = may_cross_fpenv_barrier.clone();
    let mut worklist: VecDeque<usize> = may_run_in_dynamic_fpenv
        .iter()
        .enumerate()
        .filter_map(|(idx, dynamic)| dynamic.then_some(idx))
        .collect();
    let mut indirect_targets_marked = false;
    while let Some(caller_idx) = worklist.pop_front() {
        if has_indirect_call[caller_idx] && !indirect_targets_marked {
            indirect_targets_marked = true;
            for (idx, dynamic) in may_run_in_dynamic_fpenv.iter_mut().enumerate() {
                if !*dynamic {
                    *dynamic = true;
                    worklist.push_back(idx);
                }
            }
        }
        for &callee_idx in &callees[caller_idx] {
            if !may_run_in_dynamic_fpenv[callee_idx] {
                may_run_in_dynamic_fpenv[callee_idx] = true;
                worklist.push_back(callee_idx);
            }
        }
    }

    FpEnvEffects {
        may_cross_fpenv_barrier,
        may_run_in_dynamic_fpenv,
    }
}

/// True when an instruction's value depends on the active rounding mode.
pub(super) fn is_rounding_dependent_fp(kind: &InstKind) -> bool {
    matches!(
        kind,
        InstKind::FAdd(..)
            | InstKind::FSub(..)
            | InstKind::FMul(..)
            | InstKind::FDiv(..)
            | InstKind::FSqrt(..)
            | InstKind::FPow(..)
            | InstKind::IntToFloat(..)
            | InstKind::FloatTrunc(..)
    )
}

/// True when removing or reusing an execution may change observable
/// floating-point-environment behavior.
///
/// Rounding-dependent operations qualify because a mode change can alter
/// their value. Comparisons and the remaining conversions also qualify even
/// when their value is rounding-independent: signaling NaNs and invalid or
/// inexact conversions can raise sticky IEEE status flags on each execution.
pub(super) fn is_fpenv_sensitive(kind: &InstKind) -> bool {
    is_rounding_dependent_fp(kind)
        || matches!(
            kind,
            InstKind::FCmp(..) | InstKind::FloatToInt(..) | InstKind::FloatExtend(..)
        )
}

/// True when a scalar instruction can change floating-point environment state.
///
/// Most scalar FP instructions have a floating result, so their result type is
/// enough to distinguish them from vectorizer-produced forms that reuse the
/// scalar opcode. Comparisons and float-to-integer conversions instead expose
/// a non-floating result and must be classified by their input type.
pub(super) fn is_scalar_fpenv_sensitive(func: &Function, inst: &Inst) -> bool {
    if !is_fpenv_sensitive(&inst.kind) {
        return false;
    }

    match &inst.kind {
        InstKind::FCmp(_, value, _) | InstKind::FloatToInt(value, _) => {
            matches!(func.value_type(*value), Some(IrType::Float(_)))
        }
        _ => matches!(inst.ty, IrType::Float(_)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::inst::{CmpOp, Function, Inst, Terminator, ValueId};
    use crate::ir::types::{FloatWidth, IrType};
    use crate::lexer::{Position, Span};

    fn function_with_calls(name: &str, callees: Vec<FuncRef>) -> Function {
        let mut function = Function::new(name.into(), vec![], IrType::Void);
        let entry = function.entry;
        let position = Position { line: 0, col: 0 };
        for callee in callees {
            let id = function.next_value_id();
            function.register_type(id, IrType::Void);
            function.block_mut(entry).insts.push(Inst {
                id,
                kind: InstKind::Call(callee, vec![]),
                ty: IrType::Void,
                span: Span {
                    file_id: 0,
                    start: position,
                    end: position,
                },
            });
        }
        function.block_mut(entry).terminator = Some(Terminator::Return(None));
        function
    }

    #[test]
    fn propagates_rounding_changes_through_internal_wrappers() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_function(function_with_calls(
            "setter",
            vec![
                FuncRef::External("afs_ieee_set_rounding".into()),
                FuncRef::Internal(2),
            ],
        ));
        module.add_function(function_with_calls("wrapper", vec![FuncRef::Internal(0)]));
        module.add_function(function_with_calls("caller", vec![FuncRef::Internal(1)]));

        let effects = analyze_fpenv_effects(&module);
        assert_eq!(effects.may_cross_fpenv_barrier, vec![true, true, true]);
        assert_eq!(effects.may_run_in_dynamic_fpenv, vec![true, true, true]);
    }

    #[test]
    fn marks_indirect_and_unknown_external_calls() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_function(function_with_calls("target", vec![]));
        module.add_function(function_with_calls(
            "indirect",
            vec![FuncRef::Indirect(crate::ir::inst::ValueId(99))],
        ));
        module.add_function(function_with_calls(
            "external",
            vec![FuncRef::External("user_callback".into())],
        ));

        let effects = analyze_fpenv_effects(&module);
        assert_eq!(effects.may_cross_fpenv_barrier, vec![false, true, true]);
        assert_eq!(effects.may_run_in_dynamic_fpenv, vec![true, true, true]);
    }

    #[test]
    fn treats_unlisted_runtime_prefixed_externals_as_unknown() {
        assert!(external_requires_fpenv_barrier("afs_user_callback"));
    }

    #[test]
    fn scalar_effect_classification_excludes_vectorized_scalar_opcodes() {
        let mut function = Function::new("test".into(), vec![], IrType::Void);
        let scalar = ValueId(10);
        let vector = ValueId(11);
        function.register_type(scalar, IrType::Float(FloatWidth::F32));
        function.register_type(
            vector,
            IrType::Vector {
                lanes: 4,
                elem: Box::new(IrType::Float(FloatWidth::F32)),
            },
        );
        let position = Position { line: 0, col: 0 };
        let comparison = |id, value| Inst {
            id: ValueId(id),
            kind: InstKind::FCmp(CmpOp::Eq, value, value),
            ty: IrType::Bool,
            span: Span {
                file_id: 0,
                start: position,
                end: position,
            },
        };

        assert!(is_scalar_fpenv_sensitive(
            &function,
            &comparison(12, scalar)
        ));
        assert!(!is_scalar_fpenv_sensitive(
            &function,
            &comparison(13, vector)
        ));
    }

    #[test]
    fn leaves_preserving_recursion_and_known_runtime_calls_unmarked() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_function(function_with_calls(
            "left",
            vec![FuncRef::Internal(1), FuncRef::External("memcpy".into())],
        ));
        module.add_function(function_with_calls(
            "right",
            vec![
                FuncRef::Internal(0),
                FuncRef::External("afs_write_int".into()),
            ],
        ));

        let effects = analyze_fpenv_effects(&module);
        assert_eq!(effects.may_cross_fpenv_barrier, vec![false, false]);
        assert_eq!(effects.may_run_in_dynamic_fpenv, vec![false, false]);
    }

    #[test]
    fn propagates_changed_environment_to_internal_callees() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_function(function_with_calls("arithmetic", vec![]));
        module.add_function(function_with_calls(
            "caller",
            vec![
                FuncRef::External("afs_ieee_set_rounding".into()),
                FuncRef::Internal(0),
            ],
        ));
        module.add_function(function_with_calls("unrelated", vec![]));

        let effects = analyze_fpenv_effects(&module);
        assert_eq!(effects.may_cross_fpenv_barrier, vec![false, true, false]);
        assert_eq!(effects.may_run_in_dynamic_fpenv, vec![true, true, false]);
    }

    #[test]
    fn propagates_ieee_flag_observation_to_internal_callees() {
        let mut module = Module::new("test".into(), crate::target::TargetLayout::LP64);
        module.add_function(function_with_calls("arithmetic", vec![]));
        module.add_function(function_with_calls(
            "observer",
            vec![
                FuncRef::Internal(0),
                FuncRef::External("afs_ieee_test_flag".into()),
            ],
        ));
        module.add_function(function_with_calls("unrelated", vec![]));

        let effects = analyze_fpenv_effects(&module);
        assert_eq!(effects.may_cross_fpenv_barrier, vec![false, true, false]);
        assert_eq!(effects.may_run_in_dynamic_fpenv, vec![true, true, false]);
    }
}
