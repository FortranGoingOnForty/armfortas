//! `ALLOCATE` / `DEALLOCATE` integrity checks and component-leaf
//! resolution.
//!
//! Extracted from `core.rs` in Sprint 13. Centralizes the rule that
//! only allocatable or pointer entities can appear in `ALLOCATE` /
//! `DEALLOCATE`, and the helper that walks a component-access chain
//! to its leaf `FieldLayout`. Pointer-target validation is in
//! `pointer.rs`; this module is the data side (storage attributes).

use crate::ast::expr::Expr;

use super::core::{extract_base_name, Ctx};

pub(super) fn validate_allocatable_item(
    ctx: &mut Ctx,
    item: &crate::ast::expr::SpannedExpr,
    stmt_name: &str,
) {
    if expr_selects_component(item) {
        if let Some(leaf) = leaf_field_layout(ctx, item) {
            if !leaf.field.allocatable && !leaf.field.pointer {
                ctx.error(
                    item.span,
                    format!(
                        "only allocatable or pointer components can appear in {}, but '{}' is neither",
                        stmt_name.to_uppercase(),
                        leaf.field.name
                    ),
                );
            }
        }
        return;
    }
    let base_name = extract_base_name(item);
    if let Some(ref name) = base_name {
        let ok = ctx
            .lookup(name)
            .map(|s| s.attrs.allocatable || s.attrs.pointer)
            .unwrap_or(true); // unknown symbol — skip
        if !ok {
            ctx.error(
                item.span,
                format!(
                    "only allocatable or pointer variables can appear in {}, but '{}' is neither",
                    stmt_name.to_uppercase(),
                    name
                ),
            );
        }
    }
}

pub(super) fn allocate_item_needs_explicit_shape(
    ctx: &Ctx<'_>,
    item: &crate::ast::expr::SpannedExpr,
) -> bool {
    match &item.node {
        Expr::Name { name } => ctx
            .allocatable_array_targets
            .contains(&(ctx.scope_id, name.to_lowercase())),
        Expr::ParenExpr { inner } => allocate_item_needs_explicit_shape(ctx, inner),
        Expr::ComponentAccess { .. } => leaf_field_layout(ctx, item)
            .map(|leaf| leaf.field.declared_array)
            .unwrap_or(false),
        _ => false,
    }
}

/// Does this expression select into a derived-type component
/// anywhere in its path? e.g. `pools(i)%tokens(n)` → true,
/// `pools(i)` → false, `pools` → false.
pub(super) fn expr_selects_component(expr: &crate::ast::expr::SpannedExpr) -> bool {
    match &expr.node {
        Expr::ComponentAccess { .. } => true,
        Expr::FunctionCall { callee, .. } => expr_selects_component(callee),
        _ => false,
    }
}

/// Resolved metadata for the leaf of a component access.
pub(super) struct LeafComponent<'a> {
    pub(super) field: &'a crate::sema::type_layout::FieldLayout,
    /// Layout that directly declares the leaf field. Procedure-pointer
    /// components encode their interface name in `field.type_info`; resolving
    /// that name must start from this declaration scope, not from whichever
    /// caller happens to select the component.
    pub(super) owner_layout: &'a crate::sema::type_layout::TypeLayout,
    /// Any ancestor on the path (including the base variable or any
    /// intermediate component) has the TARGET attribute.  F2018
    /// §8.5.14: a subobject of a TARGET is itself a valid target.
    pub(super) ancestor_is_target: bool,
    /// Any ancestor is ALLOCATABLE — per §8.5.14, an allocated
    /// subobject of an allocatable is also a valid target.
    pub(super) ancestor_is_allocatable: bool,
    /// Any ancestor is a pointer. Defining a subobject reached through
    /// that pointer defines its target, not the pointer-bearing ancestor.
    pub(super) ancestor_is_pointer: bool,
}

/// Walk an expression down to its leaf component access and return
/// that component's FieldLayout (with attribute metadata).  Returns
/// `None` if the expression has no component access, or if the
/// chain's derived-type path can't be resolved through the symbol
/// table + layout registry (for example, a field whose type is a
/// derived type that wasn't in the registry — uncommon but possible
/// when a cross-TU .amod is stale).
pub(super) fn leaf_field_layout<'a>(
    ctx: &'a Ctx,
    expr: &crate::ast::expr::SpannedExpr,
) -> Option<LeafComponent<'a>> {
    let layouts = ctx.type_layouts?;
    let mut chain: Vec<&str> = Vec::new();
    let mut cur = expr;
    let base_name = loop {
        match &cur.node {
            Expr::ComponentAccess { base, component } => {
                chain.push(component.as_str());
                cur = base;
            }
            Expr::FunctionCall { callee, .. } => {
                cur = callee;
            }
            Expr::Name { name } => break name.as_str(),
            _ => return None,
        }
    };
    chain.reverse();
    if chain.is_empty() {
        return None;
    }
    let sym = ctx.lookup(base_name)?;
    let base_type = match sym.type_info.as_ref()? {
        crate::sema::symtab::TypeInfo::Derived(name)
        | crate::sema::symtab::TypeInfo::Class(name) => name.clone(),
        _ => return None,
    };
    let mut ancestor_is_target = sym.attrs.target;
    let mut ancestor_is_allocatable = sym.attrs.allocatable;
    let mut ancestor_is_pointer = sym.attrs.pointer;
    let mut current_layout = layouts
        .get_for_scope(ctx.scope_id, &base_type)
        .or_else(|| layouts.get(&base_type))?;
    let mut leaf: Option<&crate::sema::type_layout::FieldLayout> = None;
    let mut leaf_owner = current_layout;
    for (i, comp) in chain.iter().enumerate() {
        let field = current_layout.field(comp)?;
        let is_terminal = i + 1 == chain.len();
        if !is_terminal {
            if field.target {
                ancestor_is_target = true;
            }
            if field.allocatable {
                ancestor_is_allocatable = true;
            }
            if field.pointer {
                ancestor_is_pointer = true;
            }
        }
        leaf = Some(field);
        leaf_owner = current_layout;
        if !is_terminal {
            let (crate::sema::symtab::TypeInfo::Derived(name)
            | crate::sema::symtab::TypeInfo::Class(name)) = &field.type_info
            else {
                return None;
            };
            current_layout = layouts.get_related(current_layout, name)?;
        }
    }
    leaf.map(|field| LeafComponent {
        field,
        owner_layout: leaf_owner,
        ancestor_is_target,
        ancestor_is_allocatable,
        ancestor_is_pointer,
    })
}
