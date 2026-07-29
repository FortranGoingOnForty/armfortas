//! Data dependence analysis for loop transformations.
//!
//! Determines whether two memory accesses inside a loop (or loop nest)
//! can touch the same location on different iterations. Uses the GCD
//! test on affine subscript expressions extracted from GEP indices.
//!
//! ## Fortran-specific simplifications
//!
//! - Distinct ordinary array bases are independent only when alias analysis
//!   proves they cannot be POINTER/TARGET aliases.
//! - Column-major strides are compile-time constants for fixed-shape arrays.
//! - INTENT(IN) arguments cannot alias INTENT(OUT) arguments.

use super::alias::{AliasOracle, AliasResult};
use crate::ir::inst::*;
use crate::target::TargetLayout;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// An affine expression: `constant + sum(coefficient * iv)`.
/// The `terms` vector maps induction variables to their coefficients.
#[derive(Debug, Clone)]
pub struct AffineExpr {
    pub constant: i64,
    pub terms: Vec<(i64, ValueId)>, // (coefficient, iv)
}

impl AffineExpr {
    fn zero() -> Self {
        Self {
            constant: 0,
            terms: Vec::new(),
        }
    }

    fn from_const(c: i64) -> Self {
        Self {
            constant: c,
            terms: Vec::new(),
        }
    }

    fn from_iv(iv: ValueId) -> Self {
        Self {
            constant: 0,
            terms: vec![(1, iv)],
        }
    }

    fn add(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.constant += other.constant;
        for &(coeff, iv) in &other.terms {
            if let Some(entry) = result.terms.iter_mut().find(|(_, v)| *v == iv) {
                entry.0 += coeff;
            } else {
                result.terms.push((coeff, iv));
            }
        }
        // Remove zero-coefficient terms.
        result.terms.retain(|(c, _)| *c != 0);
        result
    }

    fn sub(&self, other: &Self) -> Self {
        let negated = AffineExpr {
            constant: -other.constant,
            terms: other.terms.iter().map(|(c, v)| (-c, *v)).collect(),
        };
        self.add(&negated)
    }

    fn scale(&self, factor: i64) -> Self {
        AffineExpr {
            constant: self.constant * factor,
            terms: self.terms.iter().map(|(c, v)| (c * factor, *v)).collect(),
        }
    }
}

/// A memory reference extracted from loop body.
#[derive(Debug, Clone)]
pub struct MemRef {
    pub inst_id: ValueId,
    pub base: ValueId,
    pub subscript: AffineExpr,
    pub is_write: bool,
}

/// Result of dependence testing between two memory references.
#[derive(Debug, Clone)]
pub struct DepResult {
    /// True if the references may access the same element on different iterations.
    pub dependent: bool,
}

// ---------------------------------------------------------------------------
// Affine expression extraction
// ---------------------------------------------------------------------------

/// Extract an affine expression from a GEP index by walking backwards
/// through arithmetic instructions. `ivs` is the set of known induction
/// variables for the enclosing loop nest.
pub fn extract_affine(func: &Function, val: ValueId, ivs: &HashSet<ValueId>) -> Option<AffineExpr> {
    // Is this an IV?
    if ivs.contains(&val) {
        return Some(AffineExpr::from_iv(val));
    }

    // Find the instruction that defines this value.
    let inst = find_inst(func, val)?;
    match &inst.kind {
        InstKind::ConstInt(c, _) => i64::try_from(*c).ok().map(AffineExpr::from_const),

        InstKind::IAdd(a, b) => {
            let ea = extract_affine(func, *a, ivs)?;
            let eb = extract_affine(func, *b, ivs)?;
            Some(ea.add(&eb))
        }

        InstKind::ISub(a, b) => {
            let ea = extract_affine(func, *a, ivs)?;
            let eb = extract_affine(func, *b, ivs)?;
            Some(ea.sub(&eb))
        }

        InstKind::IMul(a, b) => {
            // One operand must be a constant for the result to be affine.
            if let Some(ca) = resolve_const(func, *a) {
                let eb = extract_affine(func, *b, ivs)?;
                Some(eb.scale(ca))
            } else if let Some(cb) = resolve_const(func, *b) {
                let ea = extract_affine(func, *a, ivs)?;
                Some(ea.scale(cb))
            } else {
                None // non-affine (product of two non-constants)
            }
        }

        InstKind::IntExtend(a, _, _) => extract_affine(func, *a, ivs),

        _ => {
            // Value defined outside the loop (e.g., function param, alloca).
            // If it's not an IV and not computable from IVs, treat as unknown.
            // Conservative: return None (non-affine).
            None
        }
    }
}

fn resolve_const(func: &Function, vid: ValueId) -> Option<i64> {
    let inst = find_inst(func, vid)?;
    if let InstKind::ConstInt(c, _) = &inst.kind {
        i64::try_from(*c).ok()
    } else {
        None
    }
}

fn find_inst(func: &Function, vid: ValueId) -> Option<&Inst> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == vid {
                return Some(inst);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Memory reference collection
// ---------------------------------------------------------------------------

/// Collect all memory references (loads and stores through GEPs) in a
/// set of blocks.
pub fn collect_mem_refs(
    func: &Function,
    blocks: &HashSet<BlockId>,
    ivs: &HashSet<ValueId>,
) -> Vec<MemRef> {
    let mut refs = Vec::new();
    for &bid in blocks {
        let block = func.block(bid);
        for inst in &block.insts {
            match &inst.kind {
                InstKind::Store(_, ptr) | InstKind::VolatileStore(_, ptr) => {
                    if let Some(mr) = extract_mem_ref(func, inst.id, *ptr, true, ivs) {
                        refs.push(mr);
                    }
                }
                InstKind::Load(ptr) | InstKind::VolatileLoad(ptr) => {
                    if let Some(mr) = extract_mem_ref(func, inst.id, *ptr, false, ivs) {
                        refs.push(mr);
                    }
                }
                _ => {}
            }
        }
    }
    refs
}

fn extract_mem_ref(
    func: &Function,
    inst_id: ValueId,
    ptr: ValueId,
    is_write: bool,
    ivs: &HashSet<ValueId>,
) -> Option<MemRef> {
    // The pointer should be a GEP.
    let gep_inst = find_inst(func, ptr)?;
    let (base, indices) = match &gep_inst.kind {
        InstKind::GetElementPtr(b, idxs) => (*b, idxs.clone()),
        _ => return None,
    };
    // We work with the flat offset (single index for 1D GEP).
    let idx = indices.first()?;
    let subscript = extract_affine(func, *idx, ivs)?;
    Some(MemRef {
        inst_id,
        base,
        subscript,
        is_write,
    })
}

// ---------------------------------------------------------------------------
// GCD test
// ---------------------------------------------------------------------------

/// Test whether two memory references can access the same element on
/// different iterations.
///
/// GCD test: given `f(I) = sum(a_k * i_k) + c1` and
/// `g(I) = sum(b_k * i_k) + c2`, a dependence is possible only if
/// `gcd(a_1, ..., b_1, ...) | (c2 - c1)`.
pub fn test_dependence(ref_a: &MemRef, ref_b: &MemRef) -> DepResult {
    // Fortran no-alias: distinct array bases → independent.
    if ref_a.base != ref_b.base {
        return DepResult { dependent: false };
    }

    // Compute the difference of the two affine expressions.
    let diff = ref_b.subscript.sub(&ref_a.subscript);

    // If there are no IV terms, the accesses are at a fixed distance.
    // If the constant is 0, they access the same element (same iteration = ok
    // unless both are writes). If non-zero, they access different elements.
    if diff.terms.is_empty() {
        return DepResult {
            dependent: diff.constant == 0,
        };
    }

    // GCD of all IV coefficients in the difference.
    let g = diff
        .terms
        .iter()
        .map(|(c, _)| c.unsigned_abs())
        .fold(0u64, gcd);

    if g == 0 {
        // All coefficients are zero — same as fixed-distance case.
        return DepResult {
            dependent: diff.constant == 0,
        };
    }

    // GCD test: if gcd does not divide the constant difference,
    // the accesses NEVER touch the same element → independent.
    let dependent = diff.constant.unsigned_abs().is_multiple_of(g);
    DepResult { dependent }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

// ---------------------------------------------------------------------------
// High-level queries for loop passes
// ---------------------------------------------------------------------------

/// Check if two adjacent loop bodies can be legally fused.
///
/// Fusion is safe when no cross-loop dependence would be reversed.
/// Same-iteration dependencies (subscript diff = 0) are ALWAYS
/// fusion-legal: the write in A's body precedes the read in B's
/// body within the same fused iteration. Only cross-iteration
/// backward dependencies (B writes, A reads at a later iteration)
/// would be reversed by fusion — and those can't exist between
/// adjacent loops that don't share a carried state.
/// Check if two adjacent loop bodies can be legally fused.
///
/// `iv_a` and `iv_b` are the IVs of the two loops — they iterate
/// over the same range, so for dependence purposes they are the
/// same variable. We remap `iv_b → iv_a` in B's subscripts before
/// comparing.
pub fn fusion_legal(
    func: &Function,
    body_a: &HashSet<BlockId>,
    body_b: &HashSet<BlockId>,
    iv_a: ValueId,
    iv_b: ValueId,
    layout: TargetLayout,
) -> bool {
    let mut ivs_a = HashSet::new();
    ivs_a.insert(iv_a);
    let mut ivs_b = HashSet::new();
    ivs_b.insert(iv_b);

    let refs_a = collect_mem_refs(func, body_a, &ivs_a);
    let refs_b_raw = collect_mem_refs(func, body_b, &ivs_b);

    // Remap iv_b → iv_a in B's subscripts so the comparison works.
    let refs_b: Vec<MemRef> = refs_b_raw
        .into_iter()
        .map(|mut r| {
            for term in &mut r.subscript.terms {
                if term.1 == iv_b {
                    term.1 = iv_a;
                }
            }
            r
        })
        .collect();
    let mut alias_oracle = AliasOracle::new(func, layout);

    for ra in &refs_a {
        for rb in &refs_b {
            if !ra.is_write && !rb.is_write {
                continue;
            }
            if matches!(alias_oracle.query(ra.base, rb.base), AliasResult::NoAlias) {
                continue;
            }

            let diff = rb.subscript.sub(&ra.subscript);

            // Same-iteration access (diff = 0) → fusion-legal.
            if diff.terms.is_empty() && diff.constant == 0 {
                continue;
            }

            // Non-zero distance → conservative reject.
            return false;
        }
    }
    true
}

/// Check if interchanging (outer, inner) preserves correctness.
/// Conservative: any carried dependence within the inner body that
/// involves both IVs may change direction after interchange.
pub fn interchange_legal(
    func: &Function,
    inner_body: &HashSet<BlockId>,
    outer_iv: ValueId,
    inner_iv: ValueId,
) -> bool {
    let mut ivs = HashSet::new();
    ivs.insert(outer_iv);
    ivs.insert(inner_iv);

    let refs = collect_mem_refs(func, inner_body, &ivs);

    // For each pair of refs where at least one is a write:
    for i in 0..refs.len() {
        for j in (i + 1)..refs.len() {
            if !refs[i].is_write && !refs[j].is_write {
                continue;
            }
            if refs[i].base != refs[j].base {
                continue;
            }

            // Both refs share a base and at least one is a write.
            // Check if the subscript difference has non-zero
            // coefficients for BOTH IVs — if so, interchange might
            // reverse the dependence direction.
            let diff = refs[j].subscript.sub(&refs[i].subscript);
            let has_outer = diff.terms.iter().any(|(c, v)| *v == outer_iv && *c != 0);
            let has_inner = diff.terms.iter().any(|(c, v)| *v == inner_iv && *c != 0);
            if has_outer && has_inner {
                // Dependence involves both IVs — interchange could reverse
                // the direction. Conservative: reject.
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
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
    fn gcd_basic() {
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(7, 3), 1);
        assert_eq!(gcd(0, 5), 5);
        assert_eq!(gcd(10, 0), 10);
    }

    #[test]
    fn affine_add_sub() {
        let iv = ValueId(100);
        let a = AffineExpr {
            constant: 3,
            terms: vec![(2, iv)],
        };
        let b = AffineExpr {
            constant: 1,
            terms: vec![(1, iv)],
        };
        let sum = a.add(&b);
        assert_eq!(sum.constant, 4);
        assert_eq!(sum.terms.len(), 1);
        assert_eq!(sum.terms[0], (3, iv));

        let diff = a.sub(&b);
        assert_eq!(diff.constant, 2);
        assert_eq!(diff.terms.len(), 1);
        assert_eq!(diff.terms[0], (1, iv));
    }

    #[test]
    fn affine_scale() {
        let iv = ValueId(100);
        let a = AffineExpr {
            constant: 2,
            terms: vec![(3, iv)],
        };
        let scaled = a.scale(4);
        assert_eq!(scaled.constant, 8);
        assert_eq!(scaled.terms[0], (12, iv));
    }

    #[test]
    fn gcd_test_independent() {
        // a(2i+1) vs a(2i+2): diff = 2i+2 - (2i+1) = 1 (constant).
        // No IV terms in diff → dependent only if constant is 0.
        // constant = 1 ≠ 0 → independent.
        let iv = ValueId(100);
        let ref_a = MemRef {
            inst_id: ValueId(0),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 1,
                terms: vec![(2, iv)],
            },
            is_write: true,
        };
        let ref_b = MemRef {
            inst_id: ValueId(1),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 2,
                terms: vec![(2, iv)],
            },
            is_write: false,
        };
        let dep = test_dependence(&ref_a, &ref_b);
        assert!(!dep.dependent, "a(2i+1) and a(2i+2) should be independent");
    }

    #[test]
    fn gcd_test_dependent() {
        // a(i) vs a(i): same subscript → always dependent.
        let iv = ValueId(100);
        let ref_a = MemRef {
            inst_id: ValueId(0),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 0,
                terms: vec![(1, iv)],
            },
            is_write: true,
        };
        let ref_b = MemRef {
            inst_id: ValueId(1),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 0,
                terms: vec![(1, iv)],
            },
            is_write: false,
        };
        let dep = test_dependence(&ref_a, &ref_b);
        assert!(dep.dependent, "a(i) and a(i) should be dependent");
    }

    #[test]
    fn gcd_test_different_stride() {
        // a(3i) vs a(3i+1): diff has constant 1, gcd of coefficients = 0
        // (they cancel: 3-3=0). So diff = constant 1, no terms → independent.
        let iv = ValueId(100);
        let ref_a = MemRef {
            inst_id: ValueId(0),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 0,
                terms: vec![(3, iv)],
            },
            is_write: true,
        };
        let ref_b = MemRef {
            inst_id: ValueId(1),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 1,
                terms: vec![(3, iv)],
            },
            is_write: false,
        };
        let dep = test_dependence(&ref_a, &ref_b);
        assert!(
            !dep.dependent,
            "a(3i) and a(3i+1) should be independent by GCD test"
        );
    }

    #[test]
    fn distinct_bases_independent() {
        let iv = ValueId(100);
        let ref_a = MemRef {
            inst_id: ValueId(0),
            base: ValueId(50),
            subscript: AffineExpr {
                constant: 0,
                terms: vec![(1, iv)],
            },
            is_write: true,
        };
        let ref_b = MemRef {
            inst_id: ValueId(1),
            base: ValueId(60), // different base
            subscript: AffineExpr {
                constant: 0,
                terms: vec![(1, iv)],
            },
            is_write: false,
        };
        let dep = test_dependence(&ref_a, &ref_b);
        assert!(
            !dep.dependent,
            "distinct bases should be independent (Fortran no-alias)"
        );
    }

    #[test]
    fn fusion_rejects_cross_iteration_pointer_alias() {
        let ptr_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)));
        let params = vec![
            Param {
                name: "first".into(),
                ty: ptr_ty.clone(),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "second".into(),
                ty: ptr_ty.clone(),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut func = Function::new("pointer_loops".into(), params, IrType::Void);
        let entry = func.entry;
        let body_a = func.create_block("body_a");
        let body_b = func.create_block("body_b");

        let iv_a = func.next_value_id();
        func.register_type(iv_a, IrType::Int(IntWidth::I64));
        func.block_mut(body_a).params.push(BlockParam {
            id: iv_a,
            ty: IrType::Int(IntWidth::I64),
        });
        let iv_b = func.next_value_id();
        func.register_type(iv_b, IrType::Int(IntWidth::I64));
        func.block_mut(body_b).params.push(BlockParam {
            id: iv_b,
            ty: IrType::Int(IntWidth::I64),
        });

        let one = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(1, IntWidth::I64),
            IrType::Int(IntWidth::I64),
        );
        let value = push_inst(
            &mut func,
            entry,
            InstKind::ConstInt(7, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let first_element = push_inst(
            &mut func,
            body_a,
            InstKind::GetElementPtr(ValueId(0), vec![iv_a]),
            ptr_ty.clone(),
        );
        push_inst(
            &mut func,
            body_a,
            InstKind::Store(value, first_element),
            IrType::Void,
        );

        let next = push_inst(
            &mut func,
            body_b,
            InstKind::IAdd(iv_b, one),
            IrType::Int(IntWidth::I64),
        );
        let second_element = push_inst(
            &mut func,
            body_b,
            InstKind::GetElementPtr(ValueId(1), vec![next]),
            ptr_ty,
        );
        push_inst(
            &mut func,
            body_b,
            InstKind::Load(second_element),
            IrType::Int(IntWidth::I32),
        );

        assert!(
            !fusion_legal(
                &func,
                &HashSet::from([body_a]),
                &HashSet::from([body_b]),
                iv_a,
                iv_b,
                TargetLayout::LP64,
            ),
            "distinct pointer descriptors may alias the same target"
        );

        for param in &mut func.params {
            param.fortran_noalias = true;
        }
        assert!(
            fusion_legal(
                &func,
                &HashSet::from([body_a]),
                &HashSet::from([body_b]),
                iv_a,
                iv_b,
                TargetLayout::LP64,
            ),
            "distinct ordinary array arguments should remain independent"
        );
    }
}
