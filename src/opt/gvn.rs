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
        let mut changed = false;
        for func in &mut module.functions {
            if gvn_function(func) { changed = true; }
        }
        changed
    }
}

/// Canonical key for value numbering (same as CSE).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    tag: u32,
    operands: Vec<ValueId>,
    aux: i64,
    name: Option<String>,
    ty: IrType,
}

fn key_of(inst: &Inst) -> Option<Key> {
    let mk = |tag: u32, ops: Vec<ValueId>, aux: i64| -> Option<Key> {
        Some(Key { tag, operands: ops, aux, name: None, ty: inst.ty.clone() })
    };
    let mk_named = |tag: u32, name: String| -> Option<Key> {
        Some(Key { tag, operands: vec![], aux: 0, name: Some(name), ty: inst.ty.clone() })
    };
    fn canon(a: ValueId, b: ValueId) -> Vec<ValueId> {
        if a.0 <= b.0 { vec![a, b] } else { vec![b, a] }
    }

    match &inst.kind {
        // Pure arithmetic — commutative ops get canonicalized operand order.
        InstKind::IAdd(a, b)  => mk(1, canon(*a, *b), 0),
        InstKind::ISub(a, b)  => mk(2, vec![*a, *b], 0),
        InstKind::IMul(a, b)  => mk(3, canon(*a, *b), 0),
        InstKind::IDiv(a, b)  => mk(4, vec![*a, *b], 0),
        InstKind::IMod(a, b)  => mk(5, vec![*a, *b], 0),
        InstKind::INeg(a)     => mk(6, vec![*a], 0),
        InstKind::FAdd(a, b)  => mk(10, canon(*a, *b), 0),
        InstKind::FSub(a, b)  => mk(11, vec![*a, *b], 0),
        InstKind::FMul(a, b)  => mk(12, canon(*a, *b), 0),
        InstKind::FDiv(a, b)  => mk(13, vec![*a, *b], 0),
        InstKind::FNeg(a)     => mk(14, vec![*a], 0),
        InstKind::FAbs(a)     => mk(15, vec![*a], 0),
        InstKind::FSqrt(a)    => mk(16, vec![*a], 0),
        InstKind::FPow(a, b)  => mk(17, vec![*a, *b], 0),
        InstKind::ICmp(op, a, b) => {
            let op_val = *op as i64;
            match op {
                CmpOp::Eq | CmpOp::Ne => mk(20, canon(*a, *b), op_val),
                _ => mk(20, vec![*a, *b], op_val),
            }
        }
        InstKind::FCmp(op, a, b) => mk(21, vec![*a, *b], *op as i64),
        InstKind::And(a, b) => mk(30, canon(*a, *b), 0),
        InstKind::Or(a, b)  => mk(31, canon(*a, *b), 0),
        InstKind::Not(a)    => mk(32, vec![*a], 0),
        InstKind::Select(c, t, f) => mk(33, vec![*c, *t, *f], 0),
        InstKind::BitAnd(a, b)  => mk(40, canon(*a, *b), 0),
        InstKind::BitOr(a, b)   => mk(41, canon(*a, *b), 0),
        InstKind::BitXor(a, b)  => mk(42, canon(*a, *b), 0),
        InstKind::BitNot(a)     => mk(43, vec![*a], 0),
        InstKind::Shl(a, b)     => mk(44, vec![*a, *b], 0),
        InstKind::LShr(a, b)    => mk(45, vec![*a, *b], 0),
        InstKind::AShr(a, b)    => mk(46, vec![*a, *b], 0),
        InstKind::CountLeadingZeros(a)  => mk(47, vec![*a], 0),
        InstKind::CountTrailingZeros(a) => mk(48, vec![*a], 0),
        InstKind::PopCount(a)           => mk(49, vec![*a], 0),
        // Conversions.
        InstKind::IntToFloat(a, w)   => mk(50, vec![*a], w.bits() as i64),
        InstKind::FloatToInt(a, w)   => mk(51, vec![*a], w.bits() as i64),
        InstKind::FloatExtend(a, w)  => mk(52, vec![*a], w.bits() as i64),
        InstKind::FloatTrunc(a, w)   => mk(53, vec![*a], w.bits() as i64),
        InstKind::IntExtend(a, w, s) => mk(54, vec![*a], w.bits() as i64 * if *s { 1 } else { -1 }),
        InstKind::IntTrunc(a, w)     => mk(55, vec![*a], w.bits() as i64),
        // Constants.
        InstKind::ConstInt(v, w)   => mk(60, vec![], *v * 100 + w.bits() as i64),
        InstKind::ConstFloat(v, w) => mk(61, vec![], ((*v).to_bits() as i64) ^ (w.bits() as i64)),
        InstKind::ConstBool(v)     => mk(62, vec![], *v as i64),
        // GlobalAddr.
        InstKind::GlobalAddr(name) => mk_named(70, name.clone()),
        // GEP.
        InstKind::GetElementPtr(base, idxs) => {
            let mut ops = vec![*base];
            ops.extend(idxs);
            mk(80, ops, 0)
        }
        // Impure: loads, stores, calls, alloca — not GVN candidates.
        InstKind::Load(..) | InstKind::Store(..) | InstKind::Alloca(..)
        | InstKind::Call(..) | InstKind::RuntimeCall(..)
        | InstKind::ConstString(..) | InstKind::Undef(..)
        | InstKind::ExtractField(..) | InstKind::InsertField(..) => None,
    }
}

fn gvn_function(func: &mut Function) -> bool {
    let idoms = compute_immediate_dominators(func);
    let children = dominator_tree_children(&idoms);

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
            // Remap operands through existing replacements.
            // (We'll apply replacements in a second pass.)

            if let Some(key) = key_of(inst) {
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

    // Apply replacements: substitute all uses of redundant values.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.kind = remap_operands(&inst.kind, &replacements);
        }
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
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;

    #[test]
    fn gvn_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Gvn;
        assert!(!pass.run(&mut m));
    }
}
