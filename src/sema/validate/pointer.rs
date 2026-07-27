//! Pointer-assignment target/source validation.
//!
//! Extracted from `core.rs` in Sprint 13. Implements the F2018 §8.5.14
//! / 10.2.2 rules for `=>`: the LHS must have the POINTER attribute,
//! the RHS must be a valid target (POINTER, TARGET, ALLOCATABLE, or a
//! sub-object of one of those, plus a few function-call escape hatches
//! for `null()` and pointer-valued functions).

use crate::ast::expr::Expr;
use crate::lexer::Span;

use super::allocatable::{expr_selects_component, leaf_field_layout};
use super::core::{extract_base_name, Ctx};
use super::procedure::validate_procedure_pointer_assignment;

pub(super) fn validate_pointer_assignment(
    ctx: &mut Ctx,
    target: &crate::ast::expr::SpannedExpr,
    value: &crate::ast::expr::SpannedExpr,
    span: Span,
) {
    if validate_procedure_pointer_assignment(ctx, target, value, span) {
        return;
    }

    // Component-access target (`p%ptr_field => x`): check the leaf
    // component's attributes through the type-layout registry.  If
    // layouts aren't available (older callers) or the chain can't be
    // resolved, skip the check rather than flag the base variable.
    if expr_selects_component(target) {
        if let Some(leaf) = leaf_field_layout(ctx, target) {
            if !leaf.field.pointer {
                ctx.error(
                    span,
                    format!(
                        "pointer assignment target component '{}' must have pointer attribute",
                        leaf.field.name
                    ),
                );
            }
        }
    } else if let Some(name) = extract_base_name(target) {
        let is_pointer = ctx.lookup(&name).map(|s| s.attrs.pointer).unwrap_or(true);
        if !is_pointer {
            ctx.error(
                span,
                format!(
                    "pointer assignment target '{}' must have pointer attribute",
                    name
                ),
            );
        }
    }

    // RHS must have target attribute or be a pointer (or null()/function call).
    if expr_selects_component(value) {
        // F2018 §8.5.14: a subobject of a TARGET base (or an
        // allocated ALLOCATABLE) is itself a valid target, so accept
        // when any ancestor on the path carries one of those
        // attributes.
        if let Some(leaf) = leaf_field_layout(ctx, value) {
            let ok = leaf.field.pointer
                || leaf.field.allocatable
                || leaf.field.target
                || leaf.ancestor_is_target
                || leaf.ancestor_is_allocatable;
            if !ok {
                ctx.error(span, format!(
                    "pointer assignment source component '{}' must have target or pointer attribute",
                    leaf.field.name
                ));
            }
        }
        return;
    }
    if let Some(name) = extract_base_name(value) {
        // Skip if the value is a function call — could be null() or
        // pointer-valued function.
        if matches!(value.node, Expr::FunctionCall { .. }) {
            return;
        }
        // Dummy procedure arguments are valid RHS targets per F2003
        // (their addresses are implicitly available).
        if let Some(sym) = ctx.lookup(&name) {
            use crate::sema::symtab::SymbolKind;
            if matches!(sym.kind, SymbolKind::Function | SymbolKind::Subroutine) {
                return;
            }
            if sym.attrs.external {
                return;
            }
        }
        let ok = ctx
            .lookup(&name)
            .map(|s| s.attrs.target || s.attrs.pointer)
            .unwrap_or(true);
        if !ok {
            ctx.error(
                span,
                format!(
                    "pointer assignment source '{}' must have target or pointer attribute",
                    name
                ),
            );
        }
    }
}
