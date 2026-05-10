//! Lowering of Fortran expressions (Expr::*) to IR ValueIds.
//!
//! Extracted from `core.rs` in Sprint 11 Stage D. Pure mechanical
//! move — behavior unchanged. Each Expr variant currently lives in
//! the `lower_expr_full` dispatcher; future sub-stages may split
//! per-variant.

use std::collections::HashMap;
use std::io::Write;

use crate::ast::expr::{Expr, SpannedExpr};
use crate::ir::builder::FuncBuilder;
use crate::ir::inst::*;
use crate::ir::types::*;
use crate::sema::symtab::SymbolTable;

use super::core::*;
use super::ctx::{current_proc_scope, CharKind, HiddenResultAbi, LocalInfo, LowerCtx};
use super::const_scalar::{clamp_const_to_type, materialize_const_scalar, ConstScalar};
use super::helpers::coerce_to_type;
use crate::ast::expr::{BinaryOp, UnaryOp};

/// Lower an expression to a ValueId.
pub(crate) fn lower_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> ValueId {
    lower_expr_full(b, locals, expr, st, None, None, None, None)
}

pub(crate) fn lower_expr_with_optional_layouts(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
) -> ValueId {
    if let Some(tl) = type_layouts {
        lower_expr_tl(b, locals, expr, st, tl)
    } else {
        lower_expr(b, locals, expr, st)
    }
}

pub(crate) fn lower_expr_ctx(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    expr: &crate::ast::expr::SpannedExpr,
) -> ValueId {
    lower_expr_full(
        b,
        &ctx.locals,
        expr,
        ctx.st,
        Some(ctx.type_layouts),
        Some(ctx.internal_funcs),
        Some(ctx.contained_host_refs),
        Some(ctx.descriptor_params),
    )
}

pub(crate) fn lower_expr_tl(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    tl: &crate::sema::type_layout::TypeLayoutRegistry,
) -> ValueId {
    lower_expr_full(b, locals, expr, st, Some(tl), None, None, None)
}

pub(crate) fn lower_expr_ctx_tl(
    b: &mut FuncBuilder,
    ctx: &LowerCtx,
    expr: &crate::ast::expr::SpannedExpr,
) -> ValueId {
    lower_expr_full(
        b,
        &ctx.locals,
        expr,
        ctx.st,
        Some(ctx.type_layouts),
        Some(ctx.internal_funcs),
        Some(ctx.contained_host_refs),
        Some(ctx.descriptor_params),
    )
}

pub(super) fn lower_short_circuit_logical_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
    internal_funcs: Option<&HashMap<String, u32>>,
    contained_host_refs: Option<&HashMap<String, Vec<String>>>,
    descriptor_params: Option<&HashMap<String, Vec<bool>>>,
) -> ValueId {
    match &expr.node {
        Expr::ParenExpr { inner } => lower_short_circuit_logical_expr(
            b,
            locals,
            inner,
            st,
            type_layouts,
            internal_funcs,
            contained_host_refs,
            descriptor_params,
        ),
        Expr::BinaryOp {
            op: BinaryOp::And,
            left,
            right,
        } => {
            let lhs = lower_short_circuit_logical_expr(
                b,
                locals,
                left,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let bb_rhs = b.create_block("and_rhs_expr");
            let bb_false = b.create_block("and_false_expr");
            let bb_merge = b.create_block("and_merge_expr");
            let result = b.add_block_param(bb_merge, IrType::Bool);
            b.cond_branch(lhs, bb_rhs, vec![], bb_false, vec![]);

            b.set_block(bb_rhs);
            let rhs = lower_short_circuit_logical_expr(
                b,
                locals,
                right,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            b.branch(bb_merge, vec![rhs]);

            b.set_block(bb_false);
            let false_val = b.const_bool(false);
            b.branch(bb_merge, vec![false_val]);

            b.set_block(bb_merge);
            result
        }
        Expr::BinaryOp {
            op: BinaryOp::Or,
            left,
            right,
        } => {
            let lhs = lower_short_circuit_logical_expr(
                b,
                locals,
                left,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let bb_true = b.create_block("or_true_expr");
            let bb_rhs = b.create_block("or_rhs_expr");
            let bb_merge = b.create_block("or_merge_expr");
            let result = b.add_block_param(bb_merge, IrType::Bool);
            b.cond_branch(lhs, bb_true, vec![], bb_rhs, vec![]);

            b.set_block(bb_true);
            let true_val = b.const_bool(true);
            b.branch(bb_merge, vec![true_val]);

            b.set_block(bb_rhs);
            let rhs = lower_short_circuit_logical_expr(
                b,
                locals,
                right,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            b.branch(bb_merge, vec![rhs]);

            b.set_block(bb_merge);
            result
        }
        _ => {
            let raw = lower_expr_full(
                b,
                locals,
                expr,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            coerce_to_type(b, raw, &IrType::Bool)
        }
    }
}

/// Walk `expr` and clone it, replacing any `Expr::Name { name: p }`
/// where `p` is in `subst` with a fresh clone of the mapped expression.
/// Used by the F77 statement-function inline-substitution path so each
/// reference to a dummy parameter in the body becomes the actual
/// argument expression — and each reference is an independent clone
/// (so downstream walkers don't see shared node identity).
pub(super) fn substitute_names_in_expr(
    expr: &SpannedExpr,
    subst: &HashMap<String, &SpannedExpr>,
) -> SpannedExpr {
    use crate::ast::expr::{AcValue, ImpliedDoLoop, SectionSubscript};

    fn rewrite_section(
        s: &SectionSubscript,
        subst: &HashMap<String, &SpannedExpr>,
    ) -> SectionSubscript {
        match s {
            SectionSubscript::Element(e) => {
                SectionSubscript::Element(substitute_names_in_expr(e, subst))
            }
            SectionSubscript::Range { start, end, stride } => SectionSubscript::Range {
                start: start.as_ref().map(|e| substitute_names_in_expr(e, subst)),
                end: end.as_ref().map(|e| substitute_names_in_expr(e, subst)),
                stride: stride.as_ref().map(|e| substitute_names_in_expr(e, subst)),
            },
        }
    }

    fn rewrite_acvalue(v: &AcValue, subst: &HashMap<String, &SpannedExpr>) -> AcValue {
        match v {
            AcValue::Expr(e) => AcValue::Expr(substitute_names_in_expr(e, subst)),
            AcValue::ImpliedDo(ido) => AcValue::ImpliedDo(Box::new(ImpliedDoLoop {
                values: ido.values.iter().map(|v| rewrite_acvalue(v, subst)).collect(),
                var: ido.var.clone(),
                start: substitute_names_in_expr(&ido.start, subst),
                end: substitute_names_in_expr(&ido.end, subst),
                step: ido.step.as_ref().map(|e| substitute_names_in_expr(e, subst)),
            })),
        }
    }

    let new_node = match &expr.node {
        Expr::Name { name } => {
            if let Some(repl) = subst.get(&name.to_ascii_lowercase()) {
                return (*repl).clone();
            }
            Expr::Name { name: name.clone() }
        }
        Expr::ComponentAccess { base, component } => Expr::ComponentAccess {
            base: Box::new(substitute_names_in_expr(base, subst)),
            component: component.clone(),
        },
        Expr::UnaryOp { op, operand } => Expr::UnaryOp {
            op: op.clone(),
            operand: Box::new(substitute_names_in_expr(operand, subst)),
        },
        Expr::BinaryOp { op, left, right } => Expr::BinaryOp {
            op: op.clone(),
            left: Box::new(substitute_names_in_expr(left, subst)),
            right: Box::new(substitute_names_in_expr(right, subst)),
        },
        Expr::FunctionCall { callee, args } => Expr::FunctionCall {
            callee: Box::new(substitute_names_in_expr(callee, subst)),
            args: args
                .iter()
                .map(|a| crate::ast::expr::Argument {
                    keyword: a.keyword.clone(),
                    value: rewrite_section(&a.value, subst),
                })
                .collect(),
        },
        Expr::ArrayConstructor { type_spec, values } => Expr::ArrayConstructor {
            type_spec: type_spec.clone(),
            values: values.iter().map(|v| rewrite_acvalue(v, subst)).collect(),
        },
        Expr::ParenExpr { inner } => Expr::ParenExpr {
            inner: Box::new(substitute_names_in_expr(inner, subst)),
        },
        Expr::ComplexLiteral { real, imag } => Expr::ComplexLiteral {
            real: Box::new(substitute_names_in_expr(real, subst)),
            imag: Box::new(substitute_names_in_expr(imag, subst)),
        },
        // Literals (Integer, Real, String, Logical, Boz) — copy as-is.
        other => other.clone(),
    };
    SpannedExpr {
        node: new_node,
        span: expr.span,
    }
}

pub(crate) fn lower_expr_full(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
    internal_funcs: Option<&HashMap<String, u32>>,
    contained_host_refs: Option<&HashMap<String, Vec<String>>>,
    descriptor_params: Option<&HashMap<String, Vec<bool>>>,
) -> ValueId {
    match &expr.node {
        Expr::IntegerLiteral { text, kind, .. } => {
            let clean = text.split('_').next().unwrap_or(text);
            let kind_width = kind
                .as_deref()
                .map(|kind_str| int_kind_to_width_in_context(kind_str, Some(locals), None, Some(st)))
                .unwrap_or_else(crate::driver::defaults::default_int_kind);
            let val: i128 = clean.parse().unwrap_or(0);
            let width = match kind_width {
                0..=1 => IntWidth::I8,
                2 => IntWidth::I16,
                3..=4 => IntWidth::I32,
                5..=8 => IntWidth::I64,
                _ => IntWidth::I128,
            };
            b.const_int(val, width)
        }
        Expr::RealLiteral { text, kind } => {
            let val: f64 = text
                .replace('d', "e")
                .replace('D', "E")
                .parse()
                .unwrap_or(0.0);
            // Determine width from kind suffix (_dp, _8), 'd' exponent, or default.
            let is_f64 = if let Some(kind_str) = kind {
                real_kind_to_width_in_context(kind_str, Some(locals), None, Some(st)) == 8
            } else {
                text.to_lowercase().contains('d')
            };
            if is_f64 {
                b.const_f64(val)
            } else {
                b.const_f32(val as f32)
            }
        }
        Expr::LogicalLiteral { value, .. } => b.const_bool(*value),
        Expr::StringLiteral { value, .. } => b.const_string(value.as_bytes()),
        Expr::BozLiteral { text, base } => {
            // BOZ literals: strip prefix letter and quotes, parse digit string.
            let radix = match base {
                crate::ast::expr::BozBase::Binary => 2,
                crate::ast::expr::BozBase::Octal => 8,
                crate::ast::expr::BozBase::Hex => 16,
            };
            // Token text is like Z'FF' or B'1010' — extract the digits between quotes.
            let digits: String = text
                .chars()
                .skip_while(|c| !matches!(c, '\'' | '"'))
                .skip(1) // skip opening quote
                .take_while(|c| !matches!(c, '\'' | '"'))
                .collect();
            let val = i64::from_str_radix(&digits, radix).unwrap_or(0);
            if val > i32::MAX as i64 || val < i32::MIN as i64 {
                b.const_i64(val)
            } else {
                b.const_i32(val as i32)
            }
        }

        Expr::Name { name } => {
            let key = name.to_lowercase();
            if let Some(info) = locals.get(&key) {
                // Audit MAJOR-4: PARAMETER-attributed locals with
                // a folded value get inlined directly. The const
                // is materialized via the appropriate b.const_*
                // helper, matching the local's declared type.
                if let Some(c) = info.inline_const {
                    return materialize_const_scalar(b, c, &info.ty);
                }
                if !info.dims.is_empty() {
                    // Array name without subscripts — return the base address.
                    info.addr
                } else if info.is_pointer && is_complex_ty(&info.ty) {
                    // Complex POINTER: slot holds ptr<[f32/f64 x 2]>.
                    // Consumers of complex values want the *address* of
                    // the 2-element buffer (same ABI as an ordinary
                    // complex variable), so load once to get the
                    // associated buffer and return that.
                    b.load_typed(info.addr, IrType::Ptr(Box::new(info.ty.clone())))
                } else if info.is_pointer && info.derived_type.is_none() {
                    // Scalar Fortran POINTER: `info.addr` is an alloca
                    // ptr<T>.  Reading the pointer as a value
                    // dereferences it: load the target address out of
                    // the slot, then load the value through it.
                    let tgt = b.load_typed(info.addr, IrType::Ptr(Box::new(info.ty.clone())));
                    b.load_typed(tgt, info.ty.clone())
                } else if info.is_pointer && info.derived_type.is_some() {
                    if local_uses_array_descriptor(info) && info.dims.is_empty() {
                        array_base_addr(b, info)
                    } else {
                        // Derived-type POINTER used as a bare Name
                        // (e.g. passed to a subroutine expecting
                        // type(t)). The consumer wants the struct
                        // address, which is what's stored in the
                        // pointer slot.
                        b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                    }
                } else if info.derived_type.is_some() {
                    if info
                        .derived_type
                        .as_ref()
                        .map(|name| is_opaque_c_handle_name(name))
                        .unwrap_or(false)
                    {
                        if info.by_ref {
                            let raw_ptr = b.load_typed(info.addr, IrType::Int(IntWidth::I64));
                            let ptr = b.int_to_ptr(raw_ptr, info.ty.clone());
                            b.load_typed(ptr, info.ty.clone())
                        } else {
                            b.load_typed(info.addr, info.ty.clone())
                        }
                    } else
                    // Derived type variable: storage is `alloca [i8 x size]`.
                    // Consumers of the value treat it as a pointer to the
                    // struct (memcpy for whole-struct assignment, GEP for
                    // component access). Without this case we fell through
                    // to load_typed(info.ty) which yanked the first 8 bytes
                    // of the struct as if they were a pointer, turning
                    // `b = a` into a memcpy from the garbage address held
                    // by a's first field slot.
                    {
                        if info.allocatable {
                            array_base_addr(b, info)
                        } else if info.by_ref {
                            b.load(info.addr)
                        } else {
                            info.addr
                        }
                    }
                } else if is_complex_ty(&info.ty) {
                    if info.by_ref {
                        // by-ref complex: info.addr holds ptr-to-ptr-to-buffer.
                        // Load once to get ptr-to-buffer; caller treats as address.
                        b.load(info.addr)
                    } else {
                        // Complex variable: return the stack-buffer address.
                        // Complex is stored as [f32/f64 x 2] — callers use the address
                        // directly (memcpy for assignment, ptr for I/O, GEP for components).
                        info.addr
                    }
                } else if info.by_ref {
                    // Pass-by-reference param: load the pointer, then load through it.
                    let ptr = b.load(info.addr);
                    b.load_typed(ptr, info.ty.clone())
                } else {
                    // Use load_typed with the local's declared type to handle cases
                    // where the address pointer type doesn't exactly match (e.g.,
                    // WHERE substitution using byte-level GEP).
                    b.load_typed(info.addr, info.ty.clone())
                }
            } else {
                if let Some(sym) = st.find_symbol_any_scope(&key) {
                    if let Some(cv) = sym.const_value {
                        if let Some(type_info) = sym.type_info.as_ref() {
                            let target = type_info_to_ir_type(type_info);
                            let clamped =
                                clamp_const_to_type(ConstScalar::Int(cv as i128), &target);
                            materialize_const_scalar(b, clamped, &target)
                        } else if i32::try_from(cv).is_ok() {
                            b.const_i32(cv as i32)
                        } else {
                            b.const_i64(cv)
                        }
                    } else {
                        b.const_i32(0)
                    }
                } else {
                    b.const_i32(0)
                }
            }
        }

        Expr::BinaryOp { op, left, right } => {
            if matches!(op, BinaryOp::Concat)
                && expr_is_character_expr(b, locals, left, st, type_layouts)
                && expr_is_character_expr(b, locals, right, st, type_layouts)
            {
                let (ptr, _len) = lower_string_expr_full(
                    b,
                    locals,
                    expr,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                );
                return ptr;
            }
            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) && (expr_is_character_expr(b, locals, left, st, type_layouts)
                || expr_is_character_expr(b, locals, right, st, type_layouts))
            {
                let (lhs_ptr, lhs_len) = lower_string_expr_full(
                    b,
                    locals,
                    left,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                );
                let (rhs_ptr, rhs_len) = lower_string_expr_full(
                    b,
                    locals,
                    right,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                );
                let cmp = b.call(
                    FuncRef::External("afs_compare_char".into()),
                    vec![lhs_ptr, lhs_len, rhs_ptr, rhs_len],
                    IrType::Int(IntWidth::I32),
                );
                let zero = b.const_i32(0);
                return match op {
                    BinaryOp::Eq => b.icmp(CmpOp::Eq, cmp, zero),
                    BinaryOp::Ne => b.icmp(CmpOp::Ne, cmp, zero),
                    BinaryOp::Lt => b.icmp(CmpOp::Lt, cmp, zero),
                    BinaryOp::Le => b.icmp(CmpOp::Le, cmp, zero),
                    BinaryOp::Gt => b.icmp(CmpOp::Gt, cmp, zero),
                    BinaryOp::Ge => b.icmp(CmpOp::Ge, cmp, zero),
                    _ => unreachable!(),
                };
            }
            if matches!(op, BinaryOp::And | BinaryOp::Or) {
                return lower_short_circuit_logical_expr(
                    b,
                    locals,
                    expr,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                );
            }
            let mut lhs = lower_expr_full(
                b,
                locals,
                left,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let mut rhs = lower_expr_full(
                b,
                locals,
                right,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let lty = b
                .func()
                .value_type(lhs)
                .unwrap_or(IrType::Int(IntWidth::I32));
            let rty = b
                .func()
                .value_type(rhs)
                .unwrap_or(IrType::Int(IntWidth::I32));

            // Defined operator dispatch (INTERFACE OPERATOR(...)): if a
            // generic interface for this operator exists and a specific
            // matches the actual operand types, emit a call instead of
            // arithmetic. Needed for e.g. `type(vec) + type(vec)` — the
            // default arithmetic would ICE trying to `iadd` pointers.
            if let Some(specific) = resolve_operator_overload(
                b,
                locals,
                st,
                type_layouts,
                op,
                &lty,
                &rty,
                left,
                right,
                lhs,
                rhs,
            ) {
                return emit_resolved_operator_call(
                    b,
                    locals,
                    st,
                    type_layouts,
                    internal_funcs,
                    contained_host_refs,
                    descriptor_params,
                    &specific,
                    left,
                    right,
                    lhs,
                    rhs,
                );
            }

            // Complex arithmetic: both operands are ptr<[f32/f64 x 2]>.
            // Add/Sub operate component-wise; Mul uses (ac-bd, ad+bc);
            // Eq/Ne reduce to a scalar bool over the two lanes
            // (F2018 §10.1.10.4: a == b iff re(a) == re(b) and im(a)
            // == im(b)).
            if is_complex_ty(&lty) || is_complex_ty(&rty) {
                let fw = if matches!(lty, IrType::Float(FloatWidth::F64))
                    || matches!(rty, IrType::Float(FloatWidth::F64))
                    || (is_complex_ty(&lty) && complex_float_width(&lty) == FloatWidth::F64)
                    || (is_complex_ty(&rty) && complex_float_width(&rty) == FloatWidth::F64)
                {
                    FloatWidth::F64
                } else {
                    FloatWidth::F32
                };
                let elem = IrType::Float(fw);
                let esz = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                let zero = b.const_i64(0);
                let lhs_buf = materialize_complex_operand(b, lhs, fw);
                let rhs_buf = materialize_complex_operand(b, rhs, fw);
                // Load components from lhs (re_l, im_l).
                let re_l_ptr = b.gep(lhs_buf, vec![zero], IrType::Int(IntWidth::I8));
                let im_l_ptr = b.gep(lhs_buf, vec![esz], IrType::Int(IntWidth::I8));
                let re_l = b.load_typed(re_l_ptr, elem.clone());
                let im_l = b.load_typed(im_l_ptr, elem.clone());
                // Load components from rhs (re_r, im_r).
                let re_r_ptr = b.gep(rhs_buf, vec![zero], IrType::Int(IntWidth::I8));
                let im_r_ptr = b.gep(rhs_buf, vec![esz], IrType::Int(IntWidth::I8));
                let re_r = b.load_typed(re_r_ptr, elem.clone());
                let im_r = b.load_typed(im_r_ptr, elem.clone());
                // Equality / inequality: scalar bool result, not a complex.
                if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                    let re_eq = b.fcmp(CmpOp::Eq, re_l, re_r);
                    let im_eq = b.fcmp(CmpOp::Eq, im_l, im_r);
                    let both_eq = b.and(re_eq, im_eq);
                    return if matches!(op, BinaryOp::Eq) {
                        both_eq
                    } else {
                        b.not(both_eq)
                    };
                }
                let arr_ty = IrType::Array(Box::new(elem.clone()), 2);
                let buf = b.alloca(arr_ty);
                let (re_res, im_res) = match op {
                    BinaryOp::Add => (b.fadd(re_l, re_r), b.fadd(im_l, im_r)),
                    BinaryOp::Sub => (b.fsub(re_l, re_r), b.fsub(im_l, im_r)),
                    BinaryOp::Mul => {
                        // (ac-bd, ad+bc)
                        let ac = b.fmul(re_l, re_r);
                        let bd = b.fmul(im_l, im_r);
                        let ad = b.fmul(re_l, im_r);
                        let bc = b.fmul(im_l, re_r);
                        (b.fsub(ac, bd), b.fadd(ad, bc))
                    }
                    BinaryOp::Div => {
                        // (a+bi)/(c+di) = ((ac+bd)/(c^2+d^2), (bc-ad)/(c^2+d^2))
                        let rr = b.fmul(re_r, re_r);
                        let ii = b.fmul(im_r, im_r);
                        let denom = b.fadd(rr, ii);
                        let ac = b.fmul(re_l, re_r);
                        let bd = b.fmul(im_l, im_r);
                        let bc = b.fmul(im_l, re_r);
                        let ad = b.fmul(re_l, im_r);
                        let real_num = b.fadd(ac, bd);
                        let imag_num = b.fsub(bc, ad);
                        (b.fdiv(real_num, denom), b.fdiv(imag_num, denom))
                    }
                    _ => (re_l, im_l), // unsupported: return lhs unchanged
                };
                let dst_re = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
                let dst_im = b.gep(buf, vec![esz], IrType::Int(IntWidth::I8));
                b.store(re_res, dst_re);
                b.store(im_res, dst_im);
                return buf;
            }

            // Implicit type promotion: if one side is int and the other float,
            // convert the int to float (Fortran mixed-mode arithmetic).
            let result_ty = if lty.is_float() || rty.is_float() {
                let fw = match (&lty, &rty) {
                    (IrType::Float(FloatWidth::F64), _) | (_, IrType::Float(FloatWidth::F64)) => {
                        FloatWidth::F64
                    }
                    _ => FloatWidth::F32,
                };
                if lty.is_int() {
                    lhs = b.int_to_float(lhs, fw);
                }
                if rty.is_int() {
                    rhs = b.int_to_float(rhs, fw);
                }
                // Promote f32 to f64 if other is f64.
                if matches!(lty, IrType::Float(FloatWidth::F32)) && fw == FloatWidth::F64 {
                    lhs = b.float_extend(lhs, FloatWidth::F64);
                }
                if matches!(rty, IrType::Float(FloatWidth::F32)) && fw == FloatWidth::F64 {
                    rhs = b.float_extend(rhs, FloatWidth::F64);
                }
                IrType::Float(fw)
            } else {
                // Integer width promotion: widen the narrower operand to
                // match the wider one. Without this, integer(int64) + 1
                // produces an IR width mismatch (i64 + i32).
                let lw = lty.int_width().unwrap_or(IntWidth::I32);
                let rw = rty.int_width().unwrap_or(IntWidth::I32);
                let target_w = if lw.bits() >= rw.bits() { lw } else { rw };
                if lw != target_w {
                    lhs = b.int_extend(lhs, target_w, true);
                }
                if rw != target_w {
                    rhs = b.int_extend(rhs, target_w, true);
                }
                IrType::Int(target_w)
            };

            match (op, &result_ty) {
                (BinaryOp::Add, IrType::Int(_)) => b.iadd(lhs, rhs),
                (BinaryOp::Add, IrType::Float(_)) => b.fadd(lhs, rhs),
                (BinaryOp::Sub, IrType::Int(_)) => b.isub(lhs, rhs),
                (BinaryOp::Sub, IrType::Float(_)) => b.fsub(lhs, rhs),
                (BinaryOp::Mul, IrType::Int(_)) => b.imul(lhs, rhs),
                (BinaryOp::Mul, IrType::Float(_)) => b.fmul(lhs, rhs),
                (BinaryOp::Div, IrType::Int(_)) => b.idiv(lhs, rhs),
                (BinaryOp::Div, IrType::Float(_)) => b.fdiv(lhs, rhs),
                (BinaryOp::Pow, IrType::Float(_)) => b.fpow(lhs, rhs),
                (BinaryOp::Pow, IrType::Int(_)) => {
                    let fl = b.int_to_float(lhs, FloatWidth::F64);
                    let fr = b.int_to_float(rhs, FloatWidth::F64);
                    let result = b.fpow(fl, fr);
                    b.float_to_int(result, IntWidth::I32)
                }
                (BinaryOp::Eq, IrType::Int(_)) => b.icmp(CmpOp::Eq, lhs, rhs),
                (BinaryOp::Eq, IrType::Float(_)) => b.fcmp(CmpOp::Eq, lhs, rhs),
                (BinaryOp::Ne, IrType::Int(_)) => b.icmp(CmpOp::Ne, lhs, rhs),
                (BinaryOp::Ne, IrType::Float(_)) => b.fcmp(CmpOp::Ne, lhs, rhs),
                (BinaryOp::Lt, IrType::Int(_)) => b.icmp(CmpOp::Lt, lhs, rhs),
                (BinaryOp::Lt, IrType::Float(_)) => b.fcmp(CmpOp::Lt, lhs, rhs),
                (BinaryOp::Le, IrType::Int(_)) => b.icmp(CmpOp::Le, lhs, rhs),
                (BinaryOp::Le, IrType::Float(_)) => b.fcmp(CmpOp::Le, lhs, rhs),
                (BinaryOp::Gt, IrType::Int(_)) => b.icmp(CmpOp::Gt, lhs, rhs),
                (BinaryOp::Gt, IrType::Float(_)) => b.fcmp(CmpOp::Gt, lhs, rhs),
                (BinaryOp::Ge, IrType::Int(_)) => b.icmp(CmpOp::Ge, lhs, rhs),
                (BinaryOp::Ge, IrType::Float(_)) => b.fcmp(CmpOp::Ge, lhs, rhs),
                (BinaryOp::And, _) => {
                    // Coerce to Bool if not already (Fortran .AND. on integers).
                    let lbool = coerce_to_type(b, lhs, &IrType::Bool);
                    let rbool = coerce_to_type(b, rhs, &IrType::Bool);
                    b.and(lbool, rbool)
                }
                (BinaryOp::Or, _) => {
                    let lbool = coerce_to_type(b, lhs, &IrType::Bool);
                    let rbool = coerce_to_type(b, rhs, &IrType::Bool);
                    b.or(lbool, rbool)
                }
                (BinaryOp::Eqv, _) => {
                    // a .eqv. b = .not. (a .xor. b)
                    let lbool = coerce_to_type(b, lhs, &IrType::Bool);
                    let rbool = coerce_to_type(b, rhs, &IrType::Bool);
                    let both = b.and(lbool, rbool);
                    let either = b.or(lbool, rbool);
                    let not_both = b.not(both);
                    let xor = b.and(either, not_both);
                    b.not(xor)
                }
                (BinaryOp::Neqv, _) => {
                    // a .neqv. b = a .xor. b
                    let lbool = coerce_to_type(b, lhs, &IrType::Bool);
                    let rbool = coerce_to_type(b, rhs, &IrType::Bool);
                    let both = b.and(lbool, rbool);
                    let either = b.or(lbool, rbool);
                    let not_both = b.not(both);
                    b.and(either, not_both)
                }
                (BinaryOp::Concat, _) => b.runtime_call(
                    RuntimeFunc::StringConcat,
                    vec![lhs, rhs],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                ),
                _ => b.iadd(lhs, rhs), // fallback for defined ops
            }
        }

        Expr::UnaryOp { op, operand } => {
            // F2018 §10.1.5: a defined unary operator `.op.X` is sugar
            // for a single-arg call to the matching specific procedure
            // declared in `INTERFACE OPERATOR(.op.)`. Without this
            // dispatch, `.det.matrix` falls through to the catch-all
            // below and silently returns the operand value, so callers
            // see the raw matrix instead of its determinant.
            if let UnaryOp::Defined(name) = op {
                let iface = format!("operator(.{}.)", name.to_lowercase());
                if let Some(sym) = st.find_symbol_any_scope(&iface) {
                    if sym.kind == crate::sema::symtab::SymbolKind::NamedInterface
                        && !sym.arg_names.is_empty()
                    {
                        let operand_ti =
                            operator_expr_type_info(operand, Some(locals), st, type_layouts);
                        let chosen = sym
                            .arg_names
                            .iter()
                            .find(|specific| {
                                procedure_scope_by_name(st, specific)
                                    .map(|scope| {
                                        let declared = declared_args_for_scope(scope);
                                        declared.len() == 1
                                            && declared
                                                .first()
                                                .and_then(|f| f.type_info.as_ref())
                                                .is_some_and(|decl| {
                                                    operator_arg_semantic_match(
                                                        decl,
                                                        operand_ti.as_ref(),
                                                    )
                                                })
                                    })
                                    .unwrap_or(false)
                            })
                            .cloned();
                        if let Some(specific) = chosen {
                            let synth = crate::ast::Spanned::new(
                                Expr::FunctionCall {
                                    callee: Box::new(crate::ast::Spanned::new(
                                        Expr::Name { name: specific },
                                        operand.span,
                                    )),
                                    args: vec![crate::ast::expr::Argument {
                                        keyword: None,
                                        value: crate::ast::expr::SectionSubscript::Element(
                                            (**operand).clone(),
                                        ),
                                    }],
                                },
                                expr.span,
                            );
                            return lower_expr_full(
                                b,
                                locals,
                                &synth,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            );
                        }
                    }
                }
            }
            let val = lower_expr_full(
                b,
                locals,
                operand,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let ty = b
                .func()
                .value_type(val)
                .unwrap_or(IrType::Int(IntWidth::I32));
            match (op, &ty) {
                (UnaryOp::Minus, IrType::Int(_)) => b.ineg(val),
                (UnaryOp::Minus, IrType::Float(_)) => b.fneg(val),
                (UnaryOp::Plus, _) => val,
                (UnaryOp::Not, _) => b.not(val),
                _ => val,
            }
        }

        Expr::ParenExpr { inner } => lower_expr_full(
            b,
            locals,
            inner,
            st,
            type_layouts,
            internal_funcs,
            contained_host_refs,
            descriptor_params,
        ),

        Expr::FunctionCall { callee, args } => {
            // F77 §15.4 statement-function call: sema parked the body in
            // a side table on the symbol table. Substitute the dummy
            // parameters for the actual argument exprs and lower the
            // resulting expression in place. No external symbol is
            // emitted — that's the whole point of the intercept.
            if let Expr::Name { name } = &callee.node {
                if let Some(scope_id) = current_proc_scope() {
                    if let Some(def) = st.lookup_statement_function(scope_id, name) {
                        if def.params.len() == args.len()
                            && args.iter().all(|a| {
                                a.keyword.is_none()
                                    && matches!(
                                        a.value,
                                        crate::ast::expr::SectionSubscript::Element(_)
                                    )
                            })
                        {
                            let mut subst: HashMap<String, &SpannedExpr> =
                                HashMap::with_capacity(def.params.len());
                            for (p, a) in def.params.iter().zip(args.iter()) {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                                    subst.insert(p.clone(), e);
                                }
                            }
                            let inlined = substitute_names_in_expr(&def.body, &subst);
                            return lower_expr_full(
                                b,
                                locals,
                                &inlined,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            );
                        }
                    }
                }
            }
            // Special-case TRANSFER intrinsic — needs source bits, not the
            // probe-value pointers used by the generic dispatch path.
            if let Expr::Name { name } = &callee.node {
                if name.eq_ignore_ascii_case("transfer") && args.len() >= 2 {
                    if let Some(result) = lower_transfer_intrinsic(
                        b, locals, args, st, type_layouts,
                        internal_funcs, contained_host_refs, descriptor_params,
                    ) {
                        return result;
                    }
                }
            }
            if args.len() == 1
                && expr_is_character_expr(b, locals, callee, st, type_layouts)
                && !expr_is_array_designator(b, locals, callee, st, type_layouts)
                && !expr_is_callable_character_callee(b, locals, callee, st, type_layouts)
            {
                match &args[0].value {
                    crate::ast::expr::SectionSubscript::Range { start, end, .. } => {
                        let (base_ptr, base_len) =
                            lower_string_expr_with_layouts(b, locals, callee, st, type_layouts);
                        let (ptr, _len) = lower_substring_full(
                            b,
                            locals,
                            st,
                            base_ptr,
                            base_len,
                            start.as_ref(),
                            end.as_ref(),
                            type_layouts,
                            internal_funcs,
                            contained_host_refs,
                            descriptor_params,
                        );
                        return ptr;
                    }
                    crate::ast::expr::SectionSubscript::Element(idx_expr) => {
                        let (base_ptr, base_len) =
                            lower_string_expr_with_layouts(b, locals, callee, st, type_layouts);
                        let (ptr, _len) = lower_substring_full(
                            b,
                            locals,
                            st,
                            base_ptr,
                            base_len,
                            Some(idx_expr),
                            Some(idx_expr),
                            type_layouts,
                            internal_funcs,
                            contained_host_refs,
                            descriptor_params,
                        );
                        return ptr;
                    }
                }
            }
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();
                let procptr_target = procedure_pointer_call_target(b, locals, st, &key);
                let signature_key = procptr_target
                    .as_ref()
                    .map(|(_, sig_key)| sig_key.clone())
                    .unwrap_or_else(|| key.clone());
                let has_named_interface = !internal_funcs
                    .is_some_and(|funcs| funcs.contains_key(&key))
                    && (find_named_interface_symbol(st, &key).is_some()
                        || crate::ir::lower::core::named_interface_specific_candidates(st, &key)
                            .is_some());

                // Check if this is an array element or section access.
                if let Some(info) = locals.get(&key) {
                    if local_is_array_like(info) {
                        let has_range = args.iter().any(|a| {
                            matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. })
                        });
                        if has_range {
                            return lower_array_section(b, locals, info, args, st, type_layouts);
                        }
                        return lower_array_element(b, locals, info, args, st, type_layouts);
                    }
                }

                // Check for pointer intrinsics (ASSOCIATED) first —
                // these work on every pointer shape and don't care
                // about the array-intrinsic filter.
                if !has_named_interface {
                    if let Some(result) =
                        lower_pointer_intrinsic(b, locals, &key, args, st, type_layouts)
                    {
                        return result;
                    }

                    if let Some(result) =
                        lower_scalar_allocated_intrinsic(b, locals, &key, args, st, type_layouts)
                    {
                        return result;
                    }

                    if let Some(result) = lower_logical_reduction_intrinsic_ast(
                        b,
                        &key,
                        args,
                        locals,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return result;
                    }

                    // Check for array intrinsics (SIZE, SUM, etc.) that need descriptor addresses.
                    if let Some(result) =
                            lower_array_intrinsic(
                                b,
                                locals,
                                &key,
                                args,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            )
                    {
                        return result;
                    }

                    // Try character intrinsics (need access to locals for CharKind).
                    if let Some(result) = lower_char_intrinsic(
                        b,
                        &key,
                        args,
                        locals,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return result;
                    }
                }

                // PRESENT(x): check if optional dummy argument x was passed.
                // By-ref params are stored as `alloca Ptr<T>` in locals; when the
                // caller omits an optional arg it passes null (0). Load the stored
                // pointer and compare to zero → non-zero means present.
                if key == "present" {
                    if let Some(arg0) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(e) = &arg0.value {
                            if let Expr::Name { name: arg_name } = &e.node {
                                let akey = arg_name.to_lowercase();
                                if let Some(info) = locals.get(&akey) {
                                    if info.by_ref {
                                        // Load the incoming pointer stored in the by-ref slot.
                                        // If absent, caller passes 0; if present, non-zero address.
                                        let ptr_val = b.load(info.addr);
                                        let zero = b.const_i64(0);
                                        return b.icmp(CmpOp::Ne, ptr_val, zero);
                                    }
                                }
                            }
                        }
                    }
                    // If we can't resolve it (non-standard usage), assume present.
                    return b.const_bool(true);
                }

                // c_loc(x): return the address of the target itself as an
                // i64 integer (matching type(c_ptr)). This must bypass the
                // normal by-ref argument path because character arguments use
                // temporary pointer slots there, while c_loc needs the real
                // underlying element/storage address.
                if key == "c_loc" {
                    if let Some(arg0) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(e) = &arg0.value {
                            if let Expr::Name { name } = &e.node {
                                if let Some(info) = locals.get(&name.to_lowercase()) {
                                    let addr = if info.by_ref {
                                        if info.descriptor_arg {
                                            array_data_ptr_for_call(b, info)
                                        } else if info.char_kind != CharKind::None
                                            && info.dims.is_empty()
                                        {
                                            if let Some((ptr, _)) =
                                                char_addr_and_runtime_len(b, e, locals)
                                            {
                                                ptr
                                            } else {
                                                b.load(info.addr)
                                            }
                                        } else {
                                            b.load(info.addr)
                                        }
                                    } else if !info.dims.is_empty()
                                        || local_uses_array_descriptor(info)
                                    {
                                        array_data_ptr_for_call(b, info)
                                    } else if info.char_kind != CharKind::None {
                                        if let Some((ptr, _)) =
                                            char_addr_and_runtime_len(b, e, locals)
                                        {
                                            ptr
                                        } else {
                                            info.addr
                                        }
                                    } else {
                                        info.addr
                                    };
                                    return b.ptr_to_int(addr);
                                }
                            }

                            if let Expr::FunctionCall {
                                callee,
                                args: subscripts,
                            } = &e.node
                            {
                                if let Expr::Name { name } = &callee.node {
                                    if let Some(info) = locals.get(&name.to_lowercase()) {
                                        if info.char_kind != CharKind::None {
                                            let addr = if local_uses_array_descriptor(info)
                                                || inline_char_array_storage(info)
                                            {
                                                char_array_element_ptr_and_len(
                                                    b,
                                                    locals,
                                                    info,
                                                    subscripts,
                                                    st,
                                                    type_layouts,
                                                )
                                                .map(|(ptr, _)| ptr)
                                                .unwrap_or_else(|| {
                                                    lower_array_element_addr(
                                                        b,
                                                        locals,
                                                        info,
                                                        subscripts,
                                                        st,
                                                        type_layouts,
                                                    )
                                                })
                                            } else {
                                                lower_array_element(
                                                    b,
                                                    locals,
                                                    info,
                                                    subscripts,
                                                    st,
                                                    type_layouts,
                                                )
                                            };
                                            return b.ptr_to_int(addr);
                                        }
                                        if !info.dims.is_empty()
                                            || local_uses_array_descriptor(info)
                                        {
                                            let addr = lower_array_element_addr(
                                                b,
                                                locals,
                                                info,
                                                subscripts,
                                                st,
                                                type_layouts,
                                            );
                                            return b.ptr_to_int(addr);
                                        }
                                    }
                                }
                            }

                            let addr = lower_arg_by_ref(b, locals, e, st);
                            return b.ptr_to_int(addr);
                        }
                    }
                }

                // c_funloc(f): return the entry address of the procedure.
                if key == "c_funloc" {
                    if let Some(arg0) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(e) = &arg0.value {
                            let addr = lower_arg_by_ref(b, locals, e, st);
                            return b.ptr_to_int(addr);
                        }
                    }
                }

                // abs(z) for complex: sqrt(re² + im²).
                // Must be handled before generic intrinsic lowering because
                // complex values are pointers to [f32/f64 x 2] buffers.
                if (key == "abs" || key == "cabs" || key == "cdabs" || key == "zabs")
                    && args.len() == 1
                {
                    if let Some(arg0) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(e) = &arg0.value {
                            let val = lower_expr_full(
                                b,
                                locals,
                                e,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            );
                            let ty = b
                                .func()
                                .value_type(val)
                                .unwrap_or(IrType::Int(IntWidth::I32));
                            if is_complex_ty(&ty) {
                                let fw = complex_float_width(&ty);
                                let elem = IrType::Float(fw);
                                let esz = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                                let zero = b.const_i64(0);
                                let re_ptr = b.gep(val, vec![zero], IrType::Int(IntWidth::I8));
                                let im_ptr = b.gep(val, vec![esz], IrType::Int(IntWidth::I8));
                                let re = b.load_typed(re_ptr, elem.clone());
                                let im = b.load_typed(im_ptr, elem);
                                let re2 = b.fmul(re, re);
                                let im2 = b.fmul(im, im);
                                let sum = b.fadd(re2, im2);
                                return b.fsqrt(sum);
                            }
                        }
                    }
                }

                // sqrt(z) for complex (F2018 §16.9.184): principal-branch
                // sqrt of a complex number, computed as
                //     re = sqrt((|z| + a) / 2)
                //     im = sqrt((|z| - a) / 2) * sign(b)
                // where sign(b) = +1 when b >= 0, else -1. This matches
                // libm's `csqrt` on the principal branch and correctly
                // handles sqrt(-1) = i, sqrt(0) = 0, and z on the real
                // line. We handle this before the generic intrinsic
                // dispatch because complex values are pointers to
                // [f32/f64 x 2] buffers and `b.fsqrt` only knows scalar
                // float; passing a complex pointer through it would
                // emit `fsqrt` on a GPR register.
                if (key == "sqrt" || key == "csqrt" || key == "zsqrt" || key == "cdsqrt")
                    && args.len() == 1
                {
                    if let Some(arg0) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(e) = &arg0.value {
                            let val = lower_expr_full(
                                b,
                                locals,
                                e,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            );
                            let ty = b
                                .func()
                                .value_type(val)
                                .unwrap_or(IrType::Int(IntWidth::I32));
                            if is_complex_ty(&ty) {
                                let fw = complex_float_width(&ty);
                                let elem = IrType::Float(fw);
                                let lane_bytes =
                                    b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
                                let zero_off = b.const_i64(0);
                                let arr_ty = IrType::Array(Box::new(elem.clone()), 2);

                                let buf = match &ty {
                                    IrType::Ptr(_) => val,
                                    _ => {
                                        let tmp = b.alloca(arr_ty.clone());
                                        b.store(val, tmp);
                                        tmp
                                    }
                                };
                                let re_ptr =
                                    b.gep(buf, vec![zero_off], IrType::Int(IntWidth::I8));
                                let im_ptr =
                                    b.gep(buf, vec![lane_bytes], IrType::Int(IntWidth::I8));
                                let re_in = b.load_typed(re_ptr, elem.clone());
                                let im_in = b.load_typed(im_ptr, elem.clone());

                                let re2 = b.fmul(re_in, re_in);
                                let im2 = b.fmul(im_in, im_in);
                                let r2 = b.fadd(re2, im2);
                                let r = b.fsqrt(r2);
                                let two = if fw == FloatWidth::F64 {
                                    b.const_f64(2.0)
                                } else {
                                    b.const_f32(2.0)
                                };
                                let zero_f = if fw == FloatWidth::F64 {
                                    b.const_f64(0.0)
                                } else {
                                    b.const_f32(0.0)
                                };
                                let r_plus_a = b.fadd(r, re_in);
                                let r_minus_a = b.fsub(r, re_in);
                                let half_plus = b.fdiv(r_plus_a, two);
                                let half_minus = b.fdiv(r_minus_a, two);
                                // Clamp to >=0 to absorb tiny negative values
                                // from rounding (mathematically these halves
                                // are non-negative but FP rounding can flip
                                // them slightly negative).
                                let half_plus_pos = b.fcmp(CmpOp::Ge, half_plus, zero_f);
                                let half_plus_safe = b.select(half_plus_pos, half_plus, zero_f);
                                let half_minus_pos = b.fcmp(CmpOp::Ge, half_minus, zero_f);
                                let half_minus_safe =
                                    b.select(half_minus_pos, half_minus, zero_f);
                                let re_out = b.fsqrt(half_plus_safe);
                                let im_mag = b.fsqrt(half_minus_safe);
                                let im_neg = b.fneg(im_mag);
                                let b_nonneg = b.fcmp(CmpOp::Ge, im_in, zero_f);
                                let im_out = b.select(b_nonneg, im_mag, im_neg);

                                let out = b.alloca(arr_ty);
                                let out_re =
                                    b.gep(out, vec![zero_off], IrType::Int(IntWidth::I8));
                                let out_im =
                                    b.gep(out, vec![lane_bytes], IrType::Int(IntWidth::I8));
                                b.store(re_out, out_re);
                                b.store(im_out, out_im);
                                return out;
                            }
                        }
                    }
                }

                // Keyword-argument reordering for function calls
                // (symmetric with the Stmt::Call path). Binds by name
                // when the callee's arg_order is resolvable.
                //
                // resolution_arg_vals is ONLY consumed when we need to
                // pick a specific procedure out of a NamedInterface or
                // route a procedure-pointer call by signature.  For an
                // ordinary user-procedure call (`waterr32(key(0:))`)
                // resolve_generic_call_actuals returns None on its
                // first line (no NamedInterface candidates) and never
                // touches the actual_vals slice — so the work spent
                // lowering each arg into a typed null / real value is
                // discarded.  For a section-shaped arg that lowering
                // emits a 384-byte descriptor + memset + afs_create_section
                // every time, which compounds badly inside nested
                // intrinsic chains (stdlib_hash_32bit_water:
                // `ieor(waterr32(key(i:)), waterp1)` lowers `key(i:)`
                // a second time once it pops out of the resolution
                // probe, then a third time when the eventual user-
                // call reference lowering fills ref_arg_vals).  Skip
                // the resolution probe when no caller can use it.
                let original_args = args;
                let needs_generic_resolution =
                    has_named_interface || procptr_target.is_some();
                let resolution_arg_vals: Vec<ValueId> = if needs_generic_resolution {
                    original_args
                        .iter()
                        .map(|arg| match &arg.value {
                            crate::ast::expr::SectionSubscript::Element(e) => {
                                generic_dispatch_probe_value(
                                    b,
                                    locals,
                                    e,
                                    st,
                                    type_layouts,
                                    internal_funcs,
                                    contained_host_refs,
                                    descriptor_params,
                                )
                            }
                            _ => b.const_i32(0),
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let fallback_to_structure_ctor = has_named_interface
                    && type_layouts.is_some_and(|tl| tl.get(&key).is_some())
                    && resolve_generic_call_actuals(
                        st,
                        b,
                        Some(locals),
                        &key,
                        original_args,
                        &resolution_arg_vals,
                        type_layouts,
                    )
                    .is_none();

                if !has_named_interface || fallback_to_structure_ctor {
                    if let Some(tmp) = lower_structure_constructor_expr(
                        b,
                        locals,
                        &key,
                        args,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return tmp;
                    }
                }

                // F2018 §16.9.96/130/79/178/176/61: HUGE/TINY/EPSILON/PRECISION/
                // RANGE/DIGITS are numeric inquiry intrinsics whose result depends
                // ONLY on the type/kind of the argument, not its value. The
                // standard `lower_intrinsic` path lowers each argument to a
                // ValueId first; for an array actual that's the descriptor
                // pointer, which doesn't reveal the element kind, so the match
                // falls through and a bare `bl _huge` external is emitted.
                // Resolve from the AST type of the actual instead. Surfaced in
                // stdlib_sorting `if (array_size > huge(index))` where `index`
                // is `integer(int_index_low)` declared with shape `(0:)`.
                if !has_named_interface
                    && matches!(
                        key.as_str(),
                        "huge" | "tiny" | "epsilon" | "precision" | "range" | "digits"
                    )
                {
                    if let Some(arg) = args.first() {
                        if let crate::ast::expr::SectionSubscript::Element(arg_expr) = &arg.value {
                            if let Some(elem_ty) = ast_arg_element_ir_type(
                                arg_expr,
                                locals,
                                st,
                                type_layouts,
                            ) {
                                if let Some(v) = lower_numeric_inquiry_constant(b, &key, &elem_ty)
                                {
                                    return v;
                                }
                            }
                        }
                    }
                }

                // Try intrinsic lowering first (intrinsics use values, not references).
                //
                // lower_intrinsic only matches a fixed set of names — for
                // anything else it returns None and intrinsic_arg_vals is
                // discarded.  Lowering each arg unconditionally costs a
                // section-descriptor materialization per array section
                // arg, which compounded into a 26 GB compile peak on
                // stdlib_hash_32bit_water (each `ieor(waterr32(key(i:)),
                // waterp1)` lowered `waterr32(key(i:))` once for the
                // intrinsic probe, then again for ref_arg_vals).  Gate
                // the work behind the intrinsic-name check so user-call
                // sites pay no probe cost.  For a name shadowed by a
                // user generic the explicit check earlier already routes
                // to the structure ctor / generic-resolve path before
                // we get here.
                let intrinsic_result = if crate::sema::validate::is_intrinsic_name(&key) {
                    let intrinsic_arg_slots =
                        reorder_args_by_keyword_slots(original_args, &key, st);
                    let intrinsic_args: Vec<crate::ast::expr::Argument> =
                        intrinsic_arg_slots.iter().flatten().cloned().collect();
                    let intrinsic_arg_vals: Vec<ValueId> = intrinsic_args
                        .iter()
                        .map(|a| match &a.value {
                            crate::ast::expr::SectionSubscript::Element(e) => {
                                generic_dispatch_probe_value(
                                    b,
                                    locals,
                                    e,
                                    st,
                                    type_layouts,
                                    internal_funcs,
                                    contained_host_refs,
                                    descriptor_params,
                                )
                            }
                            _ => b.const_i32(0),
                        })
                        .collect();
                    super::intrinsic::lower_intrinsic(b, &key, &intrinsic_arg_vals)
                } else {
                    None
                };
                if !has_named_interface {
                    if let Some(result) = intrinsic_result {
                        let coerced = operator_expr_type_info(expr, Some(locals), st, type_layouts)
                            .and_then(|ti| match ti {
                                crate::sema::symtab::TypeInfo::Integer { .. }
                                | crate::sema::symtab::TypeInfo::Real { .. }
                                | crate::sema::symtab::TypeInfo::DoublePrecision
                                | crate::sema::symtab::TypeInfo::Complex { .. }
                                | crate::sema::symtab::TypeInfo::Logical { .. } => {
                                    Some(type_info_to_ir_type(&ti))
                                }
                                _ => None,
                            })
                            .map(|target_ty| {
                                // Complex intrinsics materialize pointer-backed
                                // two-lane buffers. Keep that pointer form at the
                                // expression boundary so downstream complex
                                // assignment/call paths can memcpy from the
                                // buffer address instead of treating lane 0 as a
                                // fake pointer.
                                let result_ty = b.func().value_type(result);
                                if is_complex_ty(&target_ty)
                                    && matches!(
                                        result_ty,
                                        Some(IrType::Ptr(ref inner))
                                            if inner.as_ref() == &target_ty
                                    )
                                {
                                    result
                                } else {
                                    coerce_to_type(b, result, &target_ty)
                                }
                            })
                            .unwrap_or(result);
                        return coerced;
                    }
                }

                // Resolve generic interface names to specific procedures.
                // For a NamedInterface callee, failing to resolve means
                // the call is ill-typed (wrong arity, wrong kind, or no
                // matching specific). Emit a compile-time diagnostic
                // instead of silently falling back to the generic name,
                // which would either mismatch the callee ABI or produce
                // an unresolved link-time symbol.
                let resolved_generic = if procptr_target.is_some() {
                    None
                } else {
                    resolve_generic_call_actuals(
                        st,
                        b,
                        Some(locals),
                        &key,
                        original_args,
                        &resolution_arg_vals,
                        type_layouts,
                    )
                };
                let (call_name, callee_key) = if procptr_target.is_some() {
                    (String::new(), signature_key.clone())
                } else if let Some(candidate) = resolved_generic.as_ref() {
                    resolved_symbol_call_target_for_candidate(st, candidate)
                } else {
                    if let Some(result) =
                        lower_pointer_intrinsic(b, locals, &key, args, st, type_layouts)
                    {
                        return result;
                    }

                    if let Some(result) =
                        lower_scalar_allocated_intrinsic(b, locals, &key, args, st, type_layouts)
                    {
                        return result;
                    }

                    if let Some(result) = lower_logical_reduction_intrinsic_ast(
                        b,
                        &key,
                        args,
                        locals,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return result;
                    }

                    if let Some(result) = lower_array_intrinsic(
                        b,
                        locals,
                        &key,
                        args,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return result;
                    }

                    if let Some(result) = lower_char_intrinsic(
                        b,
                        &key,
                        args,
                        locals,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    ) {
                        return result;
                    }

                    if let Some(result) = intrinsic_result {
                        return result;
                    }
                    if let Some(specifics) = named_interface_specifics(st, &key) {
                        eprintln!(
                            "armfortas: error: {}:{}: no specific procedure of generic '{}' matches the actual arguments; candidates: [{}]",
                            expr.span.start.line,
                            expr.span.start.col,
                            name,
                            specifics.join(", "),
                        );
                        let _ = std::io::stderr().flush();
                        std::process::exit(1);
                    }
                    resolved_symbol_call_target(st, &key, name)
                };
                let arg_slots = reorder_args_by_keyword_slots(
                    original_args,
                    if procptr_target.is_some() {
                        &signature_key
                    } else {
                        &callee_key
                    },
                    st,
                );
                let arg_slots = if let Some(candidate) = resolved_generic.as_ref() {
                    reorder_args_for_specific_candidate(st, candidate, original_args)
                        .unwrap_or(arg_slots)
                } else {
                    arg_slots
                };
                let abi_lookup_keys = procedure_abi_lookup_keys(
                    st,
                    &[call_name.as_str(), &callee_key, &signature_key, &key],
                );
                let abi_primary_key = abi_lookup_keys
                    .first()
                    .map(String::as_str)
                    .unwrap_or(callee_key.as_str());
                if procptr_target.is_none() {
                    if let Some(hidden_abi) = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_hidden_result_abi(st, k)
                    }) {
                        if let Some(bytes) = hidden_result_temp_bytes_for_callee(
                            st,
                            type_layouts,
                            &abi_lookup_keys,
                            hidden_abi,
                        ) {
                            // For ComplexBuffer ABI returns, type the temp
                            // as `[f32 x 2]` / `[f64 x 2]` so the call's
                            // result is `Ptr<[fN x 2]>` and `is_complex_ty`
                            // sees it as a complex value.  Without this,
                            // the buffer is `Ptr<[i8 x 8/16]>`, the binop
                            // path's `is_complex_ty(&lty) || is_complex_ty(&rty)`
                            // check fails for `complex_local - complex_call(...)`,
                            // and the int/float promotion path then
                            // emits `fsub %ptr<[i8 x 8]>` against the buffer
                            // pointer — IR-verify rejects with `float op
                            // has non-float operand : ptr<[i8 x 8]>`.
                            // Surfaced in stdlib_lapack_solve_chol_comp's
                            // CPOTF2/ZPOTF2 routines:
                            //   `ajj = real( real(a(j,j),sp) - cdotc(...), sp)`.
                            let alloca_ty = if hidden_abi == HiddenResultAbi::ComplexBuffer {
                                let fw = if bytes == 16 {
                                    FloatWidth::F64
                                } else {
                                    FloatWidth::F32
                                };
                                IrType::Array(Box::new(IrType::Float(fw)), 2)
                            } else {
                                IrType::Array(Box::new(IrType::Int(IntWidth::I8)), bytes)
                            };
                            let desc = b.alloca(alloca_ty);
                            let zero_i32 = b.const_i32(0);
                            let size = b.const_i64(bytes as i64);
                            b.call(
                                FuncRef::External("memset".into()),
                                vec![desc, zero_i32, size],
                                IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            );
                            emit_named_function_call(
                                b,
                                locals,
                                st,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                                name,
                                original_args,
                                Some(desc),
                                true,
                                IrType::Void,
                            );
                            return desc;
                        }
                    }
                }
                let callee_value_args =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_value_arg_mask(st, k));
                let callee_descriptor_args = first_procedure_lookup(&abi_lookup_keys, |k| {
                    descriptor_params.and_then(|m| cached_param_mask_for_lookup(st, m, k))
                });
                let callee_string_descriptor_args = first_procedure_lookup(&abi_lookup_keys, |k| {
                    callee_string_descriptor_arg_mask(st, k)
                });
                let callee_bind_c_char_args = first_procedure_lookup(&abi_lookup_keys, |k| {
                    callee_bind_c_char_arg_mask(st, k)
                });
                let callee_pointer_args =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_pointer_arg_mask(st, k));
                let callee_class_args =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_class_arg_mask(st, k));
                let opt_flags =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_optional_arg_mask(st, k));
                let mut ref_arg_vals: Vec<ValueId> = Vec::with_capacity(arg_slots.len());
                for (i, slot) in arg_slots.iter().enumerate() {
                    let is_value = callee_value_args
                        .as_ref()
                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    let wants_descriptor = callee_descriptor_args
                        .as_ref()
                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    let wants_string_descriptor = callee_string_descriptor_args
                        .as_ref()
                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    let wants_bind_c_char = callee_bind_c_char_args
                        .as_ref()
                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    let wants_descriptor = wants_descriptor && !wants_bind_c_char;
                    let wants_polymorphic_descriptor = wants_descriptor
                        && callee_class_args
                            .as_ref()
                            .map(|mask| mask.get(i).copied().unwrap_or(false))
                            .unwrap_or(false);
                    let wants_string_descriptor = wants_string_descriptor && !wants_bind_c_char;
                    let wants_pointer = callee_pointer_args
                        .as_ref()
                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                        .unwrap_or(false);
                    let value = match slot {
                        Some(arg) => match &arg.value {
                            crate::ast::expr::SectionSubscript::Element(e) => {
                                if is_value && wants_bind_c_char {
                                    lower_bind_c_char_value_arg(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    )
                                } else if is_value {
                                    let raw = lower_expr_full(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    );
                                    coerce_value_call_arg(b, st, abi_primary_key, i, raw)
                                } else if wants_descriptor {
                                    lower_arg_descriptor(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        wants_polymorphic_descriptor,
                                    )
                                } else if wants_string_descriptor {
                                    lower_arg_string_descriptor(b, locals, e, st, type_layouts)
                                } else if wants_bind_c_char {
                                    lower_bind_c_char_arg_raw(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    )
                                } else if wants_pointer {
                                    lower_pointer_dummy_actual(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    )
                                    .unwrap_or_else(|| {
                                        lower_arg_by_ref_full(
                                            b,
                                            locals,
                                            e,
                                            st,
                                            type_layouts,
                                            internal_funcs,
                                            contained_host_refs,
                                            descriptor_params,
                                        )
                                    })
                                } else {
                                    lower_arg_by_ref_full(
                                        b,
                                        locals,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    )
                                }
                            }
                            _ => b.const_i32(0),
                        },
                        None => missing_optional_call_arg(b, st, abi_primary_key, i, is_value),
                    };
                    ref_arg_vals.push(value);
                }
                if let Some(opt_flags) = opt_flags {
                    for flag in opt_flags.iter().skip(ref_arg_vals.len()) {
                        if *flag {
                            ref_arg_vals.push(b.const_i64(0));
                        }
                    }
                }
                let callee_char_len_star_args =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_char_len_star_mask(st, k));

                if let Some(cls_flags) = &callee_char_len_star_args {
                    for (i, flag) in cls_flags.iter().enumerate() {
                        if !*flag || i >= arg_slots.len() {
                            continue;
                        }
                        if let Some(arg) = &arg_slots[i] {
                            if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                ref_arg_vals.push(
                                    actual_char_arg_runtime_len(
                                        b,
                                        locals,
                                        None,
                                        e,
                                        st,
                                        type_layouts,
                                        internal_funcs,
                                        contained_host_refs,
                                        descriptor_params,
                                    )
                                    .unwrap_or_else(|| b.const_i64(0)),
                                );
                            } else {
                                ref_arg_vals.push(b.const_i64(0));
                            }
                        } else {
                            ref_arg_vals.push(b.const_i64(0));
                        }
                    }
                }

                // Host-association closure-passing ABI: append trailing
                // pointer args for each host-local the callee references.
                // Prefer the generic-resolved name's map entry; fall back
                // to the unresolved key so calls that don't go through
                // generic dispatch still thread host vars.
                let closure_key = if contained_host_refs
                    .map(|m| m.contains_key(&callee_key))
                    .unwrap_or(false)
                {
                    &callee_key
                } else {
                    &key
                };
                if procptr_target.is_none() {
                    append_host_closure_args_raw(
                        b,
                        locals,
                        contained_host_refs,
                        closure_key,
                        &mut ref_arg_vals,
                    );
                }

                // Look up callee return type from symbol table.
                let ret_ty =
                    first_procedure_lookup(&abi_lookup_keys, |k| callee_return_ir_type(st, k))
                        .unwrap_or(IrType::Int(IntWidth::I32));
                let func_ref = if let Some((target, _)) = procptr_target {
                    FuncRef::Indirect(target)
                } else {
                    same_unit_func_ref(
                        st,
                        b.func().name.as_str(),
                        internal_funcs,
                        &[&callee_key, &signature_key, &key],
                        call_name,
                    )
                };
                let call_result = b.call(func_ref, ref_arg_vals, ret_ty);
                if let Some(tl) = type_layouts {
                    if let Some(type_name) = first_procedure_lookup(&abi_lookup_keys, |k| {
                        callee_return_stabilized_derived_type_name(st, k)
                    }) {
                        return stabilize_derived_call_result(b, tl, &type_name, call_result);
                    }
                }
                call_result
            } else if let Expr::ComponentAccess { base, component } = &callee.node {
                if let Some(tl) = type_layouts {
                    if let Some(Some(result)) = emit_polymorphic_component_bound_dispatch(
                        b,
                        locals,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        None,
                        descriptor_params,
                        callee.span,
                        base,
                        component,
                        args,
                        None,
                    ) {
                        return result;
                    }
                    if let Some(info) = component_intrinsic_local_info(b, locals, callee, st, tl) {
                        let has_range = args.iter().any(|a| {
                            matches!(a.value, crate::ast::expr::SectionSubscript::Range { .. })
                        });
                        if has_range {
                            return lower_array_section(b, locals, &info, args, st, type_layouts);
                        }
                        return lower_array_element(b, locals, &info, args, st, type_layouts);
                    }

                    if let Some((obj_addr, type_name)) =
                        resolve_component_base_for_method(b, locals, base, st, tl)
                    {
                        if let Some(layout) = tl.get(&type_name) {
                            let bp_opt = resolved_bound_proc_for_call(
                                b,
                                locals,
                                st,
                                layout,
                                component,
                                args,
                                type_layouts,
                                internal_funcs,
                                contained_host_refs,
                                descriptor_params,
                            )
                            .or_else(|| layout.bound_proc(component));
                            // Procedure-pointer component (not a TBP):
                            // lower as an indirect call.  Common in
                            // stdlib_hashmaps where `map % hasher(key)`
                            // dispatches through a proc-pointer field.
                            if bp_opt.is_none() {
                                if let Some((target_ptr, closure_args, signature_key)) =
                                    procedure_pointer_component_call_target(
                                        b, locals, callee, st, tl,
                                    )
                                {
                                    let abi_lookup_keys =
                                        procedure_abi_lookup_keys(st, &[&signature_key]);
                                    let ret_ty = first_procedure_lookup(
                                        &abi_lookup_keys,
                                        |k| callee_return_ir_type(st, k),
                                    )
                                    .unwrap_or(IrType::Int(IntWidth::I32));
                                    // Honor the abstract-interface descriptor
                                    // mask when forwarding actuals.  When an
                                    // actual is a descriptor-backed array
                                    // (assumed-shape dummy / allocatable /
                                    // pointer), pass its full descriptor —
                                    // the callee declared via the abstract
                                    // interface must take a descriptor for
                                    // assumed-shape parameters. Without this,
                                    // `lower_arg_by_ref_full` returned the
                                    // base data pointer (loaded out of the
                                    // descriptor) and the callee tried to
                                    // read rank/dims out of the array elements'
                                    // bytes — stdlib's iterative solvers all
                                    // dispatch dot_product/matvec through a
                                    // procedure-pointer field of a class arg
                                    // this way and SEGV'd deep inside
                                    // stdlib_dot_product_dp on an indirect
                                    // load whose target was the array's first
                                    // f64 (1.0 = bits 0x3ff0...).
                                    let callee_descriptor_args =
                                        first_procedure_lookup(&abi_lookup_keys, |k| {
                                            descriptor_params
                                                .and_then(|m| cached_param_mask_for_lookup(st, m, k))
                                        });
                                    let mut arg_vals: Vec<ValueId> =
                                        Vec::with_capacity(args.len());
                                    for (i, arg) in args.iter().enumerate() {
                                        if let crate::ast::expr::SectionSubscript::Element(
                                            e,
                                        ) = &arg.value
                                        {
                                            let mask_says_descriptor = callee_descriptor_args
                                                .as_ref()
                                                .map(|mask| mask.get(i).copied().unwrap_or(false))
                                                .unwrap_or(false);
                                            // Fallback: if the lookup missed
                                            // (abstract iface not in
                                            // descriptor_params), inspect the
                                            // actual itself. A descriptor-
                                            // backed local must be passed by
                                            // descriptor regardless.
                                            let actual_is_descriptor_backed =
                                                actual_is_descriptor_array(locals, e);
                                            let wants_descriptor = mask_says_descriptor
                                                || actual_is_descriptor_backed;
                                            let v = if wants_descriptor {
                                                lower_arg_descriptor(
                                                    b,
                                                    locals,
                                                    e,
                                                    st,
                                                    type_layouts,
                                                    false,
                                                )
                                            } else {
                                                lower_arg_by_ref_full(
                                                    b,
                                                    locals,
                                                    e,
                                                    st,
                                                    type_layouts,
                                                    internal_funcs,
                                                    contained_host_refs,
                                                    descriptor_params,
                                                )
                                            };
                                            arg_vals.push(v);
                                        }
                                    }
                                    arg_vals.extend(closure_args);
                                    return b.call(
                                        FuncRef::Indirect(target_ptr),
                                        arg_vals,
                                        ret_ty,
                                    );
                                }
                            }
                            let bp = bp_opt.unwrap_or_else(|| {
                                fail_unmatched_bound_proc_resolution(callee.span, layout, component)
                            });
                                let target = bp.target_name.clone();
                                let target_key = abi_key_for_link_name(st, &target)
                                    .unwrap_or_else(|| bp.abi_name.clone());
                                let call_name = target.clone();
                                let nopass = bp.nopass;
                                let arg_slots = reorder_args_by_keyword_slots_with_formal_skip(
                                    args,
                                    &target_key,
                                    st,
                                    if nopass { 0 } else { 1 },
                                );
                                let abi_lookup_keys = procedure_abi_lookup_keys(
                                    st,
                                    &[call_name.as_str(), &target_key],
                                );
                                let abi_primary_key = abi_lookup_keys
                                    .first()
                                    .map(String::as_str)
                                    .unwrap_or(target_key.as_str());
                                if let Some(hidden_abi) =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_hidden_result_abi(st, k)
                                    })
                                {
                                    if let Some(bytes) = hidden_result_temp_bytes_for_callee(
                                        st,
                                        type_layouts,
                                        &abi_lookup_keys,
                                        hidden_abi,
                                    ) {
                                        // Type ComplexBuffer ABI temps as
                                        // [fN x 2] so downstream `is_complex_ty`
                                        // checks recognize the result; same
                                        // motivation as the Name-callee path.
                                        let alloca_ty = if hidden_abi
                                            == HiddenResultAbi::ComplexBuffer
                                        {
                                            let fw = if bytes == 16 {
                                                FloatWidth::F64
                                            } else {
                                                FloatWidth::F32
                                            };
                                            IrType::Array(Box::new(IrType::Float(fw)), 2)
                                        } else {
                                            IrType::Array(
                                                Box::new(IrType::Int(IntWidth::I8)),
                                                bytes,
                                            )
                                        };
                                        let desc = b.alloca(alloca_ty);
                                        let zero_i32 = b.const_i32(0);
                                        let size = b.const_i64(bytes as i64);
                                        b.call(
                                            FuncRef::External("memset".into()),
                                            vec![desc, zero_i32, size],
                                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                        );
                                        emit_bound_function_call(
                                            b,
                                            locals,
                                            st,
                                            type_layouts,
                                            internal_funcs,
                                            contained_host_refs,
                                            descriptor_params,
                                            callee.span,
                                            base,
                                            component,
                                            args,
                                            Some(desc),
                                            IrType::Void,
                                        );
                                        return desc;
                                    }
                                }
                                let callee_value_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_value_arg_mask(st, k)
                                    });
                                let callee_descriptor_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        descriptor_params
                                            .and_then(|m| cached_param_mask_for_lookup(st, m, k))
                                    });
                                let callee_string_descriptor_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_string_descriptor_arg_mask(st, k)
                                    });
                                let callee_bind_c_char_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_bind_c_char_arg_mask(st, k)
                                    });
                                let callee_pointer_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_pointer_arg_mask(st, k)
                                    });
                                let opt_flags = first_procedure_lookup(&abi_lookup_keys, |k| {
                                    callee_optional_arg_mask(st, k)
                                });
                                let callee_char_len_star_args =
                                    first_procedure_lookup(&abi_lookup_keys, |k| {
                                        callee_char_len_star_mask(st, k)
                                    });

                                let mut call_args = Vec::with_capacity(arg_slots.len() + 1);
                                for (i, slot) in arg_slots.iter().enumerate() {
                                    if !nopass && i == 0 {
                                        let wants_bind_c_char = callee_bind_c_char_args
                                            .as_ref()
                                            .map(|mask| mask.first().copied().unwrap_or(false))
                                            .unwrap_or(false);
                                        let wants_descriptor = callee_descriptor_args
                                            .as_ref()
                                            .map(|mask| mask.first().copied().unwrap_or(false))
                                            .unwrap_or(false)
                                            && !wants_bind_c_char;
                                        call_args.push(if wants_descriptor {
                                            lower_arg_descriptor(
                                                b,
                                                locals,
                                                base,
                                                st,
                                                type_layouts,
                                                false,
                                            )
                                        } else {
                                            obj_addr
                                        });
                                        continue;
                                    }
                                    let is_value = callee_value_args
                                        .as_ref()
                                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                                        .unwrap_or(false);
                                    let wants_descriptor = callee_descriptor_args
                                        .as_ref()
                                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                                        .unwrap_or(false);
                                    let wants_string_descriptor = callee_string_descriptor_args
                                        .as_ref()
                                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                                        .unwrap_or(false);
                                    let wants_bind_c_char = callee_bind_c_char_args
                                        .as_ref()
                                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                                        .unwrap_or(false);
                                    let wants_descriptor = wants_descriptor && !wants_bind_c_char;
                                    let wants_string_descriptor =
                                        wants_string_descriptor && !wants_bind_c_char;
                                    let wants_pointer = callee_pointer_args
                                        .as_ref()
                                        .map(|mask| mask.get(i).copied().unwrap_or(false))
                                        .unwrap_or(false);
                                    let value = match slot {
                                        Some(arg) => match &arg.value {
                                            crate::ast::expr::SectionSubscript::Element(e) => {
                                                if is_value && wants_bind_c_char {
                                                    lower_bind_c_char_value_arg(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    )
                                                } else if is_value {
                                                    let raw = lower_expr_full(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    );
                                                    coerce_value_call_arg(
                                                        b,
                                                        st,
                                                        abi_primary_key,
                                                        i,
                                                        raw,
                                                    )
                                                } else if wants_descriptor {
                                                    lower_arg_descriptor(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        false,
                                                    )
                                                } else if wants_string_descriptor {
                                                    lower_arg_string_descriptor(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                    )
                                                } else if wants_bind_c_char {
                                                    lower_bind_c_char_arg_raw(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    )
                                                } else if wants_pointer {
                                                    lower_pointer_dummy_actual(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    )
                                                    .unwrap_or_else(|| {
                                                        lower_arg_by_ref_full(
                                                            b,
                                                            locals,
                                                            e,
                                                            st,
                                                            type_layouts,
                                                            internal_funcs,
                                                            contained_host_refs,
                                                            descriptor_params,
                                                        )
                                                    })
                                                } else {
                                                    lower_arg_by_ref_full(
                                                        b,
                                                        locals,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    )
                                                }
                                            }
                                            _ => b.const_i32(0),
                                        },
                                        None => missing_optional_call_arg(
                                            b,
                                            st,
                                            abi_primary_key,
                                            i,
                                            is_value,
                                        ),
                                    };
                                    call_args.push(value);
                                }
                                if let Some(opt_flags) = opt_flags {
                                    for flag in opt_flags.iter().skip(call_args.len()) {
                                        if *flag {
                                            call_args.push(b.const_i64(0));
                                        }
                                    }
                                }
                                if let Some(cls_flags) = &callee_char_len_star_args {
                                    for (i, flag) in cls_flags.iter().enumerate() {
                                        if !*flag || i >= arg_slots.len() {
                                            continue;
                                        }
                                        if let Some(arg) = &arg_slots[i] {
                                            if let crate::ast::expr::SectionSubscript::Element(e) =
                                                &arg.value
                                            {
                                                call_args.push(
                                                    actual_char_arg_runtime_len(
                                                        b,
                                                        locals,
                                                        None,
                                                        e,
                                                        st,
                                                        type_layouts,
                                                        internal_funcs,
                                                        contained_host_refs,
                                                        descriptor_params,
                                                    )
                                                    .unwrap_or_else(|| b.const_i64(0)),
                                                );
                                            } else {
                                                call_args.push(b.const_i64(0));
                                            }
                                        } else {
                                            call_args.push(b.const_i64(0));
                                        }
                                    }
                                }

                                let ret_ty = first_procedure_lookup(&abi_lookup_keys, |k| {
                                    callee_return_ir_type(st, k)
                                })
                                .unwrap_or(IrType::Int(IntWidth::I32));
                                let call_result =
                                    b.call(FuncRef::External(call_name), call_args, ret_ty);
                                if let Some(tl) = type_layouts {
                                    if let Some(type_name) =
                                        callee_return_stabilized_derived_type_name(st, &target_key)
                                    {
                                        return stabilize_derived_call_result(
                                            b,
                                            tl,
                                            &type_name,
                                            call_result,
                                        );
                                    }
                                }
                                return call_result;
                        }
                    }
                    reject_unsupported_polymorphic_component_method_base(
                        callee.span,
                        base,
                        locals,
                        st,
                        tl,
                    );
                }
                b.const_i32(0)
            } else {
                b.const_i32(0)
            }
        }

        Expr::ComponentAccess { base, component } => {
            // F2008 §6.2: complex part designator. `c%re` of a complex(k)
            // value returns the real part as real(k); `c%im` returns the
            // imaginary part. Layout is two side-by-side f32/f64 lanes
            // (re at offset 0, im at offset kind-bytes). Without this
            // arm both designators fell through to the const_i32(0)
            // fallback, silently producing 0 for `start%re` etc.
            let lc_component = component.to_lowercase();
            if lc_component == "re" || lc_component == "im" {
                let base_ti = operator_expr_type_info(base, Some(locals), st, type_layouts);
                if let Some(crate::sema::symtab::TypeInfo::Complex { kind }) = base_ti {
                    let kind_bytes = kind.unwrap_or(4) as i64;
                    let elem_ty = if kind_bytes == 8 {
                        IrType::Float(FloatWidth::F64)
                    } else {
                        IrType::Float(FloatWidth::F32)
                    };
                    let base_addr = lower_expr_full(
                        b,
                        locals,
                        base,
                        st,
                        type_layouts,
                        internal_funcs,
                        contained_host_refs,
                        descriptor_params,
                    );
                    let off = if lc_component == "re" {
                        0
                    } else {
                        kind_bytes
                    };
                    let off_v = b.const_i64(off);
                    let lane_ptr = b.gep(base_addr, vec![off_v], IrType::Int(IntWidth::I8));
                    return b.load_typed(lane_ptr, elem_ty);
                }
            }
            if let Some(tl) = type_layouts {
                if let Expr::Name { name } = &base.node {
                    if let Some(value) =
                        lower_parameter_derived_component_const(b, st, tl, name, component)
                    {
                        return value;
                    }
                }
                // Common case: base is a Name or chained ComponentAccess.
                let resolved = resolve_component_base(b, locals, base, st, tl);
                // Inline case: base is a call that returns a derived type,
                // e.g. `add_t(a, b)%x`.  resolve_component_base doesn't
                // know how to lower a FunctionCall, so handle it here.
                // The call itself evaluates to a ptr<i8> pointing at the
                // result struct, so we can reuse it as the component
                // base address — but we still need the callee's return
                // type name to look up the layout.
                let resolved = resolved.or_else(|| {
                    if let Expr::FunctionCall { callee, args } = &base.node {
                        if let Expr::Name { name } = &callee.node {
                            if let Some(ret_type_name) = callee_return_derived_type_name(st, name) {
                                let base_ptr = lower_expr_full(
                                    b,
                                    locals,
                                    base,
                                    st,
                                    type_layouts,
                                    internal_funcs,
                                    contained_host_refs,
                                    descriptor_params,
                                );
                                return Some((base_ptr, ret_type_name));
                            }
                        }
                        if let Expr::ComponentAccess {
                            base: method_base,
                            component,
                        } = &callee.node
                        {
                            if let Some(tl) = type_layouts {
                                if let Some((_obj_addr, type_name)) =
                                    resolve_component_base_for_method(
                                        b,
                                        locals,
                                        method_base,
                                        st,
                                        tl,
                                    )
                                {
                                    if let Some(layout) = tl.get(&type_name) {
                                        let bp = resolved_bound_proc_for_call(
                                            b,
                                            locals,
                                            st,
                                            layout,
                                            component,
                                            args,
                                            type_layouts,
                                            internal_funcs,
                                            contained_host_refs,
                                            descriptor_params,
                                        )
                                        .or_else(|| layout.bound_proc(component));
                                        if let Some(bp) = bp {
                                            let target_key =
                                                abi_key_for_link_name(st, &bp.target_name)
                                                    .unwrap_or_else(|| bp.abi_name.clone());
                                            if let Some(ret_type_name) =
                                                callee_return_derived_type_name(st, &target_key)
                                            {
                                                let base_ptr = lower_expr_full(
                                                    b,
                                                    locals,
                                                    base,
                                                    st,
                                                    type_layouts,
                                                    internal_funcs,
                                                    contained_host_refs,
                                                    descriptor_params,
                                                );
                                                return Some((base_ptr, ret_type_name));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    None
                });
                if let Some((base_addr, type_name)) = resolved {
                    if let Some(layout) = tl.get(&type_name) {
                        if let Some(field) = layout.field(component) {
                            let offset = b.const_i64(field.offset as i64);
                            let field_ptr =
                                b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));

                            if is_deferred_char_component_field(field) {
                                let (ptr, _len) = load_string_descriptor_view(b, field_ptr);
                                return ptr;
                            }

                            if is_opaque_c_handle_type(&field.type_info) {
                                let ir_ty = type_info_to_ir_type(&field.type_info);
                                return b.load_typed(field_ptr, ir_ty);
                            }

                            if let crate::sema::symtab::TypeInfo::Derived(_) = &field.type_info {
                                if field.pointer {
                                    return b.load_typed(
                                        field_ptr,
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                                    );
                                }
                                // Non-pointer derived fields stay address-valued
                                // so chained access like x%inner%field can walk
                                // the inline storage directly.
                                return field_ptr;
                            }

                            if field.pointer {
                                let slot_ty =
                                    IrType::Ptr(Box::new(type_info_to_ir_type(&field.type_info)));
                                let pointee = b.load_typed(field_ptr, slot_ty);
                                return b
                                    .load_typed(pointee, type_info_to_ir_type(&field.type_info));
                            }

                            // Character fields: return the pointer to the inline
                            // character data, not a load. The data is stored
                            // inline in the struct, not behind a pointer.
                            if let crate::sema::symtab::TypeInfo::Character { .. } =
                                &field.type_info
                            {
                                return field_ptr;
                            }

                            // Complex fields follow the same address-valued
                            // convention as ordinary complex locals: callers
                            // expect a pointer to the inline [re, im] buffer,
                            // not the aggregate loaded by value. Retag the
                            // byte-addressed field pointer to the real complex
                            // storage type so intrinsic dispatch sees
                            // ptr<[f32/f64 x 2]> instead of ptr<i8>.
                            if let crate::sema::symtab::TypeInfo::Complex { .. } = &field.type_info
                            {
                                let addr = b.ptr_to_int(field_ptr);
                                return b.int_to_ptr(addr, type_info_to_ir_type(&field.type_info));
                            }

                            let ir_ty = type_info_to_ir_type(&field.type_info);
                            return b.load_typed(field_ptr, ir_ty);
                        }
                    } else {
                        eprintln!("warning: no field '{}' in type '{}'", component, type_name);
                    }
                }
            }
            b.const_i32(0) // fallback for unresolved component access
        }

        Expr::ArrayConstructor { values, .. } => {
            // Allocate a temporary stack array, store each literal
            // element into it, return the base pointer. Element
            // type is inferred from the first element's IR type
            // (or defaults to i32 for an empty constructor — rare
            // but legal). Implied-do values are slot-skipped by
            // store_ac_values_into; see the helper for details.
            //
            // The expression form is needed when an array literal
            // appears as a function argument or print item; the
            // assignment form (`a = [1,2,3]`) bypasses this and
            // routes through lower_array_assign for direct stores.
            let first_expr = first_array_constructor_expr(values);
            let first_ti =
                first_array_constructor_type_info(values, Some(locals), st, type_layouts);
            let elem_ty = match first_ti.as_ref() {
                Some(crate::sema::symtab::TypeInfo::Derived(name))
                | Some(crate::sema::symtab::TypeInfo::Class(name)) => type_layouts
                    .and_then(|tl| derived_storage_ir_type(name, tl))
                    .or_else(|| {
                        first_expr.map(|e| {
                            // Peek at the first element's type by lowering
                            // it on a scratch path. Rather than actually
                            // lower (and have to undo), approximate from
                            // the AST: integer literals → i32, real → f64,
                            // etc.
                            infer_const_expr_ty(&e.node, Some(locals), st)
                        })
                    })
                    .unwrap_or(IrType::Int(IntWidth::I32)),
                Some(ti) => type_info_to_ir_type(ti),
                None => first_expr
                    .map(|e| {
                        // Peek at the first element's type by lowering
                        // it on a scratch path. Rather than actually
                        // lower (and have to undo), approximate from
                        // the AST: integer literals → i32, real → f64,
                        // etc.
                        infer_const_expr_ty(&e.node, Some(locals), st)
                    })
                    .unwrap_or(IrType::Int(IntWidth::I32)),
            };
            let n = const_array_constructor_len(values).unwrap_or(values.len() as i64) as u64;
            let arr_ty = IrType::Array(Box::new(elem_ty.clone()), n.max(1));
            let buf = b.alloca(arr_ty);
            let derived_type = first_ti.as_ref().and_then(|ti| match ti {
                crate::sema::symtab::TypeInfo::Derived(name)
                | crate::sema::symtab::TypeInfo::Class(name) => Some(name.as_str()),
                _ => None,
            });
            if derived_type.is_some() {
                let zero32 = b.const_i32(0);
                let total_bytes = b.const_i64(ir_scalar_byte_size(&elem_ty) * n.max(1) as i64);
                b.call(
                    FuncRef::External("memset".into()),
                    vec![buf, zero32, total_bytes],
                    IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                );
            }
            store_ac_values_into(
                b,
                locals,
                buf,
                &elem_ty,
                derived_type,
                values,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            buf
        }

        Expr::ComplexLiteral { real, imag } => {
            // Complex numbers are stored as a 2-element float array on the stack.
            // Determine float width from the literal parts: if either uses a 'd'/'D'
            // exponent it's double precision (f64), otherwise single (f32).
            let is_double = |e: &crate::ast::expr::SpannedExpr| -> bool {
                if let Expr::RealLiteral { text, .. } = &e.node {
                    text.to_lowercase().contains('d')
                } else {
                    false
                }
            };
            let fw = if is_double(real) || is_double(imag) {
                FloatWidth::F64
            } else {
                FloatWidth::F32
            };
            let elem_ty = IrType::Float(fw);
            let elem_bytes = b.const_i64(if fw == FloatWidth::F64 { 8 } else { 4 });
            let arr_ty = IrType::Array(Box::new(elem_ty.clone()), 2);
            let buf = b.alloca(arr_ty);

            let real_raw = lower_expr_full(
                b,
                locals,
                real,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let imag_raw = lower_expr_full(
                b,
                locals,
                imag,
                st,
                type_layouts,
                internal_funcs,
                contained_host_refs,
                descriptor_params,
            );
            let real_val = coerce_to_type(b, real_raw, &elem_ty);
            let imag_val = coerce_to_type(b, imag_raw, &elem_ty);

            // Store real at byte offset 0, imag at byte offset elem_bytes.
            let zero = b.const_i64(0);
            let real_ptr = b.gep(buf, vec![zero], IrType::Int(IntWidth::I8));
            b.store(real_val, real_ptr);
            let imag_ptr = b.gep(buf, vec![elem_bytes], IrType::Int(IntWidth::I8));
            b.store(imag_val, imag_ptr);

            buf
        }
    }
}
