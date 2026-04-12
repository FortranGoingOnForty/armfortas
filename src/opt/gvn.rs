//! Global Value Numbering (GVN).
//!
//! Extends local CSE to cross-block redundancy elimination. Processes
//! blocks in dominator-tree preorder, maintaining a scoped hash table
//! of value numbers. When a block computes an expression already
//! available from a dominating block, the redundant instruction is
//! replaced with the dominating definition.
//!
//! Uses the same canonical Key structure as local CSE (cse.rs).

use std::collections::HashMap;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{compute_immediate_dominators, dominator_tree_children};
use super::pass::Pass;

pub struct Gvn;

impl Pass for Gvn {
    fn name(&self) -> &'static str { "gvn" }

    fn run(&self, module: &mut Module) -> bool {
        let pure_calls: Vec<PureCallPolicy> = module
            .functions
            .iter()
            .map(PureCallPolicy::for_function)
            .collect();
        let mut changed = false;
        for func in &mut module.functions {
            if gvn_function(func, &pure_calls) { changed = true; }
        }
        changed
    }
}

/// Canonical key for value numbering (same as CSE).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    tag: u32,
    operands: Vec<ValueId>,
    aux: i128,
    name: Option<String>,
    ty: IrType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PureArgPolicy {
    ByValue,
    ReadOnlyWrapperPtr,
    Unsupported,
}

#[derive(Debug, Clone)]
struct PureCallPolicy {
    reusable: bool,
    arg_policies: Vec<PureArgPolicy>,
}

impl PureCallPolicy {
    fn for_function(func: &Function) -> Self {
        let arg_policies = func.params.iter().map(|param| classify_pure_arg(func, param)).collect::<Vec<_>>();
        let reusable = func.is_pure
            && !matches!(func.return_type, IrType::Void)
            && !func.return_type.is_ptr()
            && arg_policies.iter().all(|policy| *policy != PureArgPolicy::Unsupported)
            && !reads_non_argument_memory(func);
        Self {
            reusable,
            arg_policies,
        }
    }
}

/// True if the function body touches any state outside of its
/// arguments — e.g. a module global via `GlobalAddr`, an external
/// call, or a runtime call that reads mutable state.
///
/// Per F2018 15.7 a PURE function is free to *read* common blocks
/// and module variables; it just can't *write* them.  The Fortran
/// `is_pure` flag therefore doesn't imply "result depends only on
/// arguments" — a pure recursive accumulator over a module
/// variable is fully legal, and GVN must not hash-cons such a call
/// across an intervening store to that variable.
///
/// We conservatively reject any callee that contains a `GlobalAddr`
/// or a non-Internal `Call` / `RuntimeCall`.  The existing
/// argument-policy machinery already handles the "reads through
/// argument pointer" case via `ReadOnlyWrapperPtr`.
fn reads_non_argument_memory(func: &Function) -> bool {
    for block in &func.blocks {
        for inst in &block.insts {
            match &inst.kind {
                InstKind::GlobalAddr(_) => return true,
                InstKind::Call(FuncRef::External(_), _) => return true,
                InstKind::RuntimeCall(_, _) => return true,
                _ => {}
            }
        }
    }
    false
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

fn classify_pure_arg(func: &Function, param: &Param) -> PureArgPolicy {
    if !param.ty.is_ptr() {
        return PureArgPolicy::ByValue;
    }
    let IrType::Ptr(inner) = &param.ty else {
        return PureArgPolicy::Unsupported;
    };
    if !is_scalar_type(inner.as_ref()) {
        return PureArgPolicy::Unsupported;
    }
    if pointer_param_is_read_only_scalar(func, param.id) {
        PureArgPolicy::ReadOnlyWrapperPtr
    } else {
        PureArgPolicy::Unsupported
    }
}

fn pointer_param_is_read_only_scalar(func: &Function, param_id: ValueId) -> bool {
    let defs = inst_map(func);
    let mut shadow_slots = Vec::new();

    for block in &func.blocks {
        for inst in &block.insts {
            let uses = super::util::inst_uses(&inst.kind);
            if !uses.contains(&param_id) {
                continue;
            }
            match &inst.kind {
                InstKind::Store(value, addr) if *value == param_id => {
                    let Some(def) = defs.get(addr) else {
                        return false;
                    };
                    match &def.kind {
                        InstKind::Alloca(slot_ty) => {
                            let IrType::Ptr(inner) = slot_ty else {
                                return false;
                            };
                            if !is_scalar_type(inner.as_ref()) {
                                return false;
                            }
                            shadow_slots.push(*addr);
                        }
                        _ => return false,
                    }
                }
                // After mem2reg, the entry shadow slot often folds away and
                // the PURE callee just reads the incoming by-ref scalar
                // directly.
                InstKind::Load(addr) if *addr == param_id => {}
                _ => return false,
            }
        }
        if let Some(term) = &block.terminator {
            if super::util::terminator_uses(term).contains(&param_id) {
                return false;
            }
        }
    }

    for slot in shadow_slots {
        let mut alias_ptrs = Vec::new();
        for block in &func.blocks {
            for inst in &block.insts {
                let uses = super::util::inst_uses(&inst.kind);
                if !uses.contains(&slot) {
                    continue;
                }
                match &inst.kind {
                    InstKind::Store(value, addr) if *value == param_id && *addr == slot => {}
                    InstKind::Load(addr) if *addr == slot => alias_ptrs.push(inst.id),
                    _ => return false,
                }
            }
            if let Some(term) = &block.terminator {
                if super::util::terminator_uses(term).contains(&slot) {
                    return false;
                }
            }
        }

        for alias in alias_ptrs {
            for block in &func.blocks {
                for inst in &block.insts {
                    let uses = super::util::inst_uses(&inst.kind);
                    if !uses.contains(&alias) {
                        continue;
                    }
                    match &inst.kind {
                        InstKind::Load(addr) if *addr == alias => {}
                        _ => return false,
                    }
                }
                if let Some(term) = &block.terminator {
                    if super::util::terminator_uses(term).contains(&alias) {
                        return false;
                    }
                }
            }
        }
    }

    true
}

fn wrapper_alloca_values(
    func: &Function,
    pure_calls: &[PureCallPolicy],
) -> HashMap<ValueId, ValueId> {
    let mut wrappers = HashMap::new();
    let candidates: Vec<ValueId> = func
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter_map(|inst| match &inst.kind {
            InstKind::Alloca(inner_ty) if is_scalar_type(inner_ty) => Some(inst.id),
            _ => None,
        })
        .collect();

    'candidate: for alloca_id in candidates {
        let mut stored_value = None;

        for scan_block in &func.blocks {
            for scan_inst in &scan_block.insts {
                let uses = super::util::inst_uses(&scan_inst.kind);
                if !uses.contains(&alloca_id) {
                    continue;
                }
                match &scan_inst.kind {
                    InstKind::Store(value, addr) if *addr == alloca_id => {
                        if stored_value.replace(*value).is_some() {
                            continue 'candidate;
                        }
                    }
                    InstKind::Call(FuncRef::Internal(idx), args) => {
                        let Some(policy) = pure_calls.get(*idx as usize) else {
                            continue 'candidate;
                        };
                        if !policy.reusable {
                            continue 'candidate;
                        }
                        let valid_arg = args.iter().enumerate().any(|(arg_idx, arg)| {
                            *arg == alloca_id
                                && policy
                                    .arg_policies
                                    .get(arg_idx)
                                    .copied()
                                    == Some(PureArgPolicy::ReadOnlyWrapperPtr)
                        });
                        if !valid_arg {
                            continue 'candidate;
                        }
                    }
                    _ => continue 'candidate,
                }
            }
            if let Some(term) = &scan_block.terminator {
                if super::util::terminator_uses(term).contains(&alloca_id) {
                    continue 'candidate;
                }
            }
        }

        if let Some(value) = stored_value {
            wrappers.insert(alloca_id, value);
        }
    }

    wrappers
}

fn resolve_value(map: &HashMap<ValueId, ValueId>, mut value: ValueId) -> ValueId {
    while let Some(&next) = map.get(&value) {
        if next == value {
            break;
        }
        value = next;
    }
    value
}

fn key_of(
    inst: &Inst,
    replacements: &HashMap<ValueId, ValueId>,
    pure_calls: &[PureCallPolicy],
    wrapper_values: &HashMap<ValueId, ValueId>,
) -> Option<Key> {
    let mk = |tag: u32, ops: Vec<ValueId>, aux: i128| -> Option<Key> {
        Some(Key { tag, operands: ops, aux, name: None, ty: inst.ty.clone() })
    };
    let mk_named = |tag: u32, name: String| -> Option<Key> {
        Some(Key { tag, operands: vec![], aux: 0, name: Some(name), ty: inst.ty.clone() })
    };
    let remap = |v: ValueId| resolve_value(replacements, v);
    fn canon(a: ValueId, b: ValueId) -> Vec<ValueId> {
        if a.0 <= b.0 { vec![a, b] } else { vec![b, a] }
    }

    match &inst.kind {
        // Pure arithmetic — commutative ops get canonicalized operand order.
        InstKind::IAdd(a, b)  => mk(1, canon(remap(*a), remap(*b)), 0),
        InstKind::ISub(a, b)  => mk(2, vec![remap(*a), remap(*b)], 0),
        InstKind::IMul(a, b)  => mk(3, canon(remap(*a), remap(*b)), 0),
        InstKind::IDiv(a, b)  => mk(4, vec![remap(*a), remap(*b)], 0),
        InstKind::IMod(a, b)  => mk(5, vec![remap(*a), remap(*b)], 0),
        InstKind::INeg(a)     => mk(6, vec![remap(*a)], 0),
        InstKind::FAdd(a, b)  => mk(10, canon(remap(*a), remap(*b)), 0),
        InstKind::FSub(a, b)  => mk(11, vec![remap(*a), remap(*b)], 0),
        InstKind::FMul(a, b)  => mk(12, canon(remap(*a), remap(*b)), 0),
        InstKind::FDiv(a, b)  => mk(13, vec![remap(*a), remap(*b)], 0),
        InstKind::FNeg(a)     => mk(14, vec![remap(*a)], 0),
        InstKind::FAbs(a)     => mk(15, vec![remap(*a)], 0),
        InstKind::FSqrt(a)    => mk(16, vec![remap(*a)], 0),
        InstKind::FPow(a, b)  => mk(17, vec![remap(*a), remap(*b)], 0),
        InstKind::ICmp(op, a, b) => {
            let op_val = *op as i128;
            match op {
                CmpOp::Eq | CmpOp::Ne => mk(20, canon(remap(*a), remap(*b)), op_val),
                _ => mk(20, vec![remap(*a), remap(*b)], op_val),
            }
        }
        InstKind::FCmp(op, a, b) => mk(21, vec![remap(*a), remap(*b)], *op as i128),
        InstKind::And(a, b) => mk(30, canon(remap(*a), remap(*b)), 0),
        InstKind::Or(a, b)  => mk(31, canon(remap(*a), remap(*b)), 0),
        InstKind::Not(a)    => mk(32, vec![remap(*a)], 0),
        InstKind::Select(c, t, f) => mk(33, vec![remap(*c), remap(*t), remap(*f)], 0),
        InstKind::BitAnd(a, b)  => mk(40, canon(remap(*a), remap(*b)), 0),
        InstKind::BitOr(a, b)   => mk(41, canon(remap(*a), remap(*b)), 0),
        InstKind::BitXor(a, b)  => mk(42, canon(remap(*a), remap(*b)), 0),
        InstKind::BitNot(a)     => mk(43, vec![remap(*a)], 0),
        InstKind::Shl(a, b)     => mk(44, vec![remap(*a), remap(*b)], 0),
        InstKind::LShr(a, b)    => mk(45, vec![remap(*a), remap(*b)], 0),
        InstKind::AShr(a, b)    => mk(46, vec![remap(*a), remap(*b)], 0),
        InstKind::CountLeadingZeros(a)  => mk(47, vec![remap(*a)], 0),
        InstKind::CountTrailingZeros(a) => mk(48, vec![remap(*a)], 0),
        InstKind::PopCount(a)           => mk(49, vec![remap(*a)], 0),
        // Conversions.
        InstKind::IntToFloat(a, w)   => mk(50, vec![remap(*a)], w.bits() as i128),
        InstKind::FloatToInt(a, w)   => mk(51, vec![remap(*a)], w.bits() as i128),
        InstKind::FloatExtend(a, w)  => mk(52, vec![remap(*a)], w.bits() as i128),
        InstKind::FloatTrunc(a, w)   => mk(53, vec![remap(*a)], w.bits() as i128),
        InstKind::IntExtend(a, w, s) => mk(54, vec![remap(*a)], (w.bits() as i128) * if *s { 1 } else { -1 }),
        InstKind::IntTrunc(a, w)     => mk(55, vec![remap(*a)], w.bits() as i128),
        // Constants.
        InstKind::ConstInt(v, w)   => {
            let bits = w.bits();
            let signed = if bits >= 128 {
                *v
            } else {
                let shift = 128 - bits;
                (*v << shift) >> shift
            };
            mk(60, vec![], signed)
        }
        InstKind::ConstFloat(v, w) => mk(61, vec![], ((*v).to_bits() as i128) ^ (w.bits() as i128)),
        InstKind::ConstBool(v)     => mk(62, vec![], *v as i128),
        // GlobalAddr.
        InstKind::GlobalAddr(name) => mk_named(70, name.clone()),
        // GEP.
        InstKind::GetElementPtr(base, idxs) => {
            let mut ops = vec![remap(*base)];
            ops.extend(idxs.iter().copied().map(remap));
            mk(80, ops, 0)
        }
        // Reusable PURE calls: by-value arguments only, real value return,
        // and no pointer result identity to preserve. For lowered
        // Fortran scalar-by-reference args, look through the wrapper
        // alloca to the stored SSA value when the callee only reads the
        // pointee as a scalar input.
        InstKind::Call(FuncRef::Internal(idx), args) => {
            let policy = pure_calls.get(*idx as usize)?;
            if !policy.reusable || policy.arg_policies.len() != args.len() {
                return None;
            }
            let mut ops = Vec::with_capacity(args.len());
            for (arg, arg_policy) in args.iter().zip(policy.arg_policies.iter()) {
                match arg_policy {
                    PureArgPolicy::ByValue => ops.push(remap(*arg)),
                    PureArgPolicy::ReadOnlyWrapperPtr => {
                        let wrapper = remap(*arg);
                        let stored = wrapper_values.get(&wrapper)?;
                        ops.push(remap(*stored));
                    }
                    PureArgPolicy::Unsupported => return None,
                }
            }
            mk(90, ops, *idx as i128)
        }
        // Impure: loads, stores, runtime calls, external calls, alloca — not GVN candidates.
        InstKind::Load(..) | InstKind::Store(..) | InstKind::Alloca(..)
        | InstKind::Call(..) | InstKind::RuntimeCall(..)
        | InstKind::ConstString(..) | InstKind::Undef(..)
        | InstKind::ExtractField(..) | InstKind::InsertField(..) => None,
    }
}

fn gvn_function(func: &mut Function, pure_calls: &[PureCallPolicy]) -> bool {
    let idoms = compute_immediate_dominators(func);
    let children = dominator_tree_children(&idoms);
    let wrapper_values = wrapper_alloca_values(func, pure_calls);

    // Scoped value number table: Key → dominating ValueId.
    let mut vn_table: HashMap<Key, ValueId> = HashMap::new();
    // Replacement map: redundant ValueId → dominating ValueId.
    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();

    // Process blocks in dominator-tree preorder (DFS from entry).
    let mut stack = vec![(func.entry, 0usize)]; // (block, depth for scope management)
    let mut scope_stack: Vec<Vec<Key>> = Vec::new(); // keys to remove when leaving scope

    while let Some((block_id, depth)) = stack.pop() {
        // Pop scope entries for blocks we've left.
        while scope_stack.len() > depth {
            if let Some(keys) = scope_stack.pop() {
                for key in keys {
                    vn_table.remove(&key);
                }
            }
        }

        // Process this block's instructions.
        let mut new_keys = Vec::new();
        let block = func.block(block_id);
        for inst in &block.insts {
            if let Some(key) = key_of(inst, &replacements, pure_calls, &wrapper_values) {
                if let Some(&existing) = vn_table.get(&key) {
                    // This expression is already available from a dominating block.
                    replacements.insert(inst.id, existing);
                } else {
                    // First occurrence — register in the table.
                    vn_table.insert(key.clone(), inst.id);
                    new_keys.push(key);
                }
            }
        }

        scope_stack.push(new_keys);

        // Push children (reverse order so leftmost child is processed first).
        if let Some(kids) = children.get(&block_id) {
            for &child in kids.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }

    if replacements.is_empty() { return false; }

    // Apply replacements: substitute all uses of redundant values, then
    // drop the now-dead duplicate instructions directly. This matters
    // for PURE calls because DCE conservatively treats generic calls as
    // side-effecting.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.kind = remap_operands(&inst.kind, &replacements);
        }
        block.insts.retain(|inst| !replacements.contains_key(&inst.id));
        if let Some(ref mut term) = block.terminator {
            remap_terminator_operands(term, &replacements);
        }
    }

    true
}

/// Remap operands in an instruction using the replacement map.
fn remap_operands(kind: &InstKind, map: &HashMap<ValueId, ValueId>) -> InstKind {
    super::loop_utils::remap_inst_kind(kind, map)
}

/// Remap operands in a terminator.
fn remap_terminator_operands(term: &mut Terminator, map: &HashMap<ValueId, ValueId>) {
    let r = |v: &ValueId| *map.get(v).unwrap_or(v);
    match term {
        Terminator::Return(Some(v)) => *v = r(v),
        Terminator::Branch(_, args) => {
            for a in args.iter_mut() { *a = r(a); }
        }
        Terminator::CondBranch { cond, true_args, false_args, .. } => {
            *cond = r(cond);
            for a in true_args.iter_mut() { *a = r(a); }
            for a in false_args.iter_mut() { *a = r(a); }
        }
        Terminator::Switch { selector, .. } => {
            *selector = r(selector);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;
    use crate::opt::pass::PassManager;
    use crate::opt::{
        bce::Bce, call_resolve::CallResolve, const_fold::ConstFold, const_prop::ConstProp,
        dead_func::DeadFuncElim, dse::Dse, fusion::LoopFusion, global_lsf::GlobalLsf,
        interchange::LoopInterchange, inline::Inline, licm::Licm, lsf::LocalLsf,
        mem2reg::Mem2Reg, peel::LoopPeel, preheader::PreheaderInsert, simplify_cfg::SimplifyCfg,
        sroa::Sroa, strength_reduce::StrengthReduce, unroll::LoopUnroll, unswitch::LoopUnswitch,
        fission::LoopFission,
    };
    use crate::opt::pipeline::OptLevel;
    use crate::lexer::{Position, Span};
    use crate::lexer::{SourceForm, tokenize};
    use crate::parser::Parser;
    use crate::preprocess::PreprocConfig;
    use crate::sema::{resolve, validate};
    use crate::ir::lower;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    fn push_inst(func: &mut Function, block: BlockId, kind: InstKind, ty: IrType) -> ValueId {
        let id = func.next_value_id();
        func.register_type(id, ty.clone());
        func.block_mut(block).insts.push(Inst { id, kind, ty, span: span() });
        id
    }

    fn lower_fixture(name: &str) -> Module {
        let path = PathBuf::from("test_programs").join(name);
        let source = fs::read_to_string(&path).expect("fixture source");
        let pp = crate::preprocess::preprocess(
            &source,
            &PreprocConfig {
                filename: path.to_string_lossy().into_owned(),
                fixed_form: false,
                ..PreprocConfig::default()
            },
        )
        .expect("preprocess fixture");
        let tokens = tokenize(&pp.text, 0, SourceForm::FreeForm).expect("tokenize fixture");
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().expect("parse fixture");
        let (st, type_layouts) = resolve::resolve_file(&units).expect("resolve fixture");
        let diags = validate::validate_file(&units, &st);
        assert!(
            !diags.iter().any(|diag| diag.kind == validate::DiagKind::Error),
            "fixture should lower cleanly: {:?}",
            diags
        );
        lower::lower_file(&units, &st, &type_layouts).0
    }

    fn build_pre_gvn_o2_pipeline() -> PassManager {
        let mut pm = PassManager::new();
        pm.add(Box::new(CallResolve));
        pm.add(Box::new(Mem2Reg));
        pm.add(Box::new(ConstFold));
        pm.add(Box::new(Sroa));
        pm.add(Box::new(Mem2Reg));
        pm.add(Box::new(Inline::for_level(OptLevel::O2)));
        pm.add(Box::new(SimplifyCfg));
        pm.add(Box::new(DeadFuncElim));
        pm.add(Box::new(Bce));
        pm.add(Box::new(StrengthReduce));
        pm.add(Box::new(LocalLsf));
        pm.add(Box::new(GlobalLsf));
        pm.add(Box::new(crate::opt::cse::LocalCse));
        pm.add(Box::new(PreheaderInsert));
        pm.add(Box::new(LoopPeel));
        pm.add(Box::new(LoopUnswitch));
        pm.add(Box::new(Licm));
        pm.add(Box::new(ConstProp));
        pm.add(Box::new(Dse));
        pm.add(Box::new(LoopInterchange));
        pm.add(Box::new(LoopFission));
        pm.add(Box::new(LoopFusion));
        pm.add(Box::new(LoopUnroll));
        pm
    }

    #[test]
    fn gvn_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Gvn;
        assert!(!pass.run(&mut m));
    }

    #[test]
    fn gvn_handles_max_i128_constant_keys() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Int(IntWidth::I128));
        let entry = f.entry;
        let wide = push_inst(
            &mut f,
            entry,
            InstKind::ConstInt(i128::MAX, IntWidth::I128),
            IrType::Int(IntWidth::I128),
        );
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(wide)));
        m.add_function(f);
        let pass = Gvn;
        assert!(
            !pass.run(&mut m),
            "single max-i128 constant should not produce a spurious rewrite"
        );
    }

    #[test]
    fn gvn_reuses_pure_by_value_calls() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "x".into(),
            ty: IrType::Int(IntWidth::I32),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("square".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(Some(ValueId(0))));
        m.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let c7 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let call1 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![c7]),
            IrType::Int(IntWidth::I32),
        );
        let call2 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![c7]),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(call1, call2),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(caller);

        let pass = Gvn;
        assert!(pass.run(&mut m));

        let caller = &m.functions[1];
        let calls: Vec<&Inst> = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .collect();
        assert_eq!(calls.len(), 1, "redundant PURE call should be removed");
        let kept_call = calls[0].id;

        let add = caller.blocks[0]
            .insts
            .iter()
            .find(|inst| matches!(inst.kind, InstKind::IAdd(..)))
            .expect("caller should still contain the sum");
        match add.kind {
            InstKind::IAdd(lhs, rhs) => {
                assert_eq!(lhs, kept_call);
                assert_eq!(rhs, kept_call);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn gvn_reuses_pure_calls_after_operand_canonicalization() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "x".into(),
            ty: IrType::Int(IntWidth::I32),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("square".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        callee.block_mut(callee.entry).terminator = Some(Terminator::Return(Some(ValueId(0))));
        m.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let a = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(4, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let sum1 = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(a, b),
            IrType::Int(IntWidth::I32),
        );
        let sum2 = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(a, b),
            IrType::Int(IntWidth::I32),
        );
        let call1 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![sum1]),
            IrType::Int(IntWidth::I32),
        );
        let call2 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![sum2]),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(call2)));
        m.add_function(caller);

        let pass = Gvn;
        assert!(pass.run(&mut m));

        let caller = &m.functions[1];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .count();
        assert_eq!(call_count, 1, "PURE call should reuse equivalent dominating args");

        match caller.blocks[0].terminator.as_ref().expect("return terminator") {
            Terminator::Return(Some(v)) => assert_eq!(*v, call1),
            other => panic!("unexpected terminator: {:?}", other),
        }
    }

    #[test]
    fn pure_pointer_param_policy_accepts_read_only_scalar_pattern() {
        let param = Param {
            name: "n".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("heavy_fact".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let entry = callee.entry;

        let slot = push_inst(
            &mut callee,
            entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        push_inst(&mut callee, entry, InstKind::Store(ValueId(0), slot), IrType::Void);
        let arg_ptr = push_inst(
            &mut callee,
            entry,
            InstKind::Load(slot),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let arg_val = push_inst(
            &mut callee,
            entry,
            InstKind::Load(arg_ptr),
            IrType::Int(IntWidth::I32),
        );
        callee.block_mut(entry).terminator = Some(Terminator::Return(Some(arg_val)));

        let policy = PureCallPolicy::for_function(&callee);
        assert!(policy.reusable);
        assert_eq!(policy.arg_policies, vec![PureArgPolicy::ReadOnlyWrapperPtr]);
    }

    #[test]
    fn wrapper_alloca_values_accept_scalar_by_ref_call_pattern() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "n".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("heavy_fact".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let callee_entry = callee.entry;
        let slot = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        push_inst(&mut callee, callee_entry, InstKind::Store(ValueId(0), slot), IrType::Void);
        let arg_ptr = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(slot),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let arg_val = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(arg_ptr),
            IrType::Int(IntWidth::I32),
        );
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(arg_val)));
        m.add_function(callee);

        let pure_calls = vec![PureCallPolicy::for_function(&m.functions[0])];

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let c = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrapper = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, entry, InstKind::Store(c, wrapper), IrType::Void);
        let call = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![wrapper]),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(call)));

        let wrappers = wrapper_alloca_values(&caller, &pure_calls);
        assert_eq!(wrappers.get(&wrapper), Some(&c));
    }

    #[test]
    fn gvn_reuses_pure_calls_through_scalar_wrapper_allocas() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "n".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("heavy_fact".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let callee_entry = callee.entry;
        let shadow = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        push_inst(&mut callee, callee_entry, InstKind::Store(ValueId(0), shadow), IrType::Void);
        let arg_ptr = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let arg_val = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(arg_ptr),
            IrType::Int(IntWidth::I32),
        );
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(arg_val)));
        m.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let first = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let second = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let c1 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c2 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrap1 = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, entry, InstKind::Store(c2, wrap1), IrType::Void);
        let call1 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![wrap1]),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut caller, entry, InstKind::Store(call1, first), IrType::Void);
        let c3 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c4 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrap2 = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, entry, InstKind::Store(c4, wrap2), IrType::Void);
        let call2 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![wrap2]),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut caller, entry, InstKind::Store(call2, second), IrType::Void);
        let left = push_inst(&mut caller, entry, InstKind::Load(first), IrType::Int(IntWidth::I32));
        let right = push_inst(&mut caller, entry, InstKind::Load(second), IrType::Int(IntWidth::I32));
        let sum = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(left, right),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(caller);

        let pure_calls: Vec<PureCallPolicy> = m.functions.iter().map(PureCallPolicy::for_function).collect();
        assert!(pure_calls[0].reusable);
        assert_eq!(pure_calls[0].arg_policies, vec![PureArgPolicy::ReadOnlyWrapperPtr]);
        let wrappers = wrapper_alloca_values(&m.functions[1], &pure_calls);
        assert_eq!(wrappers.get(&wrap1), Some(&c2));
        assert_eq!(wrappers.get(&wrap2), Some(&c4));

        let mut replacements = HashMap::new();
        replacements.insert(c2, c1);
        replacements.insert(c3, c1);
        replacements.insert(c4, c1);
        let caller_before = &m.functions[1];
        let call1_inst = caller_before.blocks[0]
            .insts
            .iter()
            .find(|inst| inst.id == call1)
            .expect("call1 inst");
        let call2_inst = caller_before.blocks[0]
            .insts
            .iter()
            .find(|inst| inst.id == call2)
            .expect("call2 inst");
        let key1 = key_of(call1_inst, &replacements, &pure_calls, &wrappers).expect("call1 should key");
        let key2 = key_of(call2_inst, &replacements, &pure_calls, &wrappers).expect("call2 should key");
        assert_eq!(key1, key2, "wrapper-based call keys should line up before the pass runs");

        let pass = Gvn;
        assert!(pass.run(&mut m));

        let caller = &m.functions[1];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .count();
        assert_eq!(call_count, 1, "wrapper-alloca PURE calls should dedupe");
    }

    #[test]
    fn gvn_reuses_pure_calls_for_recursive_factorial_shape() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "n".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("heavy_fact".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let entry = callee.entry;
        let if_end = callee.create_block("if_end");
        let if_then = callee.create_block("if_then");
        let if_else = callee.create_block("if_else");

        let shadow = push_inst(
            &mut callee,
            entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        push_inst(&mut callee, entry, InstKind::Store(ValueId(0), shadow), IrType::Void);
        let result = push_inst(
            &mut callee,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let p1 = push_inst(
            &mut callee,
            entry,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let n1 = push_inst(&mut callee, entry, InstKind::Load(p1), IrType::Int(IntWidth::I32));
        let one = push_inst(
            &mut callee,
            entry,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let cond = push_inst(
            &mut callee,
            entry,
            InstKind::ICmp(CmpOp::Le, n1, one),
            IrType::Bool,
        );
        callee.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: if_then,
            true_args: vec![],
            false_dest: if_else,
            false_args: vec![],
        });

        let then_one = push_inst(
            &mut callee,
            if_then,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut callee, if_then, InstKind::Store(then_one, result), IrType::Void);
        callee.block_mut(if_then).terminator = Some(Terminator::Branch(if_end, vec![]));

        let p2 = push_inst(
            &mut callee,
            if_else,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let n2 = push_inst(&mut callee, if_else, InstKind::Load(p2), IrType::Int(IntWidth::I32));
        let p3 = push_inst(
            &mut callee,
            if_else,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let n3 = push_inst(&mut callee, if_else, InstKind::Load(p3), IrType::Int(IntWidth::I32));
        let one2 = push_inst(
            &mut callee,
            if_else,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let dec1 = push_inst(
            &mut callee,
            if_else,
            InstKind::ISub(n3, one2),
            IrType::Int(IntWidth::I32),
        );
        let p4 = push_inst(
            &mut callee,
            if_else,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let n4 = push_inst(&mut callee, if_else, InstKind::Load(p4), IrType::Int(IntWidth::I32));
        let one3 = push_inst(
            &mut callee,
            if_else,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let dec2 = push_inst(
            &mut callee,
            if_else,
            InstKind::ISub(n4, one3),
            IrType::Int(IntWidth::I32),
        );
        let wrap = push_inst(
            &mut callee,
            if_else,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut callee, if_else, InstKind::Store(dec2, wrap), IrType::Void);
        let rec = push_inst(
            &mut callee,
            if_else,
            InstKind::Call(FuncRef::Internal(0), vec![wrap]),
            IrType::Int(IntWidth::I32),
        );
        let prod = push_inst(
            &mut callee,
            if_else,
            InstKind::IMul(n2, rec),
            IrType::Int(IntWidth::I32),
        );
        push_inst(&mut callee, if_else, InstKind::Store(prod, result), IrType::Void);
        callee.block_mut(if_else).terminator = Some(Terminator::Branch(if_end, vec![]));

        let final_val = push_inst(&mut callee, if_end, InstKind::Load(result), IrType::Int(IntWidth::I32));
        callee.block_mut(if_end).terminator = Some(Terminator::Return(Some(final_val)));
        m.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Void);
        let caller_entry = caller.entry;
        let unit = push_inst(
            &mut caller,
            caller_entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let unit2 = push_inst(
            &mut caller,
            caller_entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrap1 = push_inst(
            &mut caller,
            caller_entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, caller_entry, InstKind::Store(unit2, wrap1), IrType::Void);
        let call1 = push_inst(
            &mut caller,
            caller_entry,
            InstKind::Call(FuncRef::Internal(0), vec![wrap1]),
            IrType::Int(IntWidth::I32),
        );
        let wrap2 = push_inst(
            &mut caller,
            caller_entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, caller_entry, InstKind::Store(unit, wrap2), IrType::Void);
        let call2 = push_inst(
            &mut caller,
            caller_entry,
            InstKind::Call(FuncRef::Internal(0), vec![wrap2]),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut caller,
            caller_entry,
            InstKind::IAdd(call1, call2),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(caller_entry).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(caller);

        let pass = Gvn;
        assert!(pass.run(&mut m));
        let caller = &m.functions[1];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .count();
        assert_eq!(call_count, 1, "recursive PURE factorial calls should dedupe");
    }

    #[test]
    fn gvn_reuses_pure_calls_when_callee_follows_caller() {
        let mut m = Module::new("test".into());

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let c1 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrap1 = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, entry, InstKind::Store(c1, wrap1), IrType::Void);
        let call1 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(1), vec![wrap1]),
            IrType::Int(IntWidth::I32),
        );
        let c2 = push_inst(
            &mut caller,
            entry,
            InstKind::ConstInt(6, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let wrap2 = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        push_inst(&mut caller, entry, InstKind::Store(c2, wrap2), IrType::Void);
        let call2 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(1), vec![wrap2]),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(call1, call2),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(caller);

        let param = Param {
            name: "n".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("heavy_fact".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let callee_entry = callee.entry;
        let shadow = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Alloca(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)))),
            IrType::Ptr(Box::new(IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))))),
        );
        push_inst(&mut callee, callee_entry, InstKind::Store(ValueId(0), shadow), IrType::Void);
        let arg_ptr = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(shadow),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let arg_val = push_inst(
            &mut callee,
            callee_entry,
            InstKind::Load(arg_ptr),
            IrType::Int(IntWidth::I32),
        );
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(arg_val)));
        m.add_function(callee);

        let pass = Gvn;
        assert!(pass.run(&mut m));
        let caller = &m.functions[0];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .count();
        assert_eq!(call_count, 1, "caller-before-callee ordering should still dedupe");
    }

    #[test]
    fn gvn_matches_real_pure_recursive_fixture_after_pre_o2_passes() {
        let mut module = lower_fixture("pure_recursive_reuse.f90");
        let pm = build_pre_gvn_o2_pipeline();
        pm.run(&mut module);

        let pure_calls: Vec<PureCallPolicy> = module.functions.iter().map(PureCallPolicy::for_function).collect();
        assert!(
            pure_calls.iter().any(|policy| policy.reusable),
            "at least one function in the fixture should remain a reusable PURE callee:\n{}",
            crate::ir::printer::print_module(&module)
        );

        let caller_idx = module
            .functions
            .iter()
            .position(|func| func.name == "__prog_pure_recursive_reuse")
            .expect("program entry in fixture");
        let wrappers = wrapper_alloca_values(&module.functions[caller_idx], &pure_calls);
        assert!(
            !wrappers.is_empty(),
            "caller should still expose scalar by-ref wrapper allocas before GVN"
        );

        let pass = Gvn;
        assert!(pass.run(&mut module), "GVN should make progress on the real fixture");

        let caller = &module.functions[caller_idx];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(FuncRef::Internal(_), _)))
            .count();
        assert_eq!(call_count, 1, "real fixture caller should end with one PURE recursive call");
    }

    #[test]
    fn gvn_does_not_reuse_pure_calls_with_pointer_args() {
        let mut m = Module::new("test".into());

        let param = Param {
            name: "p".into(),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            id: ValueId(0),
            fortran_noalias: false,
        };
        let mut callee = Function::new("peek".into(), vec![param], IrType::Int(IntWidth::I32));
        callee.is_pure = true;
        let callee_entry = callee.entry;
        let zero = push_inst(
            &mut callee,
            callee_entry,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        callee.block_mut(callee_entry).terminator = Some(Terminator::Return(Some(zero)));
        m.add_function(callee);

        let mut caller = Function::new("main".into(), vec![], IrType::Int(IntWidth::I32));
        let entry = caller.entry;
        let slot = push_inst(
            &mut caller,
            entry,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let call1 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![slot]),
            IrType::Int(IntWidth::I32),
        );
        let call2 = push_inst(
            &mut caller,
            entry,
            InstKind::Call(FuncRef::Internal(0), vec![slot]),
            IrType::Int(IntWidth::I32),
        );
        let sum = push_inst(
            &mut caller,
            entry,
            InstKind::IAdd(call1, call2),
            IrType::Int(IntWidth::I32),
        );
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
        m.add_function(caller);

        let pass = Gvn;
        assert!(!pass.run(&mut m), "pointer-arg PURE calls must stay distinct");

        let caller = &m.functions[1];
        let call_count = caller.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::Call(..)))
            .count();
        assert_eq!(call_count, 2);
    }
}
