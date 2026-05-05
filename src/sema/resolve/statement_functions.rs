//! F77 §15.4 statement-function detection.
//!
//! Walks the leading prologue of a procedure body looking for
//! definitions of the form `Name(p1, p2, ...) = expr` where `Name`
//! is a previously declared scalar variable in the current scope.
//! When found, the symbol is converted from `Variable` to
//! `Function`, its dummy parameter names are recorded, and the body
//! expression is parked in `SymbolTable::statement_functions` keyed
//! by `(scope_id, name)`.
//!
//! Detection stops at the first statement that does not match the
//! statement-function shape, so true executable code following the
//! definitions is not misclassified.
//!
//! Extracted from `core.rs` in Sprint 14.

use crate::sema::symtab::{
    ScopeId, StatementFunctionDef, SymbolKind, SymbolTable, TypeInfo,
};

pub(super) fn detect_statement_functions(
    st: &mut SymbolTable,
    scope_id: ScopeId,
    body: &[crate::ast::stmt::SpannedStmt],
) {
    use crate::ast::expr::{Expr, SectionSubscript};
    use crate::ast::stmt::Stmt;

    for stmt in body {
        let (target, value) = match &stmt.node {
            Stmt::Assignment { target, value } => (target, value),
            _ => break,
        };

        let (fname, args) = match &target.node {
            Expr::FunctionCall { callee, args } => match &callee.node {
                Expr::Name { name } => (name.clone(), args),
                _ => break,
            },
            _ => break,
        };

        let key = fname.to_ascii_lowercase();
        let result_type = {
            let scope = st.scope(scope_id);
            let Some(sym) = scope.symbols.get(&key) else {
                break;
            };
            if !matches!(sym.kind, SymbolKind::Variable) {
                break;
            }
            if !sym.attrs.array_spec.is_empty() {
                break;
            }
            let Some(ti) = sym.type_info.clone() else {
                break;
            };
            match ti {
                TypeInfo::Integer { .. }
                | TypeInfo::Real { .. }
                | TypeInfo::DoublePrecision
                | TypeInfo::Complex { .. }
                | TypeInfo::Logical { .. }
                | TypeInfo::Character { .. } => ti,
                _ => break,
            }
        };

        let mut params: Vec<String> = Vec::with_capacity(args.len());
        let mut all_params_ok = true;
        for a in args {
            if a.keyword.is_some() {
                all_params_ok = false;
                break;
            }
            let pname = match &a.value {
                SectionSubscript::Element(e) => match &e.node {
                    Expr::Name { name } => name.clone(),
                    _ => {
                        all_params_ok = false;
                        break;
                    }
                },
                _ => {
                    all_params_ok = false;
                    break;
                }
            };
            let pkey = pname.to_ascii_lowercase();
            let scope = st.scope(scope_id);
            let Some(psym) = scope.symbols.get(&pkey) else {
                all_params_ok = false;
                break;
            };
            if !matches!(psym.kind, SymbolKind::Variable | SymbolKind::Parameter) {
                all_params_ok = false;
                break;
            }
            if !psym.attrs.array_spec.is_empty() {
                all_params_ok = false;
                break;
            }
            params.push(pkey);
        }
        if !all_params_ok {
            break;
        }

        // Statement functions are pure (single-expression, no side
        // effects) and elemental (broadcast over array actuals
        // naturally) by construction. Mark them so PURE-procedure
        // callers (e.g. BLAS rotation routines) validate cleanly.
        {
            let scope = st.scope_mut(scope_id);
            if let Some(sym) = scope.symbols.get_mut(&key) {
                sym.kind = SymbolKind::Function;
                sym.arg_names = params.clone();
                sym.attrs.pure = true;
                sym.attrs.elemental = true;
            }
        }
        st.statement_functions.insert(
            (scope_id, key),
            StatementFunctionDef {
                params,
                body: value.clone(),
                result_type,
            },
        );
    }
}
