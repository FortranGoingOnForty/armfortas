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

use crate::ast::decl::ArraySpec;
use std::collections::HashMap;

/// Maximum array rank (Fortran allows up to 15).
const MAX_RANK: usize = 15;

/// Loop context for EXIT/CYCLE targeting.
struct LoopScope {
    name: Option<String>,
    header: BlockId,  // CYCLE target
    exit: BlockId,    // EXIT target
}

/// Info about a local variable.
#[derive(Clone)]
struct LocalInfo {
    addr: ValueId,
    ty: IrType,
    /// For arrays: (lower_bound, size) per dimension.
    /// Empty for scalars.
    dims: Vec<(i64, i64)>,
}

/// Lowering context — tracks locals, loop scopes, and symbol table.
struct LowerCtx<'a> {
    locals: HashMap<String, LocalInfo>,
    loops: Vec<LoopScope>,
    st: &'a SymbolTable,
    globals: &'a HashMap<String, (usize, IrType)>,
}

impl<'a> LowerCtx<'a> {
    fn new(st: &'a SymbolTable, globals: &'a HashMap<String, (usize, IrType)>) -> Self {
        Self { locals: HashMap::new(), loops: Vec::new(), st, globals }
    }

    fn insert_scalar(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![] });
    }

    fn insert_array(&mut self, name: String, addr: ValueId, ty: IrType, dims: Vec<(i64, i64)>) {
        self.locals.insert(name, LocalInfo { addr, ty, dims });
    }

    fn push_loop(&mut self, name: Option<String>, header: BlockId, exit: BlockId) {
        self.loops.push(LoopScope { name, header, exit });
    }

    fn pop_loop(&mut self) {
        self.loops.pop();
    }

    /// Find loop by construct name (or innermost if None).
    fn find_loop(&self, name: &Option<String>) -> Option<&LoopScope> {
        if let Some(n) = name {
            self.loops.iter().rev().find(|l| l.name.as_deref().map(|s| s.eq_ignore_ascii_case(n)).unwrap_or(false))
        } else {
            self.loops.last()
        }
    }
}

/// Lower a file of program units to an IR module.
pub fn lower_file(
    units: &[SpannedUnit],
    st: &SymbolTable,
) -> Module {
    let mut module = Module::new("main".into());
    let globals = HashMap::new(); // populated by module lowering
    for unit in units {
        lower_unit(&mut module, unit, st, &globals);
    }
    module
}

fn lower_unit(module: &mut Module, unit: &SpannedUnit, st: &SymbolTable, globals: &HashMap<String, (usize, IrType)>) {
    match &unit.node {
        ProgramUnit::Program { name, decls, body, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let mut func = Function::new(fname, vec![], IrType::Void);
            let mut ctx = LowerCtx::new(st, globals);

            {
                let mut b = FuncBuilder::new(&mut func);
                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
        }
        ProgramUnit::Subroutine { name, decls, body, args, .. } => {
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    Some(Param {
                        name: n.clone(),
                        ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                        id: ValueId(i as u32),
                    })
                } else { None }
            }).collect();
            let mut func = Function::new(name.clone(), params, IrType::Void);
            let mut ctx = LowerCtx::new(st, globals);

            for p in &func.params {
                ctx.insert_scalar(p.name.to_lowercase(), p.id, p.ty.clone());
            }

            {
                let mut b = FuncBuilder::new(&mut func);
                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);
                ensure_termination(&mut b, None);
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
            let mut ctx = LowerCtx::new(st, globals);

            for p in &func.params {
                ctx.insert_scalar(p.name.to_lowercase(), p.id, p.ty.clone());
            }

            {
                let mut b = FuncBuilder::new(&mut func);
                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                let result_addr = b.alloca(ret_ty.clone());
                ctx.insert_scalar(result_name, result_addr, ret_ty.clone());

                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);

                if b.func().block(b.current_block()).terminator.is_none() {
                    let rv = b.load(result_addr);
                    b.ret(Some(rv));
                }
            }

            module.add_function(func);
        }
        ProgramUnit::Module { name, decls, .. } => {
            // Module variables become globals.
            for decl in decls {
                if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
                    let ir_ty = lower_type_spec(type_spec);
                    for entity in entities {
                        module.add_global(Global {
                            name: format!("{}::{}", name, entity.name),
                            ty: ir_ty.clone(),
                            initializer: Some(GlobalInit::Zero),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

/// Allocate local variables from declarations. Handles both scalars and arrays.
fn alloc_decls(b: &mut FuncBuilder, locals: &mut HashMap<String, LocalInfo>, decls: &[crate::ast::decl::SpannedDecl]) {
    use crate::ast::decl::Attribute;
    for decl in decls {
        if let Decl::TypeDecl { type_spec, attrs, entities } = &decl.node {
            let elem_ty = lower_type_spec(type_spec);

            // Check for DIMENSION attribute on the declaration.
            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let Attribute::Dimension(specs) = a { Some(specs) } else { None }
            });

            for entity in entities {
                let key = entity.name.to_lowercase();
                if locals.contains_key(&key) { continue; }

                // Use entity-level array spec, or fall back to attribute-level DIMENSION.
                let array_spec = entity.array_spec.as_ref().or(attr_dims);

                if let Some(specs) = array_spec {
                    // Array variable.
                    let dims = extract_array_dims(specs);
                    let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                    let arr_ty = IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                    let addr = b.alloca(arr_ty);
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims });
                } else {
                    // Scalar variable.
                    let addr = b.alloca(elem_ty.clone());
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![] });
                }
            }
        }
    }
}

/// Extract compile-time array dimensions from array spec.
/// Returns (lower_bound, extent) pairs. Runtime expressions default to (1, 1).
fn extract_array_dims(specs: &[ArraySpec]) -> Vec<(i64, i64)> {
    specs.iter().map(|spec| {
        match spec {
            ArraySpec::Explicit { lower, upper } => {
                let lo = lower.as_ref().and_then(eval_const_int).unwrap_or(1);
                let hi = eval_const_int(upper).unwrap_or(1);
                (lo, hi - lo + 1)
            }
            ArraySpec::AssumedShape { .. } => (1, 0), // size unknown at compile time
            ArraySpec::Deferred => (1, 0),
            ArraySpec::AssumedSize { .. } => (1, 0),
            ArraySpec::AssumedRank => (1, 0),
        }
    }).collect()
}

/// Try to evaluate a constant integer expression at compile time.
fn eval_const_int(expr: &crate::ast::expr::SpannedExpr) -> Option<i64> {
    match &expr.node {
        Expr::IntegerLiteral { text, .. } => text.parse().ok(),
        Expr::UnaryOp { op: UnaryOp::Minus, operand } => {
            eval_const_int(operand).map(|v| -v)
        }
        _ => None,
    }
}

/// Ensure a block has a terminator.
fn ensure_termination(b: &mut FuncBuilder, result_addr: Option<ValueId>) {
    if b.func().block(b.current_block()).terminator.is_none() {
        if let Some(addr) = result_addr {
            let rv = b.load(addr);
            b.ret(Some(rv));
        } else {
            b.ret_void();
        }
    }
}

/// Extract the kind value from a KindSelector, defaulting if absent.
fn extract_kind(sel: &Option<crate::ast::decl::KindSelector>, default: u8) -> u8 {
    use crate::ast::decl::KindSelector;
    use crate::ast::expr::Expr;
    match sel {
        Some(KindSelector::Expr(e)) | Some(KindSelector::Star(e)) => {
            if let Expr::IntegerLiteral { text, .. } = &e.node {
                text.parse().unwrap_or(default)
            } else { default }
        }
        None => default,
    }
}

/// Lower a Fortran type specifier to an IR type.
fn lower_type_spec(ts: &TypeSpec) -> IrType {
    match ts {
        TypeSpec::Integer(sel) => IrType::int_from_kind(extract_kind(sel, 4)),
        TypeSpec::Real(sel) => IrType::float_from_kind(extract_kind(sel, 4)),
        TypeSpec::DoublePrecision => IrType::Float(FloatWidth::F64),
        TypeSpec::Complex(sel) => {
            let fw = match extract_kind(sel, 4) {
                8 => FloatWidth::F64,
                _ => FloatWidth::F32,
            };
            IrType::Array(Box::new(IrType::Float(fw)), 2)
        }
        TypeSpec::DoubleComplex => IrType::Array(Box::new(IrType::Float(FloatWidth::F64)), 2),
        TypeSpec::Logical(_) => IrType::Bool,
        TypeSpec::Character(_) => IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
        _ => IrType::Int(IntWidth::I32), // fallback for derived types etc.
    }
}

/// Lower a list of statements.
fn lower_stmts(b: &mut FuncBuilder, ctx: &mut LowerCtx, stmts: &[SpannedStmt]) {
    for stmt in stmts {
        // If current block already has a terminator (e.g., after STOP), skip dead code.
        if b.func().block(b.current_block()).terminator.is_some() {
            break;
        }
        lower_stmt(b, ctx, stmt);
    }
}

/// Lower a single statement.
fn lower_stmt(b: &mut FuncBuilder, ctx: &mut LowerCtx, stmt: &SpannedStmt) {
    match &stmt.node {
        Stmt::Assignment { target, value } => {
            let val = lower_expr(b, &ctx.locals, value, ctx.st);
            match &target.node {
                Expr::Name { name } => {
                    let key = name.to_lowercase();
                    if let Some(info) = ctx.locals.get(&key) {
                        b.store(val, info.addr);
                    }
                }
                Expr::FunctionCall { callee, args } => {
                    // Array element assignment: a(i) = val
                    if let Expr::Name { name } = &callee.node {
                        let key = name.to_lowercase();
                        if let Some(info) = ctx.locals.get(&key).cloned() {
                            if !info.dims.is_empty() {
                                lower_array_store(b, &ctx.locals, &info, args, val, ctx.st);
                            }
                        }
                    }
                }
                _ => {} // component access etc. deferred
            }
        }

        Stmt::Print { items, .. } | Stmt::Write { items, .. } => {
            for item in items {
                let val = lower_expr(b, &ctx.locals, item, ctx.st);
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
                // Fortran default: pass by reference (pass address of each argument).
                // If the argument is a named variable, pass its address directly.
                // If it's an expression, evaluate it, store to a temp, pass temp address.
                let arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => {
                            lower_arg_by_ref(b, &ctx.locals, e, ctx.st)
                        }
                        _ => b.const_i32(0),
                    }
                }).collect();
                b.call(FuncRef::External(name.clone()), arg_vals, IrType::Void);
            }
        }

        // ---- Control flow ----

        Stmt::IfConstruct { condition, then_body, else_ifs, else_body, .. } => {
            lower_if(b, ctx, condition, then_body, else_ifs, else_body);
        }

        Stmt::IfStmt { condition, action } => {
            let cond = lower_expr(b, &ctx.locals, condition, ctx.st);
            let bb_then = b.create_block("if_then");
            let bb_end = b.create_block("if_end");
            b.cond_branch(cond, bb_then, vec![], bb_end, vec![]);

            b.set_block(bb_then);
            lower_stmt(b, ctx, action);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }

            b.set_block(bb_end);
        }

        Stmt::DoLoop { name, var, start, end, step, body } => {
            lower_do_loop(b, ctx, DoLoopFields { name, var, start, end, step, body });
        }

        Stmt::DoWhile { name, condition, body } => {
            let bb_header = b.create_block("do_while_header");
            let bb_body = b.create_block("do_while_body");
            let bb_exit = b.create_block("do_while_exit");
            b.branch(bb_header, vec![]);

            ctx.push_loop(name.clone(), bb_header, bb_exit);

            b.set_block(bb_header);
            let cond = lower_expr(b, &ctx.locals, condition, ctx.st);
            b.cond_branch(cond, bb_body, vec![], bb_exit, vec![]);

            b.set_block(bb_body);
            lower_stmts(b, ctx, body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_header, vec![]);
            }

            ctx.pop_loop();
            b.set_block(bb_exit);
        }

        Stmt::SelectCase { selector, cases, .. } => {
            lower_select_case(b, ctx, selector, cases);
        }

        Stmt::Exit { name } => {
            if let Some(lp) = ctx.find_loop(name) {
                let exit = lp.exit;
                b.branch(exit, vec![]);
            }
        }

        Stmt::Cycle { name } => {
            if let Some(lp) = ctx.find_loop(name) {
                let header = lp.header;
                b.branch(header, vec![]);
            }
        }

        Stmt::Return { .. } => {
            b.ret_void();
        }

        Stmt::Stop { .. } => {
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::ErrorStop { .. } => {
            b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
            b.unreachable();
        }

        Stmt::Allocate { items, .. } => {
            for item in items {
                let base_name = extract_base_name(item);
                if let Some(name) = base_name {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()) {
                        let ptr = b.runtime_call(RuntimeFunc::Allocate, vec![], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        b.store(ptr, info.addr);
                    }
                }
            }
        }

        Stmt::Deallocate { items, .. } => {
            for item in items {
                let base_name = extract_base_name(item);
                if let Some(name) = base_name {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()) {
                        let ptr = b.load(info.addr);
                        b.runtime_call(RuntimeFunc::Deallocate, vec![ptr], IrType::Void);
                    }
                }
            }
        }

        Stmt::Block { body, .. } => {
            lower_stmts(b, ctx, body);
        }

        Stmt::Associate { assocs, body, .. } => {
            // Associate names are aliases — lower the expression and bind the value.
            for (name, expr) in assocs {
                let val = lower_expr(b, &ctx.locals, expr, ctx.st);
                let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
                let addr = b.alloca(ty.clone());
                b.store(val, addr);
                ctx.locals.insert(name.to_lowercase(), LocalInfo { addr, ty, dims: vec![] });
            }
            lower_stmts(b, ctx, body);
        }

        Stmt::Continue { .. } => {} // no-op

        _ => {} // remaining statements (FORALL, WHERE, etc.) deferred
    }
}

/// Lower IF construct with else-if chain and optional else.
fn lower_if(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    condition: &crate::ast::expr::SpannedExpr,
    then_body: &[SpannedStmt],
    else_ifs: &[(crate::ast::expr::SpannedExpr, Vec<SpannedStmt>)],
    else_body: &Option<Vec<SpannedStmt>>,
) {
    let bb_end = b.create_block("if_end");

    let cond = lower_expr(b, &ctx.locals, condition, ctx.st);
    let bb_then = b.create_block("if_then");
    let bb_next = if !else_ifs.is_empty() || else_body.is_some() {
        b.create_block("if_else")
    } else {
        bb_end
    };
    b.cond_branch(cond, bb_then, vec![], bb_next, vec![]);

    // Then block.
    b.set_block(bb_then);
    lower_stmts(b, ctx, then_body);
    if b.func().block(b.current_block()).terminator.is_none() {
        b.branch(bb_end, vec![]);
    }

    // Else-if chain.
    let mut current_else = bb_next;
    for (i, (ei_cond, ei_body)) in else_ifs.iter().enumerate() {
        b.set_block(current_else);
        let ei_cond_val = lower_expr(b, &ctx.locals, ei_cond, ctx.st);
        let bb_ei_then = b.create_block(&format!("elseif_{}_then", i));
        let bb_ei_next = if i + 1 < else_ifs.len() || else_body.is_some() {
            b.create_block(&format!("elseif_{}_else", i))
        } else {
            bb_end
        };
        b.cond_branch(ei_cond_val, bb_ei_then, vec![], bb_ei_next, vec![]);

        b.set_block(bb_ei_then);
        lower_stmts(b, ctx, ei_body);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_end, vec![]);
        }

        current_else = bb_ei_next;
    }

    // Else block.
    if let Some(eb) = else_body {
        b.set_block(current_else);
        lower_stmts(b, ctx, eb);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_end, vec![]);
        }
    }

    b.set_block(bb_end);
}

/// DO loop fields bundled for passing without too many args.
struct DoLoopFields<'a> {
    name: &'a Option<String>,
    var: &'a Option<String>,
    start: &'a Option<crate::ast::expr::SpannedExpr>,
    end: &'a Option<crate::ast::expr::SpannedExpr>,
    step: &'a Option<crate::ast::expr::SpannedExpr>,
    body: &'a [SpannedStmt],
}

/// Lower DO loop (counted loop with variable, start, end, step).
fn lower_do_loop(b: &mut FuncBuilder, ctx: &mut LowerCtx, fields: DoLoopFields) {
    let DoLoopFields { name, var, start, end, step, body } = fields;
    if let (Some(var_name), Some(start_expr), Some(end_expr)) = (var, start, end) {
        // Counted DO loop.
        let key = var_name.to_lowercase();
        let var_addr = ctx.locals.get(&key).map(|info| info.addr).unwrap_or_else(|| {
            let addr = b.alloca(IrType::Int(IntWidth::I32));
            ctx.locals.insert(key.clone(), LocalInfo { addr, ty: IrType::Int(IntWidth::I32), dims: vec![] });
            addr
        });

        // Initialize loop variable.
        let init_val = lower_expr(b, &ctx.locals, start_expr, ctx.st);
        b.store(init_val, var_addr);

        let end_val = lower_expr(b, &ctx.locals, end_expr, ctx.st);
        let step_val = if let Some(step_expr) = step {
            lower_expr(b, &ctx.locals, step_expr, ctx.st)
        } else {
            b.const_i32(1)
        };

        let bb_check = b.create_block("do_check");
        let bb_body = b.create_block("do_body");
        let bb_incr = b.create_block("do_incr");
        let bb_exit = b.create_block("do_exit");

        b.branch(bb_check, vec![]);

        // Check: i <= end (or i >= end for negative step).
        b.set_block(bb_check);
        let cur = b.load(var_addr);
        let cond = b.icmp(CmpOp::Le, cur, end_val);
        b.cond_branch(cond, bb_body, vec![], bb_exit, vec![]);

        // Body.
        ctx.push_loop(name.clone(), bb_incr, bb_exit);
        b.set_block(bb_body);
        lower_stmts(b, ctx, body);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_incr, vec![]);
        }
        ctx.pop_loop();

        // Increment.
        b.set_block(bb_incr);
        let cur2 = b.load(var_addr);
        let next = b.iadd(cur2, step_val);
        b.store(next, var_addr);
        b.branch(bb_check, vec![]);

        b.set_block(bb_exit);
    } else {
        // Infinite DO (no variable) — `do ... end do` without loop control.
        let bb_body = b.create_block("do_body");
        let bb_exit = b.create_block("do_exit");
        b.branch(bb_body, vec![]);

        ctx.push_loop(name.clone(), bb_body, bb_exit);
        b.set_block(bb_body);
        lower_stmts(b, ctx, body);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_body, vec![]);
        }
        ctx.pop_loop();

        b.set_block(bb_exit);
    }
}

/// Lower SELECT CASE.
fn lower_select_case(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    selector: &crate::ast::expr::SpannedExpr,
    cases: &[CaseBlock],
) {
    let sel_val = lower_expr(b, &ctx.locals, selector, ctx.st);
    let bb_end = b.create_block("select_end");

    // For simplicity, lower as a chain of if-else comparisons.
    // (Switch terminator would be ideal for integer constants, but the
    // general case needs range checks and DEFAULT handling.)
    let mut bb_current = b.current_block();

    for (i, case) in cases.iter().enumerate() {
        let is_default = case.selectors.iter().any(|s| matches!(s, CaseSelector::Default));

        if is_default {
            // Default case — always taken.
            b.set_block(bb_current);
            let bb_body = b.create_block(&format!("case_{}_body", i));
            b.branch(bb_body, vec![]);

            b.set_block(bb_body);
            lower_stmts(b, ctx, &case.body);
            if b.func().block(b.current_block()).terminator.is_none() {
                b.branch(bb_end, vec![]);
            }
            // After default, no more cases matter.
            b.set_block(bb_end);
            return;
        }

        let bb_body = b.create_block(&format!("case_{}_body", i));
        let bb_next = b.create_block(&format!("case_{}_next", i));

        b.set_block(bb_current);

        // Build condition from selectors (OR them together).
        let mut combined_cond: Option<ValueId> = None;
        for sel in &case.selectors {
            let cond = match sel {
                CaseSelector::Value(expr) => {
                    let val = lower_expr(b, &ctx.locals, expr, ctx.st);
                    b.icmp(CmpOp::Eq, sel_val, val)
                }
                CaseSelector::Range { low, high } => {
                    let low_ok = if let Some(lo) = low {
                        let lo_val = lower_expr(b, &ctx.locals, lo, ctx.st);
                        Some(b.icmp(CmpOp::Ge, sel_val, lo_val))
                    } else { None };
                    let high_ok = if let Some(hi) = high {
                        let hi_val = lower_expr(b, &ctx.locals, hi, ctx.st);
                        Some(b.icmp(CmpOp::Le, sel_val, hi_val))
                    } else { None };
                    match (low_ok, high_ok) {
                        (Some(l), Some(h)) => b.and(l, h),
                        (Some(c), None) | (None, Some(c)) => c,
                        (None, None) => b.const_bool(true),
                    }
                }
                CaseSelector::Default => unreachable!(), // handled above
            };
            combined_cond = Some(match combined_cond {
                Some(prev) => b.or(prev, cond),
                None => cond,
            });
        }

        let cond = combined_cond.unwrap_or_else(|| b.const_bool(false));
        b.cond_branch(cond, bb_body, vec![], bb_next, vec![]);

        b.set_block(bb_body);
        lower_stmts(b, ctx, &case.body);
        if b.func().block(b.current_block()).terminator.is_none() {
            b.branch(bb_end, vec![]);
        }

        bb_current = bb_next;
    }

    // If no case matched and no default, fall through.
    b.set_block(bb_current);
    b.branch(bb_end, vec![]);

    b.set_block(bb_end);
}

/// Lower an array element access: compute flat offset from subscripts, GEP, load.
/// Fortran column-major: a(i, j) in a(m, n) → offset = (i - lower1) + (j - lower2) * m
fn lower_array_element(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    st: &SymbolTable,
) -> ValueId {
    // Compute flat index from subscripts.
    let mut flat_offset: Option<ValueId> = None;
    let mut stride: i64 = 1;

    for (dim_idx, arg) in args.iter().enumerate() {
        let subscript = match &arg.value {
            crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
            _ => b.const_i32(0),
        };

        let (lower, extent) = if dim_idx < info.dims.len() {
            info.dims[dim_idx]
        } else {
            (1, 1)
        };

        // offset_dim = (subscript - lower) * stride
        let lower_val = b.const_i32(lower as i32);
        let adjusted = b.isub(subscript, lower_val);

        let dim_offset = if stride == 1 {
            adjusted
        } else {
            let stride_val = b.const_i32(stride as i32);
            b.imul(adjusted, stride_val)
        };

        flat_offset = Some(match flat_offset {
            Some(prev) => b.iadd(prev, dim_offset),
            None => dim_offset,
        });

        stride *= extent;
    }

    let idx = flat_offset.unwrap_or_else(|| b.const_i32(0));
    let elem_ptr = b.gep(info.addr, vec![idx], info.ty.clone());
    b.load(elem_ptr)
}

/// Lower an array element store: compute flat offset, GEP, store.
fn lower_array_store(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    info: &LocalInfo,
    args: &[crate::ast::expr::Argument],
    value: ValueId,
    st: &SymbolTable,
) {
    let mut flat_offset: Option<ValueId> = None;
    let mut stride: i64 = 1;

    for (dim_idx, arg) in args.iter().enumerate() {
        let subscript = match &arg.value {
            crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
            _ => b.const_i32(0),
        };

        let (lower, extent) = if dim_idx < info.dims.len() {
            info.dims[dim_idx]
        } else {
            (1, 1)
        };

        let lower_val = b.const_i32(lower as i32);
        let adjusted = b.isub(subscript, lower_val);

        let dim_offset = if stride == 1 {
            adjusted
        } else {
            let stride_val = b.const_i32(stride as i32);
            b.imul(adjusted, stride_val)
        };

        flat_offset = Some(match flat_offset {
            Some(prev) => b.iadd(prev, dim_offset),
            None => dim_offset,
        });

        stride *= extent;
    }

    let idx = flat_offset.unwrap_or_else(|| b.const_i32(0));
    let elem_ptr = b.gep(info.addr, vec![idx], info.ty.clone());
    b.store(value, elem_ptr);
}

/// Extract base variable name from an expression.
fn extract_base_name(expr: &crate::ast::expr::SpannedExpr) -> Option<String> {
    match &expr.node {
        Expr::Name { name } => Some(name.clone()),
        Expr::FunctionCall { callee, .. } => extract_base_name(callee),
        _ => None,
    }
}

/// Lower an argument for pass-by-reference: return the address of the value.
/// If the argument is a named variable, return its alloca address.
/// If it's an expression (literal, computation), store to a temp and return the temp address.
fn lower_arg_by_ref(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> ValueId {
    // If it's a simple name, pass its existing address.
    if let Expr::Name { name } = &expr.node {
        let key = name.to_lowercase();
        if let Some(info) = locals.get(&key) {
            return info.addr;
        }
    }
    // Otherwise, evaluate and store to a temp.
    let val = lower_expr(b, locals, expr, st);
    let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
    let tmp = b.alloca(ty);
    b.store(val, tmp);
    tmp
}

/// Lower an expression to a ValueId.
#[allow(clippy::only_used_in_recursion)]
fn lower_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
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
            if let Some(info) = locals.get(&key) {
                if info.dims.is_empty() {
                    b.load(info.addr)
                } else {
                    // Array name without subscripts — return the base address.
                    info.addr
                }
            } else {
                b.const_i32(0) // implicit or unknown
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
                (BinaryOp::Pow, IrType::Float(_)) => b.fpow(lhs, rhs),
                (BinaryOp::Pow, IrType::Int(_)) => {
                    // Integer power: convert to float, fpow, convert back.
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
            if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();

                // Check if this is an array element access.
                if let Some(info) = locals.get(&key) {
                    if !info.dims.is_empty() {
                        // Array element: compute flat offset and load.
                        return lower_array_element(b, locals, info, args, st);
                    }
                }

                // Otherwise, function/intrinsic call.
                let arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
                        _ => b.const_i32(0),
                    }
                }).collect();
                let ret_ty = IrType::Int(IntWidth::I32); // default
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
        lower_and_verify("program p\n  implicit none\n  integer :: x\n  x = 1\nend program\n");
        lower_and_verify("program p\n  implicit none\n  real :: x\n  x = 1.0\nend program\n");
        lower_and_verify("program p\n  implicit none\n  integer :: x, y\n  x = 1\n  y = x + 2\n  print *, y\nend program\n");
    }

    // ---- Control flow ----

    #[test]
    fn lower_if_then_else() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y
  x = 5
  if (x > 0) then
    y = 1
  else
    y = -1
  end if
end program
");
        assert!(ir.contains("cond_br"));
        assert!(ir.contains("if_then"));
        assert!(ir.contains("if_else"));
        assert!(ir.contains("if_end"));
    }

    #[test]
    fn lower_if_elseif() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y
  x = 5
  if (x > 10) then
    y = 1
  else if (x > 0) then
    y = 2
  else
    y = 3
  end if
end program
");
        assert!(ir.contains("elseif_0_then"));
    }

    #[test]
    fn lower_if_stmt() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 5
  if (x > 0) x = 0
end program
");
        assert!(ir.contains("if_then"));
        assert!(ir.contains("if_end"));
    }

    #[test]
    fn lower_do_loop() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, s
  s = 0
  do i = 1, 10
    s = s + i
  end do
end program
");
        assert!(ir.contains("do_check"));
        assert!(ir.contains("do_body"));
        assert!(ir.contains("do_incr"));
        assert!(ir.contains("do_exit"));
        assert!(ir.contains("icmp le"));
    }

    #[test]
    fn lower_do_loop_with_step() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, s
  s = 0
  do i = 1, 10, 2
    s = s + i
  end do
end program
");
        assert!(ir.contains("const_int 2"));
        assert!(ir.contains("do_incr"));
    }

    #[test]
    fn lower_do_while() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 10
  do while (x > 0)
    x = x - 1
  end do
end program
");
        assert!(ir.contains("do_while_header"));
        assert!(ir.contains("do_while_body"));
        assert!(ir.contains("do_while_exit"));
    }

    #[test]
    fn lower_exit_cycle() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, s
  s = 0
  do i = 1, 100
    if (i > 10) exit
    if (i == 5) cycle
    s = s + i
  end do
end program
");
        // EXIT should branch to do_exit, CYCLE to do_incr.
        assert!(ir.contains("do_exit"));
        assert!(ir.contains("do_incr"));
    }

    #[test]
    fn lower_select_case() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x, y
  x = 2
  select case (x)
  case (1)
    y = 10
  case (2)
    y = 20
  case default
    y = 0
  end select
end program
");
        assert!(ir.contains("case_0_body"));
        assert!(ir.contains("case_1_body"));
        assert!(ir.contains("select_end"));
    }

    #[test]
    fn lower_nested_loops() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, j, s
  s = 0
  do i = 1, 10
    do j = 1, 10
      s = s + i * j
    end do
  end do
end program
");
        // Should have multiple do_check blocks (label lines + branch references).
        // Two loops means at least 2 label lines "do_check():" in the output.
        let label_count = ir.matches("do_check():").count();
        assert_eq!(label_count, 2, "expected 2 loop header labels, got {} in:\n{}", label_count, ir);
    }

    #[test]
    fn lower_return() {
        let (_, ir) = lower_and_verify("\
subroutine foo()
  implicit none
  return
end subroutine
");
        assert!(ir.contains("ret void"));
    }

    #[test]
    fn lower_associate() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 42
  associate (n => x)
    print *, n
  end associate
end program
");
        assert!(ir.contains("rt_call @__afs_print_int"));
    }

    #[test]
    fn lower_call_passes_addresses() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 42
  call foo(x)
end program
");
        // x should be passed by reference — the alloca address, not a loaded value.
        // The call should reference the alloca directly.
        assert!(ir.contains("call @foo("));
    }

    #[test]
    fn lower_call_expression_arg() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 5
  call foo(x + 1)
end program
");
        // Expression arg: x+1 evaluated, stored to temp, temp address passed.
        assert!(ir.contains("iadd"));
        assert!(ir.contains("alloca")); // temp for expression result
        assert!(ir.contains("call @foo("));
    }

    // ---- Arrays ----

    #[test]
    fn lower_array_declaration() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: a(10)
  a(1) = 42
end program
");
        // Should alloca an array of 10 i32, then GEP + store.
        assert!(ir.contains("[i32 x 10]"), "expected array alloca in:\n{}", ir);
        assert!(ir.contains("gep"), "expected GEP in:\n{}", ir);
    }

    #[test]
    fn lower_array_read() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: a(10), x
  a(3) = 99
  x = a(3)
end program
");
        // Reading array element: GEP + load.
        let gep_count = ir.matches("gep").count();
        assert!(gep_count >= 2, "expected at least 2 GEPs (write + read), got {} in:\n{}", gep_count, ir);
    }

    #[test]
    fn lower_2d_array() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: mat(3, 4)
  mat(2, 3) = 42
end program
");
        // 2D array: alloca [i32 x 12], column-major offset.
        assert!(ir.contains("[i32 x 12]"), "expected 3*4=12 element array in:\n{}", ir);
        assert!(ir.contains("gep"), "expected GEP in:\n{}", ir);
    }

    #[test]
    fn lower_array_in_loop() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: a(10), i
  do i = 1, 10
    a(i) = i * 2
  end do
end program
");
        assert!(ir.contains("gep"));
        assert!(ir.contains("imul"));
    }

    #[test]
    fn lower_module_globals() {
        let module = lower_source("\
module mymod
  implicit none
  integer :: counter
  real :: threshold
end module
");
        assert_eq!(module.globals.len(), 2);
        assert!(module.globals.iter().any(|g| g.name.contains("counter")));
        assert!(module.globals.iter().any(|g| g.name.contains("threshold")));
    }

    #[test]
    fn lower_block_construct() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: x
  x = 1
  block
    x = x + 1
  end block
end program
");
        assert!(ir.contains("iadd"));
    }
}
