//! Type-spec resolution: TypeSpec → TypeInfo, kind/length extraction.
//!
//! Extracted from `core.rs` in Sprint 14. Owns the small bridge from
//! the parser's `TypeSpec` (literal-text-bearing) to the sema's
//! `TypeInfo` (kind-resolved), plus the kind- and char-length-selector
//! evaluators it builds on. Larger derived-type layout work
//! (`compute_all_layouts`, `collect_derived_type_layouts`,
//! `register_local_type_layouts`) remains in `core.rs` for now —
//! follow-up sprints can split those out as they grow.

use crate::ast::decl::{self, TypeSpec};
use crate::sema::symtab::{SymbolTable, TypeInfo};

use super::core::eval_const_int_expr;

pub(super) fn extract_kind(sel: &Option<decl::KindSelector>, st: &SymbolTable) -> Option<u8> {
    use crate::ast::expr::Expr;
    match sel {
        Some(decl::KindSelector::Expr(e)) | Some(decl::KindSelector::Star(e)) => match &e.node {
            Expr::IntegerLiteral { text, .. } => text.parse().ok(),
            Expr::Name { name } => {
                let key = name.to_lowercase();
                st.lookup_in(st.current_scope(), &key)
                    .and_then(|sym| sym.const_value.map(|v| v as u8))
            }
            _ => None,
        },
        None => None,
    }
}

/// Compute the byte length of a string-valued PARAMETER initializer
/// for `character(*)` length inference (F2008 §5.3.2). Handles string
/// literals, references to other character parameters whose length is
/// already known, and `lit // lit` / `lit // name` concat chains.
pub(super) fn derived_char_init_len(e: &crate::ast::expr::Expr, st: &SymbolTable) -> Option<usize> {
    use crate::ast::expr::Expr;
    match e {
        Expr::StringLiteral { value, .. } => Some(value.len()),
        Expr::Name { name } => {
            let sym = st.find_symbol_any_scope(&name.to_lowercase())?;
            if let Some(TypeInfo::Character { len: Some(n), .. }) = &sym.type_info {
                usize::try_from(*n).ok()
            } else {
                None
            }
        }
        Expr::ParenExpr { inner } => derived_char_init_len(&inner.node, st),
        Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Concat,
            left,
            right,
        } => Some(derived_char_init_len(&left.node, st)? + derived_char_init_len(&right.node, st)?),
        _ => None,
    }
}

pub(super) fn extract_char_len(sel: &Option<decl::CharSelector>, st: &SymbolTable) -> Option<i64> {
    match sel {
        Some(cs) => match &cs.len {
            Some(decl::LenSpec::Expr(e)) => eval_const_int_expr(e, st),
            Some(decl::LenSpec::Star) => None,  // assumed length
            Some(decl::LenSpec::Colon) => None, // deferred length
            None => None,
        },
        None => None,
    }
}

pub(super) fn type_spec_to_info(ts: &TypeSpec, st: &SymbolTable) -> TypeInfo {
    match ts {
        TypeSpec::Integer(sel) => TypeInfo::Integer {
            kind: extract_kind(sel, st),
        },
        TypeSpec::Real(sel) => TypeInfo::Real {
            kind: extract_kind(sel, st),
        },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(sel) => TypeInfo::Complex {
            kind: extract_kind(sel, st),
        },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(sel) => TypeInfo::Logical {
            kind: extract_kind(sel, st),
        },
        TypeSpec::Character(sel) => TypeInfo::Character {
            len: extract_char_len(sel, st),
            kind: None,
        },
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
    }
}

pub(super) fn entity_char_len_to_info(
    info: &mut TypeInfo,
    entity_len: Option<&decl::LenSpec>,
    st: &SymbolTable,
) {
    let Some(entity_len) = entity_len else {
        return;
    };
    let TypeInfo::Character { len, .. } = info else {
        return;
    };
    *len = match entity_len {
        decl::LenSpec::Expr(e) => eval_const_int_expr(e, st),
        decl::LenSpec::Star | decl::LenSpec::Colon => None,
    };
}
