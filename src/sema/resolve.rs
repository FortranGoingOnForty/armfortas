//! Symbol resolution — walks the AST and populates symbol tables.
//!
//! First pass: collect declarations, create scopes, process USE/IMPLICIT.
//! This establishes the symbol table that type checking (Sprint 13) will use.

use crate::ast::unit::*;
use crate::ast::decl;
use crate::ast::decl::{SpannedDecl, Decl, TypeSpec, Attribute, OnlyItem};
use super::symtab::*;

/// Walk a list of program units and build the symbol table.
pub fn resolve_file(units: &[SpannedUnit]) -> Result<SymbolTable, SemaError> {
    let mut st = SymbolTable::new();

    // First pass: create module scopes so USE can find them.
    for unit in units {
        if let ProgramUnit::Module { name, .. } = &unit.node {
            st.push_scope(ScopeKind::Module(name.clone()));
            st.pop_scope();
        }
    }

    // Second pass: populate all scopes.
    for unit in units {
        resolve_unit(&mut st, unit)?;
    }

    Ok(st)
}

fn resolve_unit(st: &mut SymbolTable, unit: &SpannedUnit) -> Result<(), SemaError> {
    match &unit.node {
        ProgramUnit::Program { name, uses, imports: _, implicit, decls, body: _, contains } => {
            let scope_name = name.clone().unwrap_or_else(|| "<main>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_contains(st, contains)?;
            st.pop_scope();
        }
        ProgramUnit::Module { name, uses, imports: _, implicit, decls, contains } => {
            // Find the pre-created module scope.
            if let Some(mod_id) = st.find_module_scope(name) {
                // Re-enter the pre-created scope. We can't just push_scope again.
                // Instead, set current to the existing scope.
                let saved = st.current_scope();
                // Manually navigate to the module scope.
                // This is a bit hacky — ideally we'd have enter_scope(id).
                st.scopes[mod_id].parent = Some(saved);
                st.current = mod_id;

                process_uses(st, uses)?;
                process_implicit(st, implicit)?;
                process_decls(st, decls)?;
                process_contains(st, contains)?;

                st.current = saved;
            }
        }
        ProgramUnit::Subroutine { name, args, prefix: _, bind: _, uses, imports: _, implicit, decls, body: _, contains } => {
            st.push_scope(ScopeKind::Subroutine(name.clone()));
            // Define dummy arguments as symbols.
            for arg in args {
                if let DummyArg::Name(arg_name) = arg {
                    st.define(Symbol {
                        name: arg_name.clone(),
                        kind: SymbolKind::Variable,
                        type_info: None,
                        attrs: SymbolAttrs::default(),
                        defined_at: unit.span,
                        scope: st.current_scope(),
                    })?;
                }
            }
            process_uses(st, uses)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_contains(st, contains)?;
            st.pop_scope();
        }
        ProgramUnit::Function { name, args, result, return_type: _, bind: _, prefix: _, uses, imports: _, implicit, decls, body: _, contains } => {
            st.push_scope(ScopeKind::Function(name.clone()));
            for arg in args {
                if let DummyArg::Name(arg_name) = arg {
                    st.define(Symbol {
                        name: arg_name.clone(),
                        kind: SymbolKind::Variable,
                        type_info: None,
                        attrs: SymbolAttrs::default(),
                        defined_at: unit.span,
                        scope: st.current_scope(),
                    })?;
                }
            }
            // Define result variable.
            let result_name = result.as_deref().unwrap_or(name.as_str());
            st.define(Symbol {
                name: result_name.into(),
                kind: SymbolKind::Variable,
                type_info: None,
                attrs: SymbolAttrs::default(),
                defined_at: unit.span,
                scope: st.current_scope(),
            })?;
            process_uses(st, uses)?;
            process_implicit(st, implicit)?;
            process_decls(st, decls)?;
            process_contains(st, contains)?;
            st.pop_scope();
        }
        ProgramUnit::BlockData { name, uses, decls } => {
            let scope_name = name.clone().unwrap_or_else(|| "<block_data>".into());
            st.push_scope(ScopeKind::Program(scope_name));
            process_uses(st, uses)?;
            process_decls(st, decls)?;
            st.pop_scope();
        }
        ProgramUnit::InterfaceBlock { name: _, is_abstract: _, bodies } => {
            st.push_scope(ScopeKind::Interface);
            for body in bodies {
                match body {
                    InterfaceBody::Subprogram(sub) => resolve_unit(st, sub)?,
                    InterfaceBody::ModuleProcedure(_names) => {
                        // Module procedures are resolved by name during type checking.
                    }
                }
            }
            st.pop_scope();
        }
        _ => {}
    }
    Ok(())
}

fn process_uses(st: &mut SymbolTable, uses: &[SpannedDecl]) -> Result<(), SemaError> {
    for use_decl in uses {
        if let Decl::UseStmt { module, nature: _, renames, only } = &use_decl.node {
            if let Some(mod_scope) = st.find_module_scope(module) {
                if let Some(only_items) = only {
                    // USE ... ONLY: import specific names.
                    for item in only_items {
                        match item {
                            OnlyItem::Name(name) => {
                                st.add_use_association(UseAssociation {
                                    local_name: name.clone(),
                                    original_name: name.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                });
                            }
                            OnlyItem::Rename(rename) => {
                                st.add_use_association(UseAssociation {
                                    local_name: rename.local.clone(),
                                    original_name: rename.remote.clone(),
                                    source_scope: mod_scope,
                                    is_submodule_access: false,
                                });
                            }
                        }
                    }
                } else {
                    // USE without ONLY: import all public symbols.
                    let mod_symbols: Vec<(String, String)> = st.scope(mod_scope).symbols.iter()
                        .filter(|(_, sym)| sym.attrs.access != Access::Private)
                        .map(|(key, sym)| (sym.name.clone(), key.clone()))
                        .collect();
                    for (name, _key) in &mod_symbols {
                        st.add_use_association(UseAssociation {
                            local_name: name.clone(),
                            original_name: name.clone(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                        });
                    }
                    // Apply renames.
                    for rename in renames {
                        st.add_use_association(UseAssociation {
                            local_name: rename.local.clone(),
                            original_name: rename.remote.clone(),
                            source_scope: mod_scope,
                            is_submodule_access: false,
                        });
                    }
                }
            }
            // If module not found, it might be external — skip for now.
        }
    }
    Ok(())
}

fn process_implicit(st: &mut SymbolTable, implicit_stmts: &[SpannedDecl]) -> Result<(), SemaError> {
    for stmt in implicit_stmts {
        match &stmt.node {
            Decl::ImplicitNone { type_, external } => {
                st.set_implicit_none(*type_, *external);
            }
            Decl::ImplicitStmt { specs } => {
                for spec in specs {
                    let itype = match &spec.type_spec {
                        TypeSpec::Integer(_) => ImplicitType::Integer,
                        TypeSpec::Real(_) => ImplicitType::Real,
                        TypeSpec::DoublePrecision => ImplicitType::DoublePrecision,
                        TypeSpec::Complex(_) => ImplicitType::Complex,
                        TypeSpec::Logical(_) => ImplicitType::Logical,
                        TypeSpec::Character(_) => ImplicitType::Character,
                        _ => continue,
                    };
                    for (start, end) in &spec.ranges {
                        st.set_implicit_rule(*start, *end, itype);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn process_decls(st: &mut SymbolTable, decls: &[SpannedDecl]) -> Result<(), SemaError> {
    for decl in decls {
        match &decl.node {
            Decl::TypeDecl { type_spec, attrs, entities } => {
                let type_info = type_spec_to_info(type_spec);
                let sym_attrs = attrs_to_symbol_attrs(attrs, st.default_access(st.current_scope()));

                for entity in entities {
                    let kind = if sym_attrs.parameter {
                        SymbolKind::Parameter
                    } else {
                        SymbolKind::Variable
                    };
                    let key = entity.name.to_lowercase();
                    if st.scope(st.current_scope()).symbols.contains_key(&key) {
                        // Symbol already exists (e.g., dummy argument) — update type info.
                        let sym = st.scope_mut(st.current_scope()).symbols.get_mut(&key).unwrap();
                        sym.type_info = Some(type_info.clone());
                        sym.attrs = sym_attrs.clone();
                    } else {
                        st.define(Symbol {
                            name: entity.name.clone(),
                            kind,
                            type_info: Some(type_info.clone()),
                            attrs: sym_attrs.clone(),
                            defined_at: decl.span,
                            scope: st.current_scope(),
                        })?;
                    }
                }
            }
            Decl::DerivedTypeDef { name, .. } => {
                st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::DerivedType,
                    type_info: None,
                    attrs: SymbolAttrs::default(),
                    defined_at: decl.span,
                    scope: st.current_scope(),
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn process_contains(st: &mut SymbolTable, contains: &[SpannedUnit]) -> Result<(), SemaError> {
    for unit in contains {
        // Register the subprogram name in the current scope before descending.
        match &unit.node {
            ProgramUnit::Subroutine { name, .. } => {
                let _ = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Subroutine,
                    type_info: None,
                    attrs: SymbolAttrs::default(),
                    defined_at: unit.span,
                    scope: st.current_scope(),
                });
            }
            ProgramUnit::Function { name, .. } => {
                let _ = st.define(Symbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    type_info: None,
                    attrs: SymbolAttrs::default(),
                    defined_at: unit.span,
                    scope: st.current_scope(),
                });
            }
            _ => {}
        }
        resolve_unit(st, unit)?;
    }
    Ok(())
}

// ---- Helpers ----

fn type_spec_to_info(ts: &TypeSpec) -> TypeInfo {
    match ts {
        TypeSpec::Integer(_) => TypeInfo::Integer { kind: None },
        TypeSpec::Real(_) => TypeInfo::Real { kind: None },
        TypeSpec::DoublePrecision => TypeInfo::DoublePrecision,
        TypeSpec::Complex(_) => TypeInfo::Complex { kind: None },
        TypeSpec::DoubleComplex => TypeInfo::Complex { kind: Some(8) },
        TypeSpec::Logical(_) => TypeInfo::Logical { kind: None },
        TypeSpec::Character(_) => TypeInfo::Character { len: None, kind: None },
        TypeSpec::Type(name) => TypeInfo::Derived(name.clone()),
        TypeSpec::Class(name) => TypeInfo::Class(name.clone()),
        TypeSpec::ClassStar => TypeInfo::ClassStar,
        TypeSpec::TypeStar => TypeInfo::TypeStar,
    }
}

fn attrs_to_symbol_attrs(attrs: &[Attribute], default_access: Access) -> SymbolAttrs {
    let mut sa = SymbolAttrs { access: default_access, ..SymbolAttrs::default() };
    for attr in attrs {
        match attr {
            Attribute::Allocatable => sa.allocatable = true,
            Attribute::Pointer => sa.pointer = true,
            Attribute::Target => sa.target = true,
            Attribute::Optional => sa.optional = true,
            Attribute::Save => sa.save = true,
            Attribute::Parameter => sa.parameter = true,
            Attribute::Value => sa.value = true,
            Attribute::External => sa.external = true,
            Attribute::Intrinsic => sa.intrinsic = true,
            Attribute::Public => sa.access = Access::Public,
            Attribute::Private => sa.access = Access::Private,
            Attribute::Intent(intent) => {
                sa.intent = Some(match intent {
                    decl::Intent::In => Intent::In,
                    decl::Intent::Out => Intent::Out,
                    decl::Intent::InOut => Intent::InOut,
                });
            }
            _ => {}
        }
    }
    sa
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn resolve_source(src: &str) -> SymbolTable {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        resolve_file(&units).unwrap()
    }

    // ---- Integration tests ----

    #[test]
    fn simple_program_declarations() {
        let st = resolve_source("program test\n  implicit none\n  integer :: x, y\n  real :: z\nend program\n");
        // Should have x, y, z defined.
        // Navigate to the program scope.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.symbols.contains_key("x"));
        assert!(prog_scope.symbols.contains_key("y"));
        assert!(prog_scope.symbols.contains_key("z"));
    }

    #[test]
    fn implicit_none_enforced() {
        let st = resolve_source("program test\n  implicit none\n  integer :: x\nend program\n");
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.implicit_rules.none_type);
    }

    #[test]
    fn module_use_association() {
        let st = resolve_source("\
module mymod
  implicit none
  integer :: shared_var
end module

program main
  use mymod
  implicit none
end program
");
        // shared_var should be in the module scope.
        let mod_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Module(ref n) if n == "mymod")).unwrap();
        assert!(mod_scope.symbols.contains_key("shared_var"));

        // The program should have a USE association for shared_var.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(!prog_scope.use_associations.is_empty());
    }

    #[test]
    fn subroutine_with_args() {
        let st = resolve_source("subroutine foo(x, y)\n  real :: x, y\nend subroutine\n");
        let sub_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "foo")).unwrap();
        assert!(sub_scope.symbols.contains_key("x"));
        assert!(sub_scope.symbols.contains_key("y"));
    }

    #[test]
    fn function_result_variable() {
        let st = resolve_source("function square(x) result(y)\n  real :: x, y\n  y = x * x\nend function\n");
        let fn_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Function(ref n) if n == "square")).unwrap();
        assert!(fn_scope.symbols.contains_key("x"));
        assert!(fn_scope.symbols.contains_key("y"));
    }

    #[test]
    fn contains_creates_child_scope() {
        let st = resolve_source("\
program main
  implicit none
  integer :: x
contains
  subroutine inner()
    integer :: local_var
  end subroutine
end program
");
        // inner should be its own scope.
        let inner_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Subroutine(ref n) if n == "inner")).unwrap();
        assert!(inner_scope.symbols.contains_key("local_var"));

        // inner should be registered as a symbol in the program scope.
        let prog_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Program(_))).unwrap();
        assert!(prog_scope.symbols.contains_key("inner"));
    }

    #[test]
    fn derived_type_defined() {
        let st = resolve_source("module m\n  type :: mytype\n    integer :: field\n  end type\nend module\n");
        let mod_scope = st.scopes.iter().find(|s| matches!(s.kind, ScopeKind::Module(_))).unwrap();
        assert!(mod_scope.symbols.contains_key("mytype"));
        assert_eq!(mod_scope.symbols["mytype"].kind, SymbolKind::DerivedType);
    }
}
