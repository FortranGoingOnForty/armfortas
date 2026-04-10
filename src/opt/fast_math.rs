//! Ofast-only fast-math reassociation.
//!
//! Reassociates floating add/sub chains that consist of one non-constant
//! base value plus finite constant terms:
//!
//! - `(x + c1) + c2` -> `x + (c1 + c2)`
//! - `(x + c1) - c2` -> `x + (c1 - c2)`
//! - `(x - c1) + c2` -> `x + (c2 - c1)`
//! - `(x - c1) - c2` -> `x + (-(c1 + c2))`
//!
//! Under strict IEEE semantics this can change results because it changes
//! rounding and signed-zero behavior. We therefore gate it at `-Ofast`
//! only, where fast-math relaxation is explicitly enabled.

use std::collections::{HashMap, HashSet};

use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IrType};
use crate::lexer::Span;

use super::pass::Pass;
use super::util::substitute_uses;

pub struct FastMathReassoc;

impl Pass for FastMathReassoc {
    fn name(&self) -> &'static str { "fast-math-reassoc" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if rewrite_function(func) {
                changed = true;
            }
        }
        changed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FloatConst {
    width: FloatWidth,
    bits: u64,
}

impl FloatConst {
    fn from_value(value: f64, width: FloatWidth) -> Option<Self> {
        let rounded = round_for_width(value, width);
        if !rounded.is_finite() {
            return None;
        }
        let bits = match width {
            FloatWidth::F32 => (rounded as f32).to_bits() as u64,
            FloatWidth::F64 => rounded.to_bits(),
        };
        Some(Self { width, bits })
    }

    fn as_f64(self) -> f64 {
        match self.width {
            FloatWidth::F32 => f32::from_bits(self.bits as u32) as f64,
            FloatWidth::F64 => f64::from_bits(self.bits),
        }
    }

    fn kind(self) -> InstKind {
        InstKind::ConstFloat(self.as_f64(), self.width)
    }
}

#[derive(Debug, Clone)]
struct AddChain {
    base: Option<ValueId>,
    terms: Vec<SignedFloatConst>,
    additive_nodes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SignedFloatConst {
    constant: FloatConst,
    negative: bool,
}

impl SignedFloatConst {
    fn positive(constant: FloatConst) -> Self {
        Self { constant, negative: false }
    }

    fn negated(self) -> Self {
        Self {
            constant: self.constant,
            negative: !self.negative,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RewritePlan {
    inst_id: ValueId,
    block_id: BlockId,
    base: ValueId,
    constant: FloatConst,
    span: Span,
}

fn rewrite_function(func: &mut Function) -> bool {
    let defs = inst_map(func);
    let mut plans = Vec::new();
    let mut rewrites = HashMap::new();
    let mut remove_ids = HashSet::new();

    for block in &func.blocks {
        for inst in &block.insts {
            let width = match inst.ty {
                IrType::Float(width) => width,
                _ => continue,
            };
            if !matches!(inst.kind, InstKind::FAdd(..) | InstKind::FSub(..)) {
                continue;
            }

            let Some(chain) = collect_chain(&defs, inst.id, width) else {
                continue;
            };
            if chain.additive_nodes <= 1 {
                continue;
            }
            let Some(base) = chain.base else {
                continue;
            };

            let rounded = combine_terms(&chain.terms, width);
            if is_effective_zero(rounded) {
                rewrites.insert(inst.id, base);
                remove_ids.insert(inst.id);
                continue;
            }

            let Some(constant) = FloatConst::from_value(rounded, width) else {
                continue;
            };

            let already_canonical = match inst.kind {
                InstKind::FAdd(lhs, rhs) => {
                    lhs == base && const_float_of(&defs, rhs, width) == Some(constant)
                }
                _ => false,
            };
            if already_canonical {
                continue;
            }

            plans.push(RewritePlan {
                inst_id: inst.id,
                block_id: block.id,
                base,
                constant,
                span: inst.span,
            });
        }
    }

    if plans.is_empty() && rewrites.is_empty() {
        return false;
    }

    let mut const_cache = HashMap::new();
    for plan in plans {
        let const_id = ensure_const_in_entry(func, &mut const_cache, plan.constant, plan.span);
        let block = func.block_mut(plan.block_id);
        let Some(inst) = block.insts.iter_mut().find(|inst| inst.id == plan.inst_id) else {
            continue;
        };
        inst.kind = InstKind::FAdd(plan.base, const_id);
    }

    if !rewrites.is_empty() {
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
            substitute_uses(func, *old, *new);
        }
        for block in &mut func.blocks {
            block.insts.retain(|inst| !remove_ids.contains(&inst.id));
        }
    }

    true
}

fn inst_map(func: &Function) -> HashMap<ValueId, &Inst> {
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .map(|inst| (inst.id, inst))
        .collect()
}

fn collect_chain(
    defs: &HashMap<ValueId, &Inst>,
    value: ValueId,
    width: FloatWidth,
) -> Option<AddChain> {
    if let Some(constant) = const_float_of(defs, value, width) {
        return Some(AddChain {
            base: None,
            terms: vec![SignedFloatConst::positive(constant)],
            additive_nodes: 0,
        });
    }

        let Some(inst) = defs.get(&value) else {
            return Some(AddChain {
                base: Some(value),
                terms: Vec::new(),
                additive_nodes: 0,
            });
        };
    match inst.kind {
        InstKind::FAdd(lhs, rhs) if inst.ty == IrType::Float(width) => {
            let lhs_chain = collect_chain(defs, lhs, width)?;
            let rhs_chain = collect_chain(defs, rhs, width)?;
            let base = merge_bases(lhs_chain.base, rhs_chain.base)?;
            let mut terms = lhs_chain.terms;
            terms.extend(rhs_chain.terms);
            Some(AddChain {
                base,
                terms,
                additive_nodes: lhs_chain.additive_nodes + rhs_chain.additive_nodes + 1,
            })
        }
        InstKind::FSub(lhs, rhs) if inst.ty == IrType::Float(width) => {
            let lhs_chain = collect_chain(defs, lhs, width)?;
            let rhs_chain = collect_chain(defs, rhs, width)?;
            if rhs_chain.base.is_some() {
                return None;
            }
            let mut terms = lhs_chain.terms;
            terms.extend(rhs_chain.terms.into_iter().map(SignedFloatConst::negated));
            Some(AddChain {
                base: lhs_chain.base,
                terms,
                additive_nodes: lhs_chain.additive_nodes + rhs_chain.additive_nodes + 1,
            })
        }
        _ => Some(AddChain {
            base: Some(value),
            terms: Vec::new(),
            additive_nodes: 0,
        }),
    }
}

fn merge_bases(lhs: Option<ValueId>, rhs: Option<ValueId>) -> Option<Option<ValueId>> {
    match (lhs, rhs) {
        (Some(_), Some(_)) => None,
        (Some(base), None) | (None, Some(base)) => Some(Some(base)),
        (None, None) => Some(None),
    }
}

fn const_float_of(
    defs: &HashMap<ValueId, &Inst>,
    value: ValueId,
    width: FloatWidth,
) -> Option<FloatConst> {
    let inst = defs.get(&value)?;
    match inst.kind {
        InstKind::ConstFloat(value, inst_width) if inst_width == width => FloatConst::from_value(value, width),
        _ => None,
    }
}

fn combine_terms(terms: &[SignedFloatConst], width: FloatWidth) -> f64 {
    let mut counts: HashMap<FloatConst, i32> = HashMap::new();
    for term in terms {
        *counts.entry(term.constant).or_insert(0) += if term.negative { -1 } else { 1 };
    }

    let mut surviving: Vec<(FloatConst, i32)> = counts
        .into_iter()
        .filter(|(_, count)| *count != 0)
        .collect();
    surviving.sort_by(|(lhs_const, lhs_count), (rhs_const, rhs_count)| {
        lhs_const
            .as_f64()
            .abs()
            .total_cmp(&rhs_const.as_f64().abs())
            .then_with(|| lhs_const.as_f64().total_cmp(&rhs_const.as_f64()))
            .then_with(|| lhs_count.cmp(rhs_count))
    });

    let mut sum = 0.0;
    for (constant, count) in surviving {
        for _ in 0..count.unsigned_abs() {
            if count > 0 {
                sum += constant.as_f64();
            } else {
                sum -= constant.as_f64();
            }
        }
    }
    round_for_width(sum, width)
}

fn ensure_const_in_entry(
    func: &mut Function,
    cache: &mut HashMap<FloatConst, ValueId>,
    value: FloatConst,
    span: Span,
) -> ValueId {
    if let Some(&id) = cache.get(&value) {
        return id;
    }

    let id = func.next_value_id();
    let ty = IrType::Float(value.width);
    func.register_type(id, ty.clone());
    let inst = Inst {
        id,
        kind: value.kind(),
        ty,
        span,
    };
    let entry = func.entry;
    func.block_mut(entry).insts.insert(0, inst);
    cache.insert(value, id);
    id
}

fn round_for_width(value: f64, width: FloatWidth) -> f64 {
    match width {
        FloatWidth::F32 => value as f32 as f64,
        FloatWidth::F64 => value,
    }
}

fn is_effective_zero(value: f64) -> bool {
    value == 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::lexer::Position;

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
    fn collapses_nested_float_constant_chain() {
        let mut module = Module::new("t".into());
        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Float(FloatWidth::F64),
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut func = Function::new("f".into(), params, IrType::Float(FloatWidth::F64));
        let c1 = push(
            &mut func,
            InstKind::ConstFloat(2.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let c2 = push(
            &mut func,
            InstKind::ConstFloat(3.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let add1 = push(
            &mut func,
            InstKind::FAdd(ValueId(0), c1),
            IrType::Float(FloatWidth::F64),
        );
        let add2 = push(
            &mut func,
            InstKind::FAdd(add1, c2),
            IrType::Float(FloatWidth::F64),
        );
        let entry = func.entry;
        func.block_mut(entry).terminator = Some(Terminator::Return(Some(add2)));
        module.add_function(func);

        assert!(FastMathReassoc.run(&mut module));
        let func = &module.functions[0];
        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(inst.kind, InstKind::ConstFloat(v, FloatWidth::F64) if v == 5.0)),
            "reassociated chain should materialize a combined constant:\n{:?}",
            insts
        );
        assert!(
            matches!(func.blocks[0].terminator, Some(Terminator::Return(Some(v))) if v == add2),
            "outer value should stay the return root"
        );
        let outer = insts.iter().find(|inst| inst.id == add2).expect("outer add should remain");
        assert!(
            matches!(outer.kind, InstKind::FAdd(ValueId(0), _)),
            "outer add should become x + const, got {:?}",
            outer.kind
        );
    }

    #[test]
    fn cancels_rounding_sensitive_add_sub_chain_to_base() {
        let mut module = Module::new("t".into());
        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Float(FloatWidth::F64),
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut func = Function::new("f".into(), params, IrType::Float(FloatWidth::F64));
        let big = push(
            &mut func,
            InstKind::ConstFloat(1.0e16, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let add = push(
            &mut func,
            InstKind::FAdd(ValueId(0), big),
            IrType::Float(FloatWidth::F64),
        );
        let sub = push(
            &mut func,
            InstKind::FSub(add, big),
            IrType::Float(FloatWidth::F64),
        );
        let entry = func.entry;
        func.block_mut(entry).terminator = Some(Terminator::Return(Some(sub)));
        module.add_function(func);

        assert!(FastMathReassoc.run(&mut module));
        let func = &module.functions[0];
        assert!(
            matches!(func.blocks[0].terminator, Some(Terminator::Return(Some(v))) if v == ValueId(0)),
            "cancelled chain should return the original base directly"
        );
        assert!(
            !func.blocks[0].insts.iter().any(|inst| inst.id == sub),
            "cancelled outer subtraction should be removed"
        );
    }

    #[test]
    fn preserves_small_constant_when_large_terms_cancel_under_ofast() {
        let mut module = Module::new("t".into());
        let params = vec![Param {
            name: "x".into(),
            ty: IrType::Float(FloatWidth::F64),
            id: ValueId(0),
            fortran_noalias: false,
        }];
        let mut func = Function::new("f".into(), params, IrType::Float(FloatWidth::F64));
        let one = push(
            &mut func,
            InstKind::ConstFloat(1.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let big = push(
            &mut func,
            InstKind::ConstFloat(1.0e16, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let add_small = push(
            &mut func,
            InstKind::FAdd(ValueId(0), one),
            IrType::Float(FloatWidth::F64),
        );
        let add_big = push(
            &mut func,
            InstKind::FAdd(add_small, big),
            IrType::Float(FloatWidth::F64),
        );
        let sub_big = push(
            &mut func,
            InstKind::FSub(add_big, big),
            IrType::Float(FloatWidth::F64),
        );
        let entry = func.entry;
        func.block_mut(entry).terminator = Some(Terminator::Return(Some(sub_big)));
        module.add_function(func);

        assert!(FastMathReassoc.run(&mut module));
        let func = &module.functions[0];
        let outer = func.blocks[0]
            .insts
            .iter()
            .find(|inst| inst.id == sub_big)
            .expect("outer value should remain after reassociation");
        let const_id = match outer.kind {
            InstKind::FAdd(ValueId(0), const_id) => const_id,
            ref other => panic!("expected x + const after reassociation, got {:?}", other),
        };
        let const_inst = func.blocks[0]
            .insts
            .iter()
            .find(|inst| inst.id == const_id)
            .expect("combined constant should be materialized");
        assert!(
            matches!(const_inst.kind, InstKind::ConstFloat(v, FloatWidth::F64) if v == 1.0),
            "large cancelling terms should preserve the remaining +1 constant, got {:?}",
            const_inst.kind
        );
    }
}
