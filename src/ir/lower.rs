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

/// Character variable kind: how string storage is managed.
#[derive(Clone, PartialEq)]
enum CharKind {
    /// Not a character variable.
    None,
    /// Fixed-length character(N): addr points to N-byte stack buffer.
    Fixed(i64),
    /// Deferred-length character(:), allocatable: addr points to 32-byte StringDescriptor.
    Deferred,
}

/// Info about a local variable.
#[derive(Clone)]
struct LocalInfo {
    addr: ValueId,
    ty: IrType,
    /// For arrays: (lower_bound, extent) per dimension. Empty for scalars.
    dims: Vec<(i64, i64)>,
    /// Is this an allocatable variable?
    allocatable: bool,
    /// Is this a pass-by-reference parameter? If true, `addr` holds a pointer
    /// to the caller's storage. Reads/writes go through the pointer.
    by_ref: bool,
    /// Character variable kind (fixed-length, deferred, or not character).
    char_kind: CharKind,
}

/// Lowering context — tracks locals, loop scopes, and symbol table.
struct LowerCtx<'a> {
    locals: HashMap<String, LocalInfo>,
    loops: Vec<LoopScope>,
    st: &'a SymbolTable,
    globals: &'a HashMap<String, (usize, IrType)>,
    /// For functions: address of the result variable (for RETURN).
    result_addr: Option<ValueId>,
    /// For functions: the return type.
    result_type: Option<IrType>,
}

impl<'a> LowerCtx<'a> {
    fn new(st: &'a SymbolTable, globals: &'a HashMap<String, (usize, IrType)>) -> Self {
        Self { locals: HashMap::new(), loops: Vec::new(), st, globals, result_addr: None, result_type: None }
    }

    fn insert_scalar(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None });
    }

    fn insert_param_by_ref(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: true, char_kind: CharKind::None });
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
        ProgramUnit::Program { name, decls, body, contains, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let mut func = Function::new(fname, vec![], IrType::Void);
            let mut ctx = LowerCtx::new(st, globals);

            {
                let mut b = FuncBuilder::new(&mut func);
                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);

            // Lower CONTAINS subprograms.
            for sub in contains {
                lower_unit(module, sub, st, globals);
            }
        }
        ProgramUnit::Subroutine { name, decls, body, args, .. } => {
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    let elem_ty = arg_type_from_decls(n, decls);
                    Some(Param {
                        name: n.clone(),
                        ty: IrType::Ptr(Box::new(elem_ty)),
                        id: ValueId(i as u32),
                    })
                } else { None }
            }).collect();
            let mut func = Function::new(name.clone(), params, IrType::Void);
            let mut ctx = LowerCtx::new(st, globals);

            // Collect param info before borrowing func mutably.
            let param_info: Vec<(String, ValueId, IrType)> = func.params.iter()
                .map(|p| {
                    let elem_ty = match &p.ty {
                        IrType::Ptr(inner) => (**inner).clone(),
                        other => other.clone(),
                    };
                    (p.name.to_lowercase(), p.id, elem_ty)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);

                for (pname, pid, elem_ty) in &param_info {
                    let slot = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                    b.store(*pid, slot);
                    ctx.insert_param_by_ref(pname.clone(), slot, elem_ty.clone());
                }

                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
        }
        ProgramUnit::Function { name, decls, body, args, result, return_type, .. } => {
            let ret_ty = return_type.as_ref()
                .map(lower_type_spec)
                .unwrap_or_else(|| {
                    // No prefix type — infer from the result variable's declaration.
                    let result_name = result.as_deref().unwrap_or(name.as_str());
                    arg_type_from_decls(result_name, decls)
                });
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    let elem_ty = arg_type_from_decls(n, decls);
                    Some(Param {
                        name: n.clone(),
                        ty: IrType::Ptr(Box::new(elem_ty)),
                        id: ValueId(i as u32),
                    })
                } else { None }
            }).collect();
            let mut func = Function::new(name.clone(), params, ret_ty.clone());
            let mut ctx = LowerCtx::new(st, globals);

            let param_info: Vec<(String, ValueId, IrType)> = func.params.iter()
                .map(|p| {
                    let elem_ty = match &p.ty {
                        IrType::Ptr(inner) => (**inner).clone(),
                        other => other.clone(),
                    };
                    (p.name.to_lowercase(), p.id, elem_ty)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);

                for (pname, pid, elem_ty) in &param_info {
                    let slot = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                    b.store(*pid, slot);
                    ctx.insert_param_by_ref(pname.clone(), slot, elem_ty.clone());
                }

                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                let result_addr = b.alloca(ret_ty.clone());
                ctx.insert_scalar(result_name, result_addr, ret_ty.clone());
                ctx.result_addr = Some(result_addr);
                ctx.result_type = Some(ret_ty.clone());

                alloc_decls(&mut b, &mut ctx.locals, decls);
                lower_stmts(&mut b, &mut ctx, body);

                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals);
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

            let attr_dims: Option<&Vec<ArraySpec>> = attrs.iter().find_map(|a| {
                if let Attribute::Dimension(specs) = a { Some(specs) } else { None }
            });
            let is_allocatable = attrs.iter().any(|a| matches!(a, Attribute::Allocatable));

            for entity in entities {
                let key = entity.name.to_lowercase();
                if locals.contains_key(&key) { continue; }

                // Use entity-level array spec, or fall back to attribute-level DIMENSION.
                let array_spec = entity.array_spec.as_ref().or(attr_dims);

                // Check for character type.
                let char_len = match type_spec {
                    TypeSpec::Character(Some(sel)) => {
                        match &sel.len {
                            Some(crate::ast::decl::LenSpec::Expr(e)) => eval_const_int(e),
                            Some(crate::ast::decl::LenSpec::Star) => None, // assumed
                            Some(crate::ast::decl::LenSpec::Colon) => None, // deferred
                            None => Some(1), // default len=1
                        }
                    }
                    TypeSpec::Character(None) => Some(1),
                    _ => None,
                };
                let is_deferred_char = matches!(type_spec,
                    TypeSpec::Character(Some(sel)) if matches!(&sel.len, Some(crate::ast::decl::LenSpec::Colon))
                );

                if is_deferred_char && is_allocatable {
                    // Deferred-length allocatable character: 32-byte StringDescriptor.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 32);
                    let addr = b.alloca(desc_ty);
                    let zero = b.const_i32(0);
                    let size32 = b.const_i64(32);
                    b.call(FuncRef::External("memset".into()), vec![addr, zero, size32], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                    locals.insert(key, LocalInfo {
                        addr, ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                        dims: vec![], allocatable: true, by_ref: false,
                        char_kind: CharKind::Deferred,
                    });
                    continue;
                } else if let Some(len) = char_len {
                    if !is_allocatable {
                        // Fixed-length character(N): alloca N bytes buffer.
                        let buf_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), len as u64);
                        let addr = b.alloca(buf_ty);
                        // Initialize with spaces.
                        let space = b.const_i32(b' ' as i32);
                        let len_val = b.const_i64(len);
                        b.call(FuncRef::External("memset".into()), vec![addr, space, len_val], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        locals.insert(key, LocalInfo {
                            addr, ty: IrType::Int(IntWidth::I8),
                            dims: vec![], allocatable: false, by_ref: false,
                            char_kind: CharKind::Fixed(len),
                        });
                        continue; // skip normal path
                    }
                }

                if is_allocatable {
                    // Allocatable variable: alloca a descriptor (384 bytes), zero-initialized.
                    let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384);
                    let addr = b.alloca(desc_ty);
                    // Zero-initialize the descriptor so flags=0 (not allocated).
                    let zero = b.const_i32(0);
                    let size = b.const_i64(384);
                    b.call(FuncRef::External("memset".into()), vec![addr, zero, size], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: true, by_ref: false, char_kind: CharKind::None });
                } else if let Some(specs) = array_spec {
                    // Fixed-size array variable.
                    let dims = extract_array_dims(specs);
                    let total_size: i64 = dims.iter().map(|(_, size)| *size).product();
                    let elem_bytes = match &elem_ty {
                        IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
                        IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
                        _ => 8,
                    };
                    let total_bytes = total_size * elem_bytes;
                    const STACK_THRESHOLD: i64 = 64 * 1024; // 64KB

                    if total_bytes >= STACK_THRESHOLD {
                        // Large array: use descriptor + heap allocation (prevents stack overflow).
                        let desc_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 384);
                        let addr = b.alloca(desc_ty);
                        let zero = b.const_i32(0);
                        let size384 = b.const_i64(384);
                        b.call(FuncRef::External("memset".into()), vec![addr, zero, size384], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        // Auto-allocate with the declared shape.
                        let es = b.const_i64(elem_bytes);
                        let n = b.const_i64(total_size);
                        b.call(FuncRef::External("afs_allocate_1d".into()), vec![addr, es, n], IrType::Void);
                        // Mark as allocatable so scope-exit dealloc fires.
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: true, by_ref: false, char_kind: CharKind::None });
                    } else {
                        // Small array: stack allocation.
                        let arr_ty = IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                        let addr = b.alloca(arr_ty);
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: false, by_ref: false, char_kind: CharKind::None });
                    }
                } else {
                    // Scalar variable.
                    let addr = b.alloca(elem_ty.clone());
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None });
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

/// Lower a Fortran intrinsic function call to IR instructions.
/// Returns Some(ValueId) if recognized, None for external functions.
fn lower_intrinsic(b: &mut FuncBuilder, name: &str, args: &[ValueId]) -> Option<ValueId> {
    match name {
        "mod" | "modulo" => {
            if args.len() >= 2 {
                Some(b.imod(args[0], args[1]))
            } else { None }
        }
        "abs" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                match &ty {
                    IrType::Int(_) => {
                        // abs(x) = x >= 0 ? x : -x
                        let zero = b.const_i32(0);
                        let is_neg = b.icmp(CmpOp::Lt, *arg, zero);
                        let neg = b.ineg(*arg);
                        // Conditional select: for now, compute both and use subtraction trick.
                        // TODO: proper conditional select (CSEL instruction).
                        let _ = is_neg;
                        Some(neg) // simplified — always negates. Needs CSEL.
                    }
                    IrType::Float(FloatWidth::F32) => Some(b.fneg(*arg)), // simplified
                    IrType::Float(FloatWidth::F64) => Some(b.fneg(*arg)), // simplified
                    _ => None,
                }
            } else { None }
        }
        "int" | "nint" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    Some(b.float_to_int(*arg, IntWidth::I32))
                } else {
                    Some(*arg) // already integer
                }
            } else { None }
        }
        "real" | "float" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_int() {
                    Some(b.int_to_float(*arg, FloatWidth::F32))
                } else {
                    Some(*arg) // already real
                }
            } else { None }
        }
        "max" => {
            if args.len() >= 2 {
                // max(a, b) = a >= b ? a : b — simplified to compare + branch
                // TODO: proper CSEL
                let cmp = b.icmp(CmpOp::Ge, args[0], args[1]);
                let _ = cmp;
                Some(args[0]) // placeholder — needs CSEL
            } else { None }
        }
        "min" => {
            if args.len() >= 2 {
                let cmp = b.icmp(CmpOp::Le, args[0], args[1]);
                let _ = cmp;
                Some(args[0]) // placeholder — needs CSEL
            } else { None }
        }
        _ => None,
    }
}

/// Look up a dummy argument's declared type from the declaration list.
/// Returns the IR type for the argument, defaulting to I32 if not found.
fn arg_type_from_decls(arg_name: &str, decls: &[crate::ast::decl::SpannedDecl]) -> IrType {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    return lower_type_spec(type_spec);
                }
            }
        }
    }
    IrType::Int(IntWidth::I32) // fallback
}

/// Lower a string expression, returning (ptr, len) as ValueIds.
/// String literals return (const_string_ptr, const_len).
/// Character variables return (buffer_addr, known_len).
/// Deferred-length variables load ptr and len from the StringDescriptor.
fn lower_string_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> (ValueId, ValueId) {
    match &expr.node {
        Expr::StringLiteral { value, .. } => {
            let ptr = b.const_string(value.as_bytes());
            let len = b.const_i64(value.len() as i64);
            (ptr, len)
        }
        Expr::Name { name } => {
            let key = name.to_lowercase();
            if let Some(info) = locals.get(&key) {
                match &info.char_kind {
                    CharKind::Fixed(len) => {
                        let len_val = b.const_i64(*len);
                        (info.addr, len_val)
                    }
                    CharKind::Deferred => {
                        // Load data ptr (offset 0) and len (offset 8) from StringDescriptor.
                        let ptr = b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        // For len at offset 8, we'd need a GEP. For now, use a second load
                        // with manual offset. This is a simplification — proper struct access
                        // would use ExtractField or GEP with field index.
                        // The StringDescriptor layout: [data(8), len(8), capacity(8), flags(4)]
                        // Load len from addr + 8 bytes.
                        let eight = b.const_i64(8);
                        let len_ptr = b.gep(info.addr, vec![eight], IrType::Int(IntWidth::I64));
                        let len = b.load_typed(len_ptr, IrType::Int(IntWidth::I64));
                        (ptr, len)
                    }
                    CharKind::None => {
                        // Not a character variable — shouldn't happen but fall back.
                        let val = lower_expr(b, locals, expr, st);
                        let zero = b.const_i64(0);
                        (val, zero)
                    }
                }
            } else {
                let val = lower_expr(b, locals, expr, st);
                let zero = b.const_i64(0);
                (val, zero)
            }
        }
        _ => {
            // For other expressions (function calls, etc.), evaluate as value
            // and return with zero length. TODO: handle concatenation expressions.
            let val = lower_expr(b, locals, expr, st);
            let len = b.const_i64(string_literal_len(expr));
            (val, len)
        }
    }
}

/// Get the length of a string literal expression (for PRINT).
fn string_literal_len(expr: &crate::ast::expr::SpannedExpr) -> i64 {
    match &expr.node {
        Expr::StringLiteral { value, .. } => value.len() as i64,
        _ => 0,
    }
}

/// Insert implicit deallocation calls for all local allocatable variables.
/// Uses a dummy STAT variable so already-deallocated arrays don't abort.
fn insert_implicit_dealloc(b: &mut FuncBuilder, locals: &HashMap<String, LocalInfo>) {
    let stat_addr = b.alloca(IrType::Int(IntWidth::I32));
    for info in locals.values() {
        if info.char_kind == CharKind::Deferred {
            // Deferred-length string: call afs_dealloc_string.
            b.call(
                FuncRef::External("afs_dealloc_string".into()),
                vec![info.addr],
                IrType::Void,
            );
        } else if info.allocatable {
            // Allocatable array: call afs_deallocate_array.
            b.call(
                FuncRef::External("afs_deallocate_array".into()),
                vec![info.addr, stat_addr],
                IrType::Void,
            );
        }
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
            match &target.node {
                Expr::Name { name } => {
                    let key = name.to_lowercase();
                    if let Some(info) = ctx.locals.get(&key).cloned() {
                        match &info.char_kind {
                            CharKind::Fixed(len) => {
                                // Fixed-length character assignment: copy with space padding.
                                // Get source pointer and length from the expression.
                                let (src_ptr, src_len) = lower_string_expr(b, &ctx.locals, value, ctx.st);
                                let dest_len = b.const_i64(*len);
                                b.call(
                                    FuncRef::External("afs_assign_char_fixed".into()),
                                    vec![info.addr, dest_len, src_ptr, src_len],
                                    IrType::Void,
                                );
                            }
                            CharKind::Deferred => {
                                // Deferred-length: call afs_assign_char_deferred.
                                let (src_ptr, src_len) = lower_string_expr(b, &ctx.locals, value, ctx.st);
                                b.call(
                                    FuncRef::External("afs_assign_char_deferred".into()),
                                    vec![info.addr, src_ptr, src_len],
                                    IrType::Void,
                                );
                            }
                            CharKind::None => {
                                // Non-character: normal store.
                                let val = lower_expr(b, &ctx.locals, value, ctx.st);
                                if info.by_ref {
                                    let ptr = b.load(info.addr);
                                    b.store(val, ptr);
                                } else {
                                    b.store(val, info.addr);
                                }
                            }
                        }
                    }
                }
                Expr::FunctionCall { callee, args } => {
                    // Array element assignment: a(i) = val
                    let arr_val = lower_expr(b, &ctx.locals, value, ctx.st);
                    if let Expr::Name { name } = &callee.node {
                        let akey = name.to_lowercase();
                        if let Some(info) = ctx.locals.get(&akey).cloned() {
                            if !info.dims.is_empty() || info.allocatable {
                                lower_array_store(b, &ctx.locals, &info, args, arr_val, ctx.st);
                            }
                        }
                    }
                }
                _ => {} // component access etc. deferred
            }
        }

        Stmt::Print { items, .. } | Stmt::Write { items, .. } => {
            for item in items {
                // Check if this item is a character variable.
                let is_char = if let Expr::Name { name } = &item.node {
                    ctx.locals.get(&name.to_lowercase())
                        .map(|i| i.char_kind != CharKind::None)
                        .unwrap_or(false)
                } else {
                    false
                };

                if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
                    // Character: use lower_string_expr to get ptr + len.
                    let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
                    b.runtime_call(RuntimeFunc::PrintString, vec![ptr, len], IrType::Void);
                } else {
                    let val = lower_expr(b, &ctx.locals, item, ctx.st);
                    let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
                    let rt_func = match &ty {
                        IrType::Int(_) => RuntimeFunc::PrintInt,
                        IrType::Float(_) => RuntimeFunc::PrintReal,
                        IrType::Bool => RuntimeFunc::PrintLogical,
                        IrType::Ptr(_) => RuntimeFunc::PrintString,
                        _ => RuntimeFunc::PrintInt,
                    };
                    if matches!(rt_func, RuntimeFunc::PrintString) {
                        let len = string_literal_len(item);
                        let len_val = b.const_i64(len);
                        b.runtime_call(RuntimeFunc::PrintString, vec![val, len_val], IrType::Void);
                    } else {
                        b.runtime_call(rt_func, vec![val], IrType::Void);
                    }
                }
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
            insert_implicit_dealloc(b, &ctx.locals);
            if let Some(addr) = ctx.result_addr {
                let rv = b.load(addr);
                b.ret(Some(rv));
            } else {
                b.ret_void();
            }
        }

        Stmt::Stop { .. } => {
            insert_implicit_dealloc(b, &ctx.locals);
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::ErrorStop { .. } => {
            insert_implicit_dealloc(b, &ctx.locals);
            b.runtime_call(RuntimeFunc::ErrorStop, vec![], IrType::Void);
            b.unreachable();
        }

        Stmt::Allocate { items, .. } => {
            for item in items {
                if let Expr::FunctionCall { callee, args } = &item.node {
                    let base_name = extract_base_name(callee);
                    if let Some(name) = base_name {
                        if let Some(info) = ctx.locals.get(&name.to_lowercase()).cloned() {
                            let elem_size_bytes: i64 = match &info.ty {
                                IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
                                IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
                                IrType::Bool => 4,
                                _ => 8,
                            };

                            if info.allocatable {
                                // Allocatable: call afs_allocate_1d for 1D, afs_allocate_array for multi-D.
                                // info.addr is the descriptor alloca.
                                let es = b.const_i64(elem_size_bytes);
                                if args.len() == 1 {
                                    let n = match &args[0].value {
                                        crate::ast::expr::SectionSubscript::Element(e) => {
                                            lower_expr(b, &ctx.locals, e, ctx.st)
                                        }
                                        _ => b.const_i64(1),
                                    };
                                    // Widen to i64 if needed.
                                    let n64 = if matches!(b.func().value_type(n), Some(IrType::Int(IntWidth::I32))) {
                                        b.int_extend(n, IntWidth::I64, true)
                                    } else { n };
                                    b.call(
                                        FuncRef::External("afs_allocate_1d".into()),
                                        vec![info.addr, es, n64],
                                        IrType::Void,
                                    );
                                } else {
                                    // Multi-D: compute each dim and call afs_allocate_array.
                                    // For now, fall back to computing total size and using simple allocate.
                                    let mut total: Option<ValueId> = None;
                                    for arg in args {
                                        let dim_size = match &arg.value {
                                            crate::ast::expr::SectionSubscript::Element(e) => {
                                                lower_expr(b, &ctx.locals, e, ctx.st)
                                            }
                                            _ => b.const_i32(1),
                                        };
                                        total = Some(match total {
                                            Some(prev) => b.imul(prev, dim_size),
                                            None => dim_size,
                                        });
                                    }
                                    let n = total.unwrap_or_else(|| b.const_i64(1));
                                    let n64 = if matches!(b.func().value_type(n), Some(IrType::Int(IntWidth::I32))) {
                                        b.int_extend(n, IntWidth::I64, true)
                                    } else { n };
                                    b.call(
                                        FuncRef::External("afs_allocate_1d".into()),
                                        vec![info.addr, es, n64],
                                        IrType::Void,
                                    );
                                }
                            } else {
                                // Non-allocatable array: old path (shouldn't happen for ALLOCATE).
                                let size_val = b.const_i32(elem_size_bytes as i32);
                                let ptr = b.runtime_call(
                                    RuntimeFunc::Allocate,
                                    vec![size_val],
                                    IrType::Ptr(Box::new(info.ty.clone())),
                                );
                                b.store(ptr, info.addr);
                            }
                        }
                    }
                }
            }
        }

        Stmt::Deallocate { items, .. } => {
            for item in items {
                let base_name = extract_base_name(item);
                if let Some(name) = base_name {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()) {
                        if info.allocatable {
                            // Pass descriptor address to runtime with null STAT.
                            // Alloca a dummy STAT to avoid abort on already-deallocated.
                            let stat_slot = b.alloca(IrType::Int(IntWidth::I32));
                            b.call(
                                FuncRef::External("afs_deallocate_array".into()),
                                vec![info.addr, stat_slot],
                                IrType::Void,
                            );
                        } else {
                            let ptr = b.load(info.addr);
                            b.runtime_call(RuntimeFunc::Deallocate, vec![ptr], IrType::Void);
                        }
                    }
                }
            }
        }

        Stmt::Block { body, .. } => {
            lower_stmts(b, ctx, body);
        }

        Stmt::Associate { assocs, body, .. } => {
            // Associate names are scoped — they only exist within the body.
            let added_keys: Vec<String> = assocs.iter()
                .map(|(name, _)| name.to_lowercase())
                .collect();

            for (name, expr) in assocs {
                let val = lower_expr(b, &ctx.locals, expr, ctx.st);
                let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
                let addr = b.alloca(ty.clone());
                b.store(val, addr);
                ctx.locals.insert(name.to_lowercase(), LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None });
            }
            lower_stmts(b, ctx, body);

            // Remove associate names from scope.
            for key in &added_keys {
                ctx.locals.remove(key);
            }
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
            ctx.locals.insert(key.clone(), LocalInfo { addr, ty: IrType::Int(IntWidth::I32), dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None });
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

        // Check: i <= end for positive step, i >= end for negative step.
        b.set_block(bb_check);
        let cur = b.load(var_addr);

        let const_step = step.as_ref().and_then(eval_const_int);
        if let Some(sv) = const_step {
            // Compile-time known step direction.
            let cmp_op = if sv < 0 { CmpOp::Ge } else { CmpOp::Le };
            let cond = b.icmp(cmp_op, cur, end_val);
            b.cond_branch(cond, bb_body, vec![], bb_exit, vec![]);
        } else {
            // Runtime step: check sign and use appropriate comparison.
            let zero = b.const_i32(0);
            let step_neg = b.icmp(CmpOp::Lt, step_val, zero);
            let bb_neg_check = b.create_block("do_neg_check");
            let bb_pos_check = b.create_block("do_pos_check");
            b.cond_branch(step_neg, bb_neg_check, vec![], bb_pos_check, vec![]);

            b.set_block(bb_neg_check);
            let cond_neg = b.icmp(CmpOp::Ge, cur, end_val);
            b.cond_branch(cond_neg, bb_body, vec![], bb_exit, vec![]);

            b.set_block(bb_pos_check);
            let cond_pos = b.icmp(CmpOp::Le, cur, end_val);
            b.cond_branch(cond_pos, bb_body, vec![], bb_exit, vec![]);
        }

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
    let base = array_base_addr(b, info);
    // Widen index to i64 for pointer arithmetic if needed.
    let idx64 = if info.allocatable {
        if matches!(b.func().value_type(idx), Some(IrType::Int(IntWidth::I32))) {
            b.int_extend(idx, IntWidth::I64, true)
        } else { idx }
    } else { idx };
    let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
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
    let base = array_base_addr(b, info);
    let idx64 = if info.allocatable {
        if matches!(b.func().value_type(idx), Some(IrType::Int(IntWidth::I32))) {
            b.int_extend(idx, IntWidth::I64, true)
        } else { idx }
    } else { idx };
    let elem_ptr = b.gep(base, vec![idx64], info.ty.clone());
    b.store(value, elem_ptr);
}

/// Get the data base address for an array variable.
/// For fixed arrays, this is the alloca address directly.
/// For allocatable arrays, load base_addr from the descriptor (offset 0).
fn array_base_addr(b: &mut FuncBuilder, info: &LocalInfo) -> ValueId {
    if info.allocatable {
        // Load base_addr (first 8 bytes of descriptor) as a pointer.
        // The descriptor alloca is [i8 x 384], but the first field is a pointer.
        b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
    } else {
        info.addr
    }
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
    // If it's a simple name, pass its address.
    if let Expr::Name { name } = &expr.node {
        let key = name.to_lowercase();
        if let Some(info) = locals.get(&key) {
            if info.by_ref {
                // Already a pointer to caller's storage — load and pass it.
                return b.load(info.addr);
            }
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
fn lower_expr(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
) -> ValueId {
    match &expr.node {
        Expr::IntegerLiteral { text, kind, .. } => {
            let val: i64 = text.parse().unwrap_or(0);
            let is_64bit = kind.as_ref().map(|k| k == "8").unwrap_or(false)
                || val > i32::MAX as i64
                || val < i32::MIN as i64;
            if is_64bit {
                b.const_i64(val)
            } else {
                b.const_i32(val as i32)
            }
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
                if !info.dims.is_empty() {
                    // Array name without subscripts — return the base address.
                    info.addr
                } else if info.by_ref {
                    // Pass-by-reference param: load the pointer, then load through it.
                    let ptr = b.load(info.addr);
                    b.load(ptr)
                } else {
                    b.load(info.addr)
                }
            } else {
                b.const_i32(0) // implicit or unknown
            }
        }

        Expr::BinaryOp { op, left, right } => {
            let mut lhs = lower_expr(b, locals, left, st);
            let mut rhs = lower_expr(b, locals, right, st);
            let lty = b.func().value_type(lhs).unwrap_or(IrType::Int(IntWidth::I32));
            let rty = b.func().value_type(rhs).unwrap_or(IrType::Int(IntWidth::I32));

            // Implicit type promotion: if one side is int and the other float,
            // convert the int to float (Fortran mixed-mode arithmetic).
            let result_ty = if lty.is_float() || rty.is_float() {
                let fw = match (&lty, &rty) {
                    (IrType::Float(FloatWidth::F64), _) | (_, IrType::Float(FloatWidth::F64)) => FloatWidth::F64,
                    _ => FloatWidth::F32,
                };
                if lty.is_int() { lhs = b.int_to_float(lhs, fw); }
                if rty.is_int() { rhs = b.int_to_float(rhs, fw); }
                // Promote f32 to f64 if other is f64.
                if matches!(lty, IrType::Float(FloatWidth::F32)) && fw == FloatWidth::F64 {
                    lhs = b.float_extend(lhs, FloatWidth::F64);
                }
                if matches!(rty, IrType::Float(FloatWidth::F32)) && fw == FloatWidth::F64 {
                    rhs = b.float_extend(rhs, FloatWidth::F64);
                }
                IrType::Float(fw)
            } else {
                lty.clone()
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
                (BinaryOp::And, _) => b.and(lhs, rhs),
                (BinaryOp::Or, _) => b.or(lhs, rhs),
                (BinaryOp::Eqv, _) => {
                    // a .eqv. b = .not. (a .xor. b)
                    let both = b.and(lhs, rhs);
                    let either = b.or(lhs, rhs);
                    let not_both = b.not(both);
                    let xor = b.and(either, not_both);
                    b.not(xor)
                }
                (BinaryOp::Neqv, _) => {
                    // a .neqv. b = a .xor. b
                    let both = b.and(lhs, rhs);
                    let either = b.or(lhs, rhs);
                    let not_both = b.not(both);
                    b.and(either, not_both)
                }
                (BinaryOp::Concat, _) => {
                    b.runtime_call(RuntimeFunc::StringConcat, vec![lhs, rhs],
                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))))
                }
                _ => b.iadd(lhs, rhs), // fallback for defined ops
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
                // Fixed arrays have dims set; allocatable arrays have allocatable flag.
                if let Some(info) = locals.get(&key) {
                    if !info.dims.is_empty() || info.allocatable {
                        return lower_array_element(b, locals, info, args, st);
                    }
                }

                // Try intrinsic lowering first (intrinsics use values, not references).
                let intrinsic_arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => lower_expr(b, locals, e, st),
                        _ => b.const_i32(0),
                    }
                }).collect();

                if let Some(result) = lower_intrinsic(b, &key, &intrinsic_arg_vals) {
                    return result;
                }

                // User function call: Fortran pass-by-reference — pass addresses.
                let ref_arg_vals: Vec<ValueId> = args.iter().map(|a| {
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => lower_arg_by_ref(b, locals, e, st),
                        _ => b.const_i32(0),
                    }
                }).collect();

                // Look up callee return type from symbol table.
                // Search all scopes since the current scope may be global after resolve.
                let callee_sym = st.scopes.iter()
                    .find_map(|scope| scope.symbols.get(&key));
                let ret_ty = callee_sym
                    .and_then(|sym| sym.type_info.as_ref())
                    .map(|info| crate::sema::types::type_info_to_fortran_type(info))
                    .map(|ft| match ft {
                        crate::sema::types::FortranType::Real { kind } => IrType::float_from_kind(kind),
                        crate::sema::types::FortranType::Integer { kind } => IrType::int_from_kind(kind),
                        crate::sema::types::FortranType::Logical { .. } => IrType::Bool,
                        _ => IrType::Int(IntWidth::I32),
                    })
                    .unwrap_or(IrType::Int(IntWidth::I32));
                b.call(FuncRef::External(name.clone()), ref_arg_vals, ret_ty)
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
        // Two loops means 2 blocks named "do_check_N":
        let label_count = ir.matches("do_check_").count();
        assert!(label_count >= 2, "expected at least 2 loop headers, got {} in:\n{}", label_count, ir);
    }

    #[test]
    fn lower_do_negative_step() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  integer :: i, s
  s = 0
  do i = 10, 1, -1
    s = s + i
  end do
end program
");
        // Negative step should use >= comparison, not <=.
        assert!(ir.contains("icmp ge"), "expected 'icmp ge' for negative step in:\n{}", ir);
    }

    #[test]
    fn lower_function_return() {
        let (_, ir) = lower_and_verify("\
function square(x) result(y)
  integer, intent(in) :: x
  integer :: y
  y = x * x
  return
end function
");
        // RETURN should load the result variable and ret it, not ret void.
        assert!(ir.contains("ret %"), "expected 'ret %value' in:\n{}", ir);
        assert!(!ir.contains("ret void"), "function should not ret void in:\n{}", ir);
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

    // ---- Allocatable / strings ----

    #[test]
    fn lower_allocate_deallocate() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  real, allocatable :: a(:)
  allocate(a(100))
  deallocate(a)
end program
");
        assert!(ir.contains("call @afs_allocate_1d"), "expected allocate call in:\n{}", ir);
        assert!(ir.contains("call @afs_deallocate_array"), "expected deallocate call in:\n{}", ir);
    }

    #[test]
    fn lower_implicit_dealloc_at_scope_exit() {
        let (_, ir) = lower_and_verify("\
subroutine foo()
  implicit none
  real, allocatable :: temp(:)
  allocate(temp(10))
end subroutine
");
        // Should have implicit deallocation before ret.
        let dealloc_count = ir.matches("call @afs_deallocate_array").count();
        assert!(dealloc_count >= 1, "expected implicit deallocation, got {} in:\n{}", dealloc_count, ir);
    }

    #[test]
    fn lower_string_literal() {
        let (_, ir) = lower_and_verify("\
program test
  implicit none
  print *, 'hello'
end program
");
        assert!(ir.contains("const_string"), "expected string constant in:\n{}", ir);
        assert!(ir.contains("rt_call @__afs_print"));
    }

    // ---- Calls ----

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
