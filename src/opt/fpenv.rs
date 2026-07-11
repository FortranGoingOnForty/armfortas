//! Floating-point environment effect analysis shared by scalar optimizers.

use crate::ir::inst::{FuncRef, InstKind, Module};
use std::collections::VecDeque;

const ROUNDING_SETTERS: &[&str] = &["afs_ieee_set_rounding", "afs_ieee_set_status"];

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

fn external_may_change_rounding(name: &str) -> bool {
    if ROUNDING_SETTERS.contains(&name) {
        return true;
    }
    !ROUNDING_PRESERVING_EXTERNALS.contains(&name)
}

pub(super) struct RoundingEffects {
    /// The function or one of its callees may change the rounding mode.
    pub(super) may_change_rounding: Vec<bool>,
    /// The function may execute after a caller changes the rounding mode.
    pub(super) may_run_after_change: Vec<bool>,
}

/// Compute interprocedural rounding-mode effects for scalar optimizers.
///
/// Change effects propagate to callers. The resulting dynamic environment
/// then propagates to callees, since their arithmetic observes the caller's
/// active mode. An indirect call can target any function in the module.
pub(super) fn analyze_rounding_effects(module: &Module) -> RoundingEffects {
    let function_count = module.functions.len();
    let mut callers = vec![Vec::new(); function_count];
    let mut callees = vec![Vec::new(); function_count];
    let mut has_indirect_call = vec![false; function_count];
    let mut may_change_rounding = vec![false; function_count];

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
                        may_change_rounding[caller_idx] = true;
                    }
                }
                FuncRef::External(name) => {
                    if external_may_change_rounding(name) {
                        may_change_rounding[caller_idx] = true;
                    }
                }
                FuncRef::Indirect(_) => {
                    may_change_rounding[caller_idx] = true;
                    has_indirect_call[caller_idx] = true;
                }
            }
        }
    }

    let mut worklist: VecDeque<usize> = may_change_rounding
        .iter()
        .enumerate()
        .filter_map(|(idx, changes)| changes.then_some(idx))
        .collect();
    while let Some(callee_idx) = worklist.pop_front() {
        for &caller_idx in &callers[callee_idx] {
            if !may_change_rounding[caller_idx] {
                may_change_rounding[caller_idx] = true;
                worklist.push_back(caller_idx);
            }
        }
    }

    let mut may_run_after_change = may_change_rounding.clone();
    let mut worklist: VecDeque<usize> = may_run_after_change
        .iter()
        .enumerate()
        .filter_map(|(idx, dynamic)| dynamic.then_some(idx))
        .collect();
    let mut indirect_targets_marked = false;
    while let Some(caller_idx) = worklist.pop_front() {
        if has_indirect_call[caller_idx] && !indirect_targets_marked {
            indirect_targets_marked = true;
            for (idx, dynamic) in may_run_after_change.iter_mut().enumerate() {
                if !*dynamic {
                    *dynamic = true;
                    worklist.push_back(idx);
                }
            }
        }
        for &callee_idx in &callees[caller_idx] {
            if !may_run_after_change[callee_idx] {
                may_run_after_change[callee_idx] = true;
                worklist.push_back(callee_idx);
            }
        }
    }

    RoundingEffects {
        may_change_rounding,
        may_run_after_change,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::inst::{Function, Inst, Terminator};
    use crate::ir::types::IrType;
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

        let effects = analyze_rounding_effects(&module);
        assert_eq!(effects.may_change_rounding, vec![true, true, true]);
        assert_eq!(effects.may_run_after_change, vec![true, true, true]);
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

        let effects = analyze_rounding_effects(&module);
        assert_eq!(effects.may_change_rounding, vec![false, true, true]);
        assert_eq!(effects.may_run_after_change, vec![true, true, true]);
    }

    #[test]
    fn treats_unlisted_runtime_prefixed_externals_as_unknown() {
        assert!(external_may_change_rounding("afs_user_callback"));
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

        let effects = analyze_rounding_effects(&module);
        assert_eq!(effects.may_change_rounding, vec![false, false]);
        assert_eq!(effects.may_run_after_change, vec![false, false]);
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

        let effects = analyze_rounding_effects(&module);
        assert_eq!(effects.may_change_rounding, vec![false, true, false]);
        assert_eq!(effects.may_run_after_change, vec![true, true, false]);
    }
}
