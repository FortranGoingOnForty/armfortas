//! AST → IR lowering.
//!
//! Walks the typed AST and produces SSA IR. Handles variable allocation,
//! expression evaluation, assignments, and runtime calls for I/O.

use crate::ast::unit::*;
use crate::ast::stmt::*;
use crate::ast::expr::{Expr, BinaryOp, UnaryOp};
use crate::ast::decl::{Decl, TypeSpec};
use crate::sema::symtab::SymbolTable;
use super::types::*;
use super::inst::*;
use super::builder::FuncBuilder;

use std::collections::HashMap;

/// Lower a file of program units to an IR module.
pub fn lower_file(
    units: &[SpannedUnit],
    st: &SymbolTable,
) -> Module {
    let mut module = Module::new("main".into());
    for unit in units {
        lower_unit(&mut module, unit, st);
    }
    module
}

fn lower_unit(module: &mut Module, unit: &SpannedUnit, st: &SymbolTable) {
    match &unit.node {
        ProgramUnit::Program { name, decls, body, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let mut func = Function::new(fname, vec![], IrType::Void);
            let mut locals = HashMap::new();

            {
                let mut b = FuncBuilder::new(&mut func);
                // Allocate local variables.
                for decl in decls {
                    if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
                        let ir_ty = lower_type_spec(type_spec);
                        for entity in entities {
                            let addr = b.alloca(ir_ty.clone());
                            locals.insert(entity.name.to_lowercase(), (addr, ir_ty.clone()));
                        }
                    }
                }
                // Lower body statements.
                for stmt in body {
                    lower_stmt(&mut b, &mut locals, stmt, st);
                }
                // Ensure termination.
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.ret_void();
                }
            }

            module.add_function(func);
        }
        ProgramUnit::Subroutine { name, decls, body, args, .. } => {
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    Some(Param {
                        name: n.clone(),
                        ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))), // default, refined by decls
                        id: ValueId(i as u32),
                    })
                } else { None }
            }).collect();
            let mut func = Function::new(name.clone(), params, IrType::Void);
            let mut locals = HashMap::new();

            // Map args to locals.
            for p in &func.params {
                locals.insert(p.name.to_lowercase(), (p.id, p.ty.clone()));
            }

            {
                let mut b = FuncBuilder::new(&mut func);
                for decl in decls {
                    if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
                        let ir_ty = lower_type_spec(type_spec);
                        for entity in entities {
                            let key = entity.name.to_lowercase();
                            locals.entry(key).or_insert_with(|| {
                                let addr = b.alloca(ir_ty.clone());
                                (addr, ir_ty.clone())
                            });
                        }
                    }
                }
                for stmt in body {
                    lower_stmt(&mut b, &mut locals, stmt, st);
                }
                if b.func().block(b.current_block()).terminator.is_none() {
                    b.ret_void();
                }
            }

            module.add_function(func);
        }
        ProgramUnit::Function { name, decls, body, args, result, return_type, .. } => {
            let ret_ty = return_type.as_ref()
                .map(lower_type_spec)
                .unwrap_or(IrType::Int(IntWidth::I32));
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    Some(Param {
                        name: n.clone(),
                        ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                        id: ValueId(i as u32),
                    })
                } else { None }
            }).collect();
            let mut func = Function::new(name.clone(), params, ret_ty.clone());
            let mut locals = HashMap::new();

            for p in &func.params {
                locals.insert(p.name.to_lowercase(), (p.id, p.ty.clone()));
            }

            {
                let mut b = FuncBuilder::new(&mut func);
                // Allocate result variable.
                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                let result_addr = b.alloca(ret_ty.clone());
                locals.insert(result_name.clone(), (result_addr, ret_ty.clone()));

                for decl in decls {
                    if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
                        let ir_ty = lower_type_spec(type_spec);
                        for entity in entities {
                            let key = entity.name.to_lowercase();
                            locals.entry(key).or_insert_with(|| {
                                let addr = b.alloca(ir_ty.clone());
                                (addr, ir_ty.clone())
                            });
                        }
                    }
                }
                for stmt in body {
                    lower_stmt(&mut b, &mut locals, stmt, st);
                }
                if b.func().block(b.current_block()).terminator.is_none() {
                    let rv = b.load(result_addr);
                    b.ret(Some(rv));
                }
            }

            module.add_function(func);
        }
        _ => {} // Module, BlockData, etc. handled later.
    }
}

/// Lower a Fortran type specifier to an IR type.
fn lower_type_spec(ts: &TypeSpec) -> IrType {
    match ts {
        TypeSpec::Integer(_) => IrType::Int(IntWidth::I32),
        TypeSpec::Real(_) => IrType::Float(FloatWidth::F32),
        TypeSpec::DoublePrecision => IrType::Float(FloatWidth::F64),
        TypeSpec::Complex(_) => {
            // Complex is a struct of two floats — simplified for now.
            IrType::Array(Box::new(IrType::Float(FloatWidth::F32)), 2)
        }
        TypeSpec::Logical(_) => IrType::Bool,
        TypeSpec::Character(_) => IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        _ => IrType::Int(IntWidth::I32), // fallback
    }
}

/// Lower a statement.
fn lower_stmt(
    b: &mut FuncBuilder,
    locals: &mut HashMap<String, (ValueId, IrType)>,
    stmt: &SpannedStmt,
    st: &SymbolTable,
) {
    match &stmt.node {
        Stmt::Assignment { target, value } => {
            let val = lower_expr(b, locals, value, st);
            if let Expr::Name { name } = &target.node {
                let key = name.to_lowercase();
                if let Some((addr, _ty)) = locals.get(&key) {
                    b.store(val, *addr);
                }
            }
        }
        Stmt::Print { items, .. } => {
            for item in items {
                let val = lower_expr(b, locals, item, st);
                let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
                let rt_func = match &ty {
                    IrType::Int(_) => RuntimeFunc::PrintInt,
                    IrType::Float(_) => RuntimeFunc::PrintReal,
                    IrType::Bool => RuntimeFunc::PrintLogical,
                    _ => RuntimeFunc::PrintInt,
                };
                b.runtime_call(rt_func, vec![val], IrType::Void);
            }
            b.runtime_call(RuntimeFunc::PrintNewline, vec![], IrType::Void);
        }
        Stmt::Call { callee, args } => {
            if let Expr::Name { name } = &callee.node {
                let arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
                        _ => b.const_i32(0), // placeholder
                    }
                }).collect();
                b.call(FuncRef::External(name.clone()), arg_vals, IrType::Void);
            }
        }
        Stmt::Stop { .. } => {
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::ErrorStop { .. } => {
            b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::Continue { .. } => {} // no-op
        _ => {} // other statements handled in Sprint 16
    }
}

/// Lower an expression to a ValueId.
#[allow(clippy::only_used_in_recursion)]
fn lower_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, (ValueId, IrType)>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> ValueId {
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => {
            let val: i64 = text.parse().unwrap_or(0);
            b.const_i32(val as i32)
        }
        Expr::RealLiteral { text, .. } => {
            let val: f64 = text.replace('d', "e").replace('D', "E").parse().unwrap_or(0.0);
            if text.to_lowercase().contains('d') {
                b.const_f64(val)
            } else {
                b.const_f32(val as f32)
            }
        }
        Expr::LogicalLiteral { value, .. } => {
            b.const_bool(*value)
        }
        Expr::StringLiteral { value, .. } => {
            b.const_string(value.as_bytes())
        }

        Expr::Name { name } => {
            let key = name.to_lowercase();
            if let Some((addr, _ty)) = locals.get(&key) {
                b.load(*addr)
            } else {
                // Implicit or unknown — generate undef.
                b.const_i32(0)
            }
        }

        Expr::BinaryOp { op, left, right } => {
            let lhs = lower_expr(b, locals, left, st);
            let rhs = lower_expr(b, locals, right, st);
            let lty = b.func().value_type(lhs).unwrap_or(IrType::Int(IntWidth::I32));

            match (op, &lty) {
                (BinaryOp::Add, IrType::Int(_)) => b.iadd(lhs, rhs),
                (BinaryOp::Add, IrType::Float(_)) => b.fadd(lhs, rhs),
                (BinaryOp::Sub, IrType::Int(_)) => b.isub(lhs, rhs),
                (BinaryOp::Sub, IrType::Float(_)) => b.fsub(lhs, rhs),
                (BinaryOp::Mul, IrType::Int(_)) => b.imul(lhs, rhs),
                (BinaryOp::Mul, IrType::Float(_)) => b.fmul(lhs, rhs),
                (BinaryOp::Div, IrType::Int(_)) => b.idiv(lhs, rhs),
                (BinaryOp::Div, IrType::Float(_)) => b.fdiv(lhs, rhs),
                (BinaryOp::Pow, _) => b.fpow(lhs, rhs),
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
                (BinaryOp::And, _) => b.and(lhs, rhs),
                (BinaryOp::Or, _) => b.or(lhs, rhs),
                _ => b.iadd(lhs, rhs), // fallback
            }
        }

        Expr::UnaryOp { op, operand } => {
            let val = lower_expr(b, locals, operand, st);
            let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
            match (op, &ty) {
                (UnaryOp::Minus, IrType::Int(_)) => b.ineg(val),
                (UnaryOp::Minus, IrType::Float(_)) => b.fneg(val),
                (UnaryOp::Plus, _) => val,
                (UnaryOp::Not, _) => b.not(val),
                _ => val,
            }
        }

        Expr::ParenExpr { inner } => lower_expr(b, locals, inner, st),

        Expr::FunctionCall { callee, args } => {
            // Could be intrinsic, user function, or array access.
            if let Expr::Name { name } = &callee.node {
                let arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
                        _ => b.const_i32(0),
                    }
                }).collect();
                let ret_ty = IrType::Int(IntWidth::I32); // default, should use type info
                b.call(FuncRef::External(name.clone()), arg_vals, ret_ty)
            } else {
                b.const_i32(0)
            }
        }

        _ => b.const_i32(0), // placeholder for unhandled expressions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::sema::resolve;
    use super::super::verify;
    use super::super::printer;

    fn lower_source(src: &str) -> Module {
        let tokens = Lexer::tokenize(src, 0).unwrap();
        let mut parser = Parser::new(&tokens);
        let units = parser.parse_file().unwrap();
        let st = resolve::resolve_file(&units).unwrap();
        lower_file(&units, &st)
    }

    fn lower_and_verify(src: &str) -> (Module, String) {
        let module = lower_source(src);
        let errs = verify::verify_module(&module);
        assert!(errs.is_empty(), "IR verification failed:\n{}\nIR:\n{}",
            errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"),
            printer::print_module(&module));
        let ir_text = printer::print_module(&module);
        (module, ir_text)
    }

    #[test]
    fn lower_integer_arithmetic() {
        let (module, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y, z
  x = 10
  y = 20
  z = x + y
end program
");
        assert_eq!(module.functions.len(), 1);
        assert!(ir.contains("const_int 10"));
        assert!(ir.contains("const_int 20"));
        assert!(ir.contains("iadd"));
    }

    #[test]
    fn lower_real_arithmetic() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  real :: a, b, c
  a = 3.14
  b = 2.0
  c = a * b
end program
");
        assert!(ir.contains("const_float"));
        assert!(ir.contains("fmul"));
    }

    #[test]
    fn lower_print() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 42
  print *, x
end program
");
        assert!(ir.contains("rt_call @__afs_print_int"));
        assert!(ir.contains("rt_call @__afs_print_newline"));
    }

    #[test]
    fn lower_unary_minus() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y
  x = 5
  y = -x
end program
");
        assert!(ir.contains("ineg"));
    }

    #[test]
    fn lower_multiple_vars() {
        let (module, ir) = lower_and_verify("\
program test
  implicit none
  integer :: a, b, c, d
  a = 1
  b = 2
  c = 3
  d = a + b + c
end program
");
        assert_eq!(module.functions.len(), 1);
        // Should have two iadd operations (a+b, then result+c).
        let iadd_count = ir.matches("iadd").count();
        assert_eq!(iadd_count, 2);
    }

    #[test]
    fn lower_stop() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  stop
end program
");
        assert!(ir.contains("rt_call @__afs_stop"));
        assert!(ir.contains("unreachable"));
    }

    #[test]
    fn ir_passes_verifier() {
        // All lowered programs should produce verifier-clean IR.
        lower_and_verify("program p\n  implicit none\n  integer :: x\n  x = 1\nend program\n");
        lower_and_verify("program p\n  implicit none\n  real :: x\n  x = 1.0\nend program\n");
        lower_and_verify("program p\n  implicit none\n  integer :: x, y\n  x = 1\n  y = x + 2\n  print *, y\nend program\n");
    }
}
