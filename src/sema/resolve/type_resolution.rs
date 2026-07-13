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

use super::core::{eval_const_int_expr, eval_const_int_expr_in_scope};

/// Integer kind backing an IEEE intrinsic-module opaque type (l09),
/// or `None` if `name` is not one. The class/round/flag tag types are
/// small ordinals (default integer); `ieee_status_type` is a 16-byte
/// save buffer for the FP control+status words, modeled as integer(16).
/// Case-insensitive; shared by declaration resolution, derived-type
/// layout, and IR lowering so every path agrees on storage size.
pub(crate) fn ieee_opaque_int_kind(name: &str) -> Option<u8> {
    match name.to_lowercase().as_str() {
        "ieee_class_type" | "ieee_round_type" | "ieee_flag_type" => Some(4),
        "ieee_status_type" => Some(16),
        _ => None,
    }
}

/// True if `name` is one of the IEEE opaque types modeled as an integer.
pub(crate) fn is_ieee_opaque_type(name: &str) -> bool {
    ieee_opaque_int_kind(name).is_some()
}

fn extract_kind_in_scope(
    sel: &Option<decl::KindSelector>,
    st: &SymbolTable,
    scope_id: crate::sema::symtab::ScopeId,
) -> Option<u8> {
    match sel {
        Some(decl::KindSelector::Expr(expr)) | Some(decl::KindSelector::Star(expr)) => {
            eval_const_int_expr_in_scope(expr, st, scope_id)
                .and_then(|kind| u8::try_from(kind).ok())
        }
        None => None,
    }
}

/// Compute the byte length of a string-valued PARAMETER initializer
/// for `character(*)` length inference (F2008 §5.3.2). Handles string
/// literals, references to other character parameters whose length is
/// already known, typed character array constructors, and `lit // lit` /
/// `lit // name` concat chains.
pub(super) fn derived_char_init_len(e: &crate::ast::expr::Expr, st: &SymbolTable) -> Option<usize> {
    use crate::ast::expr::{AcValue, Expr};
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
        Expr::FunctionCall { callee, args } => {
            let Expr::Name { name } = &callee.node else {
                return None;
            };
            match name.to_ascii_lowercase().as_str() {
                "char" | "achar" if !args.is_empty() => Some(1),
                "new_line" if args.len() == 1 => Some(1),
                "repeat" if args.len() == 2 => {
                    let crate::ast::expr::SectionSubscript::Element(source) = &args[0].value else {
                        return None;
                    };
                    let crate::ast::expr::SectionSubscript::Element(count) = &args[1].value else {
                        return None;
                    };
                    let source_len = derived_char_init_len(&source.node, st)?;
                    let count = eval_const_int_expr(count, st)?;
                    if count < 0 {
                        return None;
                    }
                    usize::try_from(count)
                        .ok()
                        .and_then(|n| source_len.checked_mul(n))
                }
                _ => None,
            }
        }
        Expr::ArrayConstructor { type_spec, values } => {
            if let Some(type_spec) = type_spec {
                if let Some(len) = typed_character_array_constructor_len(type_spec, st) {
                    return Some(len);
                }
            }

            let mut max_len = None;
            for value in values {
                let AcValue::Expr(expr) = value else {
                    return None;
                };
                let len = derived_char_init_len(&expr.node, st)?;
                max_len = Some(max_len.map_or(len, |prev: usize| prev.max(len)));
            }
            max_len
        }
        Expr::BinaryOp {
            op: crate::ast::expr::BinaryOp::Concat,
            left,
            right,
        } => Some(derived_char_init_len(&left.node, st)? + derived_char_init_len(&right.node, st)?),
        _ => None,
    }
}

fn typed_character_array_constructor_len(type_spec: &str, st: &SymbolTable) -> Option<usize> {
    let tokens = crate::lexer::tokenize(type_spec, 0, crate::lexer::SourceForm::FreeForm).ok()?;
    let mut parser = crate::parser::Parser::new(&tokens);
    let parsed = parser.try_parse_type_spec()?.ok()?;
    if parser.peek() != &crate::lexer::TokenKind::Eof {
        return None;
    }

    let TypeSpec::Character(Some(selector)) = parsed else {
        return None;
    };
    let decl::LenSpec::Expr(expr) = selector.len? else {
        return None;
    };
    let len = eval_const_int_expr(&expr, st)?;
    if len < 0 {
        return None;
    }
    usize::try_from(len).ok()
}

fn extract_char_len_in_scope(
    sel: &Option<decl::CharSelector>,
    st: &SymbolTable,
    scope_id: crate::sema::symtab::ScopeId,
) -> Option<i64> {
    match sel {
        Some(cs) => match &cs.len {
            Some(decl::LenSpec::Expr(expr)) => eval_const_int_expr_in_scope(expr, st, scope_id),
            Some(decl::LenSpec::Star) => None,  // assumed length
            Some(decl::LenSpec::Colon) => None, // deferred length
            None => Some(1),
        },
        None => Some(1),
    }
}

fn extract_char_kind_in_scope(
    sel: &Option<decl::CharSelector>,
    st: &SymbolTable,
    scope_id: crate::sema::symtab::ScopeId,
) -> Option<u8> {
    sel.as_ref()
        .and_then(|selector| selector.kind.as_ref())
        .and_then(|kind| eval_const_int_expr_in_scope(kind, st, scope_id))
        .and_then(|kind| u8::try_from(kind).ok())
}

pub(crate) fn type_spec_to_info(ts: &TypeSpec, st: &SymbolTable) -> TypeInfo {
    type_spec_to_info_in_scope(ts, st, st.current_scope())
}

pub(crate) fn type_spec_to_info_in_scope(
    ts: &TypeSpec,
    st: &SymbolTable,
    scope_id: crate::sema::symtab::ScopeId,
) -> TypeInfo {
    match ts {
        TypeSpec::Integer(sel) => TypeInfo::Integer {
            kind: extract_kind_in_scope(sel, st, scope_id),
        },
        TypeSpec::Real(sel) => TypeInfo::Real {
            kind: extract_kind_in_scope(sel, st, scope_id),
        },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(sel) => TypeInfo::Complex {
            kind: extract_kind_in_scope(sel, st, scope_id),
        },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(sel) => TypeInfo::Logical {
            kind: extract_kind_in_scope(sel, st, scope_id),
        },
        TypeSpec::Character(sel) => TypeInfo::Character {
            len: extract_char_len_in_scope(sel, st, scope_id),
            kind: extract_char_kind_in_scope(sel, st, scope_id),
        },
        TypeSpec::Type(name) if ieee_opaque_int_kind(name).is_some() => {
            // The IEEE_ARITHMETIC/IEEE_EXCEPTIONS opaque types carry a
            // small enumerated tag (class/round/flag) or an FP-env save
            // buffer (status). We model them as integer under the hood
            // (l09 deliverable 2, documented ABI): assignment, `==`/`/=`,
            // and named-constant equality become plain integer ops.
            TypeInfo::Integer {
                kind: ieee_opaque_int_kind(name),
            }
        }
        TypeSpec::Type(name) => {
            // TYPE(name) covers derived types AND F2023 enumeration /
            // named-enum types (the standard reuses the spelling —
            // 7.6.2 NOTE declares `Type(v_value) :: x`). What `name`
            // denotes decides.
            match st.find_symbol_any_scope(&name.to_lowercase()) {
                Some(sym)
                    if matches!(sym.kind, crate::sema::symtab::SymbolKind::EnumerationType) =>
                {
                    sym.type_info
                        .clone()
                        .unwrap_or(TypeInfo::Derived(name.clone()))
                }
                _ => TypeInfo::Derived(name.clone()),
            }
        }
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
        // F2023 TYPEOF/CLASSOF resolve at declaration time to the
        // referenced entity's declared type. Resolution is source-
        // ordered, so a forward (or missing) reference fails the
        // lookup here; validation emits the diagnostic and the
        // TypeStar fallback never survives to lowering.
        TypeSpec::TypeOf(entity) => match st.find_symbol_any_scope(&entity.to_lowercase()) {
            Some(sym) => match sym.type_info.clone() {
                // TYPEOF gives the non-polymorphic declared type.
                Some(TypeInfo::Class(base)) => TypeInfo::Derived(base),
                Some(ti) => ti,
                None => TypeInfo::TypeStar,
            },
            None => TypeInfo::TypeStar,
        },
        TypeSpec::ClassOf(entity) => match st.find_symbol_any_scope(&entity.to_lowercase()) {
            Some(sym) => match sym.type_info.clone() {
                Some(TypeInfo::Derived(base)) | Some(TypeInfo::Class(base)) => {
                    TypeInfo::Class(base)
                }
                // CLASSOF of a non-derived entity is rejected in
                // validation; fall back like TYPEOF.
                _ => TypeInfo::TypeStar,
            },
            None => TypeInfo::TypeStar,
        },
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
