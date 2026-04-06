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
    /// Derived type name (for component access resolution). Empty for non-derived.
    derived_type: Option<String>,
}

/// Lowering context — tracks locals, loop scopes, and symbol table.
struct LowerCtx<'a> {
    locals: HashMap<String, LocalInfo>,
    loops: Vec<LoopScope>,
    st: &'a SymbolTable,
    globals: &'a HashMap<String, (usize, IrType)>,
    type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry,
    /// For functions: address of the result variable (for RETURN).
    result_addr: Option<ValueId>,
    /// For functions: the return type.
    result_type: Option<IrType>,
}

impl<'a> LowerCtx<'a> {
    fn new(st: &'a SymbolTable, globals: &'a HashMap<String, (usize, IrType)>, type_layouts: &'a crate::sema::type_layout::TypeLayoutRegistry) -> Self {
        Self { locals: HashMap::new(), loops: Vec::new(), st, globals, type_layouts, result_addr: None, result_type: None }
    }

    fn insert_scalar(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
    }

    fn insert_param_by_ref(&mut self, name: String, addr: ValueId, ty: IrType) {
        self.locals.insert(name, LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: true, char_kind: CharKind::None, derived_type: None });
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
    type_layouts: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Module {
    let mut module = Module::new("main".into());
    let globals = HashMap::new();
    for unit in units {
        lower_unit(&mut module, unit, st, &globals, type_layouts);
    }
    module
}

fn lower_unit(module: &mut Module, unit: &SpannedUnit, st: &SymbolTable, globals: &HashMap<String, (usize, IrType)>, type_layouts: &crate::sema::type_layout::TypeLayoutRegistry) {
    match &unit.node {
        ProgramUnit::Program { name, decls, body, contains, .. } => {
            let fname = name.clone().unwrap_or_else(|| "main".into());
            let mut func = Function::new(fname, vec![], IrType::Void);
            let mut ctx = LowerCtx::new(st, globals, type_layouts);

            {
                let mut b = FuncBuilder::new(&mut func);
                alloc_decls(&mut b, &mut ctx.locals, decls, type_layouts);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);

            // Lower CONTAINS subprograms.
            for sub in contains {
                lower_unit(module, sub, st, globals, type_layouts);
            }
        }
        ProgramUnit::Subroutine { name, decls, body, args, bind, .. } => {
            // BIND(C): use specified C name, otherwise use Fortran name.
            let func_name = bind.as_ref()
                .map(|b| b.name.as_deref().unwrap_or(name).trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_else(|| name.clone());
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    let elem_ty = arg_type_from_decls(n, decls);
                    if arg_has_value_attr(n, decls) {
                        // VALUE: pass by value (raw type, not pointer).
                        Some(Param { name: n.clone(), ty: elem_ty, id: ValueId(i as u32) })
                    } else {
                        Some(Param { name: n.clone(), ty: IrType::Ptr(Box::new(elem_ty)), id: ValueId(i as u32) })
                    }
                } else { None }
            }).collect();
            let mut func = Function::new(func_name, params, IrType::Void);
            let mut ctx = LowerCtx::new(st, globals, type_layouts);

            // Collect param info: (name, param_id, elem_type, is_value).
            let param_info: Vec<(String, ValueId, IrType, bool)> = func.params.iter()
                .map(|p| {
                    let is_ptr = matches!(&p.ty, IrType::Ptr(_));
                    let elem_ty = match &p.ty {
                        IrType::Ptr(inner) => (**inner).clone(),
                        other => other.clone(),
                    };
                    (p.name.to_lowercase(), p.id, elem_ty, !is_ptr)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);

                for (pname, pid, elem_ty, is_value) in &param_info {
                    if *is_value {
                        let slot = b.alloca(elem_ty.clone());
                        b.store(*pid, slot);
                        ctx.insert_scalar(pname.clone(), slot, elem_ty.clone());
                    } else {
                        let slot = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                        b.store(*pid, slot);
                        // Check if this is a derived type parameter.
                        let dt_name = arg_derived_type_name(pname, decls);
                        let info = LocalInfo {
                            addr: slot, ty: elem_ty.clone(),
                            dims: vec![], allocatable: false, by_ref: true,
                            char_kind: CharKind::None, derived_type: dt_name,
                        };
                        ctx.locals.insert(pname.clone(), info);
                    }
                }

                alloc_decls(&mut b, &mut ctx.locals, decls, type_layouts);
                lower_stmts(&mut b, &mut ctx, body);
                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts);
                }
                ensure_termination(&mut b, None);
            }

            module.add_function(func);
        }
        ProgramUnit::Function { name, decls, body, args, result, return_type, bind, .. } => {
            let func_name = bind.as_ref()
                .map(|b| b.name.as_deref().unwrap_or(name).trim_matches('\'').trim_matches('"').to_string())
                .unwrap_or_else(|| name.clone());
            let ret_ty = return_type.as_ref()
                .map(lower_type_spec)
                .unwrap_or_else(|| {
                    let result_name = result.as_deref().unwrap_or(name.as_str());
                    arg_type_from_decls(result_name, decls)
                });
            let params: Vec<Param> = args.iter().enumerate().filter_map(|(i, arg)| {
                if let DummyArg::Name(n) = arg {
                    let elem_ty = arg_type_from_decls(n, decls);
                    if arg_has_value_attr(n, decls) {
                        Some(Param { name: n.clone(), ty: elem_ty, id: ValueId(i as u32) })
                    } else {
                        Some(Param { name: n.clone(), ty: IrType::Ptr(Box::new(elem_ty)), id: ValueId(i as u32) })
                    }
                } else { None }
            }).collect();
            let mut func = Function::new(func_name, params, ret_ty.clone());
            let mut ctx = LowerCtx::new(st, globals, type_layouts);

            let param_info: Vec<(String, ValueId, IrType, bool)> = func.params.iter()
                .map(|p| {
                    let is_ptr = matches!(&p.ty, IrType::Ptr(_));
                    let elem_ty = match &p.ty {
                        IrType::Ptr(inner) => (**inner).clone(),
                        other => other.clone(),
                    };
                    (p.name.to_lowercase(), p.id, elem_ty, !is_ptr)
                })
                .collect();

            {
                let mut b = FuncBuilder::new(&mut func);

                for (pname, pid, elem_ty, is_value) in &param_info {
                    if *is_value {
                        let slot = b.alloca(elem_ty.clone());
                        b.store(*pid, slot);
                        ctx.insert_scalar(pname.clone(), slot, elem_ty.clone());
                    } else {
                        let slot = b.alloca(IrType::Ptr(Box::new(elem_ty.clone())));
                        b.store(*pid, slot);
                        let dt_name = arg_derived_type_name(pname, decls);
                        ctx.locals.insert(pname.clone(), LocalInfo {
                            addr: slot, ty: elem_ty.clone(),
                            dims: vec![], allocatable: false, by_ref: true,
                            char_kind: CharKind::None, derived_type: dt_name,
                        });
                    }
                }

                let result_name = result.as_deref().unwrap_or(name.as_str()).to_lowercase();
                let result_addr = b.alloca(ret_ty.clone());
                ctx.insert_scalar(result_name, result_addr, ret_ty.clone());
                ctx.result_addr = Some(result_addr);
                ctx.result_type = Some(ret_ty.clone());

                alloc_decls(&mut b, &mut ctx.locals, decls, type_layouts);
                lower_stmts(&mut b, &mut ctx, body);

                if b.func().block(b.current_block()).terminator.is_none() {
                    insert_implicit_dealloc(&mut b, &ctx.locals, type_layouts);
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
fn alloc_decls(b: &mut FuncBuilder, locals: &mut HashMap<String, LocalInfo>, decls: &[crate::ast::decl::SpannedDecl], type_layouts: &crate::sema::type_layout::TypeLayoutRegistry) {
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
                        char_kind: CharKind::Deferred, derived_type: None,
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
                            char_kind: CharKind::Fixed(len), derived_type: None,
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
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: true, by_ref: false, char_kind: CharKind::None, derived_type: None });
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
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: true, by_ref: false, char_kind: CharKind::None, derived_type: None });
                    } else {
                        // Small array: stack allocation.
                        let arr_ty = IrType::Array(Box::new(elem_ty.clone()), total_size as u64);
                        let addr = b.alloca(arr_ty);
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims, allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
                    }
                } else if let TypeSpec::Type(ref type_name) = type_spec {
                    // Derived type variable: allocate struct-sized byte array.
                    if let Some(layout) = type_layouts.get(type_name) {
                        let struct_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), layout.size as u64);
                        let addr = b.alloca(struct_ty);
                        // Store the derived type name in the ty field for component access lookup.
                        // Use Ptr<i8> as a marker — the type_layouts registry is used for field resolution.
                        locals.insert(key, LocalInfo {
                            addr,
                            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))),
                            dims: vec![],
                            allocatable: false,
                            by_ref: false,
                            char_kind: CharKind::None,
                            derived_type: Some(type_name.clone()),
                        });
                    } else {
                        // Unknown derived type — fall back to 8-byte alloca.
                        let addr = b.alloca(IrType::Int(IntWidth::I64));
                        locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
                    }
                } else {
                    // Scalar variable.
                    let addr = b.alloca(elem_ty.clone());
                    locals.insert(key, LocalInfo { addr, ty: elem_ty.clone(), dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
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
        "mod" => {
            // MOD(a, p) = a - INT(a/p) * p  (sign of dividend)
            // C-style remainder matches this.
            if args.len() >= 2 {
                Some(b.imod(args[0], args[1]))
            } else { None }
        }
        "modulo" => {
            // MODULO(a, p) = a - FLOOR(a/p) * p  (sign of divisor, result in [0, |p|))
            // For integers: if result has opposite sign to p, add p.
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    // Float modulo: use fmod then adjust.
                    let rem = b.call(FuncRef::External("fmod".into()), vec![args[0], args[1]], ty.clone());
                    let sum = b.fadd(rem, args[1]);
                    let rem2 = b.call(FuncRef::External("fmod".into()), vec![sum, args[1]], ty);
                    Some(rem2)
                } else {
                    // Integer modulo: rem = a % p; if (rem != 0 && (rem ^ p) < 0) rem += p
                    let rem = b.imod(args[0], args[1]);
                    let zero = match &ty {
                        IrType::Int(IntWidth::I64) => b.const_i64(0),
                        _ => b.const_i32(0),
                    };
                    let rem_ne_zero = b.icmp(CmpOp::Ne, rem, zero);
                    let rem_xor_p = b.bit_xor(rem, args[1]);
                    let sign_differs = b.icmp(CmpOp::Lt, rem_xor_p, zero);
                    let needs_adjust = b.and(rem_ne_zero, sign_differs);
                    let adjusted = b.iadd(rem, args[1]);
                    Some(b.select(needs_adjust, adjusted, rem))
                }
            } else { None }
        }
        "abs" | "iabs" | "dabs" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                match &ty {
                    IrType::Int(w) => {
                        let zero = match w {
                            IntWidth::I64 => b.const_i64(0),
                            _ => b.const_i32(0),
                        };
                        let is_pos = b.icmp(CmpOp::Ge, *arg, zero);
                        let neg = b.ineg(*arg);
                        Some(b.select(is_pos, *arg, neg))
                    }
                    IrType::Float(_) => Some(b.fabs(*arg)),
                    _ => None,
                }
            } else { None }
        }
        "int" | "idint" | "ifix" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_float() {
                    Some(b.float_to_int(*arg, IntWidth::I32))
                } else {
                    Some(*arg)
                }
            } else { None }
        }
        "nint" | "idnint" => {
            // NINT: round to nearest integer (not truncate).
            // Round via libm round(), then convert to int.
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F64));
                if ty.is_float() {
                    let func = if matches!(ty, IrType::Float(FloatWidth::F32)) { "roundf" } else { "round" };
                    let rounded = b.call(FuncRef::External(func.into()), vec![*arg], ty.clone());
                    Some(b.float_to_int(rounded, IntWidth::I32))
                } else {
                    Some(*arg)
                }
            } else { None }
        }
        "anint" | "dnint" => {
            // ANINT: round to nearest whole number, return as real.
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F64));
                let func = if matches!(ty, IrType::Float(FloatWidth::F32)) { "roundf" } else { "round" };
                Some(b.call(FuncRef::External(func.into()), vec![*arg], ty))
            } else { None }
        }
        "real" | "float" | "sngl" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_int() {
                    Some(b.int_to_float(*arg, FloatWidth::F32))
                } else {
                    Some(*arg)
                }
            } else { None }
        }
        "dble" | "dfloat" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                if ty.is_int() {
                    Some(b.int_to_float(*arg, FloatWidth::F64))
                } else if matches!(ty, IrType::Float(FloatWidth::F32)) {
                    Some(b.float_extend(*arg, FloatWidth::F64))
                } else {
                    Some(*arg)
                }
            } else { None }
        }
        "max" | "max0" | "amax1" | "dmax1" => {
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Int(IntWidth::I32));
                let cmp = if ty.is_float() {
                    b.fcmp(CmpOp::Ge, args[0], args[1])
                } else {
                    b.icmp(CmpOp::Ge, args[0], args[1])
                };
                let mut result = b.select(cmp, args[0], args[1]);
                // Variadic: max(a, b, c, ...) chains.
                for arg in &args[2..] {
                    let cmp = if ty.is_float() {
                        b.fcmp(CmpOp::Ge, result, *arg)
                    } else {
                        b.icmp(CmpOp::Ge, result, *arg)
                    };
                    result = b.select(cmp, result, *arg);
                }
                Some(result)
            } else { None }
        }
        "min" | "min0" | "amin1" | "dmin1" => {
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Int(IntWidth::I32));
                let cmp = if ty.is_float() {
                    b.fcmp(CmpOp::Le, args[0], args[1])
                } else {
                    b.icmp(CmpOp::Le, args[0], args[1])
                };
                let mut result = b.select(cmp, args[0], args[1]);
                for arg in &args[2..] {
                    let cmp = if ty.is_float() {
                        b.fcmp(CmpOp::Le, result, *arg)
                    } else {
                        b.icmp(CmpOp::Le, result, *arg)
                    };
                    result = b.select(cmp, result, *arg);
                }
                Some(result)
            } else { None }
        }
        "sign" | "dsign" | "isign" => {
            // sign(a, b) = abs(a) * sign_of(b) = b >= 0 ? abs(a) : -abs(a)
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Int(IntWidth::I32));
                let abs_a = if ty.is_float() {
                    b.fabs(args[0])
                } else {
                    let zero = match &ty {
                        IrType::Int(IntWidth::I64) => b.const_i64(0),
                        _ => b.const_i32(0),
                    };
                    let is_pos = b.icmp(CmpOp::Ge, args[0], zero);
                    let neg = b.ineg(args[0]);
                    b.select(is_pos, args[0], neg)
                };
                let neg_abs = if ty.is_float() { b.fneg(abs_a) } else { b.ineg(abs_a) };
                let zero = match &ty {
                    IrType::Float(FloatWidth::F32) => b.const_f32(0.0),
                    IrType::Float(_) => b.const_f64(0.0),
                    IrType::Int(IntWidth::I64) => b.const_i64(0),
                    _ => b.const_i32(0),
                };
                let b_pos = if ty.is_float() {
                    b.fcmp(CmpOp::Ge, args[1], zero)
                } else {
                    b.icmp(CmpOp::Ge, args[1], zero)
                };
                Some(b.select(b_pos, abs_a, neg_abs))
            } else { None }
        }
        "sqrt" | "dsqrt" => {
            args.first().map(|a| b.fsqrt(*a))
        }
        // ---- Bit manipulation (inline) ----
        "iand" => {
            if args.len() >= 2 { Some(b.bit_and(args[0], args[1])) } else { None }
        }
        "ior" => {
            if args.len() >= 2 { Some(b.bit_or(args[0], args[1])) } else { None }
        }
        "ieor" => {
            if args.len() >= 2 { Some(b.bit_xor(args[0], args[1])) } else { None }
        }
        "not" => {
            args.first().map(|a| b.bit_not(*a))
        }
        "leadz" => {
            args.first().map(|a| b.clz(*a))
        }
        "trailz" => {
            args.first().map(|a| b.ctz(*a))
        }
        "popcount" | "popcnt" => {
            // Use __builtin_popcountll via runtime call since ARM64 NEON popcount
            // requires a complex instruction sequence.
            args.first().map(|a| {
                let widened = b.int_extend(*a, IntWidth::I64, false);
                b.call(FuncRef::External("afs_popcount".into()), vec![widened], IrType::Int(IntWidth::I32))
            })
        }
        "ishft" => {
            // ishft(a, shift): positive shift = left, negative = right.
            // For now, only handle positive (left shift). Full impl needs Select.
            if args.len() >= 2 {
                let zero = b.const_i32(0);
                let is_left = b.icmp(CmpOp::Ge, args[1], zero);
                let neg_shift = b.ineg(args[1]);
                let left = b.shl(args[0], args[1]);
                let right = b.lshr(args[0], neg_shift);
                Some(b.select(is_left, left, right))
            } else { None }
        }
        "btest" => {
            // btest(a, pos) = (a >> pos) & 1 /= 0
            if args.len() >= 2 {
                let shifted = b.lshr(args[0], args[1]);
                let one = b.const_i32(1);
                let masked = b.bit_and(shifted, one);
                let zero = b.const_i32(0);
                Some(b.icmp(CmpOp::Ne, masked, zero))
            } else { None }
        }
        "ibset" => {
            // ibset(a, pos) = a | (1 << pos)
            if args.len() >= 2 {
                let one = b.const_i32(1);
                let mask = b.shl(one, args[1]);
                Some(b.bit_or(args[0], mask))
            } else { None }
        }
        "ibclr" => {
            // ibclr(a, pos) = a & ~(1 << pos)
            if args.len() >= 2 {
                let one = b.const_i32(1);
                let mask = b.shl(one, args[1]);
                let inv = b.bit_not(mask);
                Some(b.bit_and(args[0], inv))
            } else { None }
        }
        "ibits" => {
            // ibits(i, pos, len) = (i >> pos) & ((1 << len) - 1)
            if args.len() >= 3 {
                let shifted = b.lshr(args[0], args[1]);
                let one = b.const_i32(1);
                let mask_hi = b.shl(one, args[2]);
                let one2 = b.const_i32(1);
                let mask = b.isub(mask_hi, one2);
                Some(b.bit_and(shifted, mask))
            } else { None }
        }
        // ---- Math intrinsics → libm calls ----
        // Dispatch to sinf/sin based on argument type for F32/F64 correctness.
        "sin" | "dsin" | "cos" | "dcos" | "tan" | "dtan" |
        "asin" | "dasin" | "acos" | "dacos" | "atan" | "datan" |
        "sinh" | "dsinh" | "cosh" | "dcosh" | "tanh" | "dtanh" |
        "exp" | "dexp" | "log" | "dlog" | "alog" |
        "log10" | "dlog10" | "alog10" |
        "erf" | "derf" | "erfc" | "derfc" |
        "ceiling" | "floor" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let base_name = match name {
                    "dsin" | "sin" => "sin",
                    "dcos" | "cos" => "cos",
                    "dtan" | "tan" => "tan",
                    "dasin" | "asin" => "asin",
                    "dacos" | "acos" => "acos",
                    "datan" | "atan" => "atan",
                    "dsinh" | "sinh" => "sinh",
                    "dcosh" | "cosh" => "cosh",
                    "dtanh" | "tanh" => "tanh",
                    "dexp" | "exp" => "exp",
                    "dlog" | "log" | "alog" => "log",
                    "dlog10" | "log10" | "alog10" => "log10",
                    "derf" | "erf" => "erf",
                    "derfc" | "erfc" => "erfc",
                    "ceiling" => "ceil",
                    "floor" => "floor",
                    _ => name,
                };
                let func_name = if is_f32 { format!("{}f", base_name) } else { base_name.to_string() };
                let ret_ty = if is_f32 { IrType::Float(FloatWidth::F32) } else { IrType::Float(FloatWidth::F64) };
                Some(b.call(FuncRef::External(func_name), vec![*arg], ret_ty))
            } else { None }
        }
        "atan2" | "datan2" => {
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "atan2f" } else { "atan2" };
                let ret_ty = if is_f32 { IrType::Float(FloatWidth::F32) } else { IrType::Float(FloatWidth::F64) };
                Some(b.call(FuncRef::External(func.into()), vec![args[0], args[1]], ret_ty))
            } else { None }
        }
        "gamma" | "dgamma" => {
            args.first().map(|a| {
                let ty = b.func().value_type(*a).unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "tgammaf" } else { "tgamma" };
                let ret_ty = if is_f32 { IrType::Float(FloatWidth::F32) } else { IrType::Float(FloatWidth::F64) };
                b.call(FuncRef::External(func.into()), vec![*a], ret_ty)
            })
        }
        "log_gamma" => {
            args.first().map(|a| {
                let ty = b.func().value_type(*a).unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "lgammaf" } else { "lgamma" };
                let ret_ty = if is_f32 { IrType::Float(FloatWidth::F32) } else { IrType::Float(FloatWidth::F64) };
                b.call(FuncRef::External(func.into()), vec![*a], ret_ty)
            })
        }
        "bessel_j0" => {
            args.first().map(|a| b.call(FuncRef::External("j0".into()), vec![*a], IrType::Float(FloatWidth::F64)))
        }
        "bessel_j1" => {
            args.first().map(|a| b.call(FuncRef::External("j1".into()), vec![*a], IrType::Float(FloatWidth::F64)))
        }
        "bessel_y0" => {
            args.first().map(|a| b.call(FuncRef::External("y0".into()), vec![*a], IrType::Float(FloatWidth::F64)))
        }
        "bessel_y1" => {
            args.first().map(|a| b.call(FuncRef::External("y1".into()), vec![*a], IrType::Float(FloatWidth::F64)))
        }
        "hypot" => {
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Float(FloatWidth::F64));
                let is_f32 = matches!(ty, IrType::Float(FloatWidth::F32));
                let func = if is_f32 { "hypotf" } else { "hypot" };
                let ret_ty = if is_f32 { IrType::Float(FloatWidth::F32) } else { IrType::Float(FloatWidth::F64) };
                Some(b.call(FuncRef::External(func.into()), vec![args[0], args[1]], ret_ty))
            } else { None }
        }
        "ishftc" => {
            // ishftc(a, shift, size): circular shift of the rightmost `size` bits.
            if args.len() >= 2 {
                let ty = b.func().value_type(args[0]).unwrap_or(IrType::Int(IntWidth::I32));
                let default_size = match &ty {
                    IrType::Int(IntWidth::I64) => 64,
                    IrType::Int(IntWidth::I16) => 16,
                    IrType::Int(IntWidth::I8) => 8,
                    _ => 32,
                };
                let size = if args.len() >= 3 { args[2] } else { b.const_i32(default_size) };
                let shift = args[1];
                // left = (a << shift) | (a >> (size - shift)), masked to size bits.
                let left = b.shl(args[0], shift);
                let diff = b.isub(size, shift);
                let right = b.lshr(args[0], diff);
                let combined = b.bit_or(left, right);
                // Mask to `size` bits: combined & ((1 << size) - 1).
                let one = b.const_i32(1);
                let shifted_one = b.shl(one, size);
                let one2 = b.const_i32(1);
                let mask = b.isub(shifted_one, one2);
                Some(b.bit_and(combined, mask))
            } else { None }
        }

        // ---- Numeric inquiry intrinsics (compile-time constants) ----
        // These depend on the argument's type, which we determine from the first arg.
        "huge" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                match &ty {
                    IrType::Int(IntWidth::I8) => Some(b.const_i32(i8::MAX as i64 as i32)),
                    IrType::Int(IntWidth::I16) => Some(b.const_i32(i16::MAX as i64 as i32)),
                    IrType::Int(IntWidth::I32) => Some(b.const_i32(i32::MAX)),
                    IrType::Int(IntWidth::I64) => Some(b.const_i64(i64::MAX)),
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::MAX)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::MAX)),
                    _ => None,
                }
            } else { None }
        }
        "tiny" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F32));
                match &ty {
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::MIN_POSITIVE)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::MIN_POSITIVE)),
                    _ => None,
                }
            } else { None }
        }
        "epsilon" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F32));
                match &ty {
                    IrType::Float(FloatWidth::F32) => Some(b.const_f32(f32::EPSILON)),
                    IrType::Float(FloatWidth::F64) => Some(b.const_f64(f64::EPSILON)),
                    _ => None,
                }
            } else { None }
        }
        "precision" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Float(FloatWidth::F32));
                let prec = match &ty {
                    IrType::Float(FloatWidth::F32) => 6,  // ~7.2 decimal digits → 6
                    IrType::Float(FloatWidth::F64) => 15, // ~15.9 decimal digits → 15
                    _ => 0,
                };
                Some(b.const_i32(prec))
            } else { None }
        }
        "range" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                let range = match &ty {
                    IrType::Int(IntWidth::I8) => 2,
                    IrType::Int(IntWidth::I16) => 4,
                    IrType::Int(IntWidth::I32) => 9,
                    IrType::Int(IntWidth::I64) => 18,
                    IrType::Float(FloatWidth::F32) => 37,
                    IrType::Float(FloatWidth::F64) => 307,
                    _ => 0,
                };
                Some(b.const_i32(range))
            } else { None }
        }
        "digits" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                let digits = match &ty {
                    IrType::Int(IntWidth::I8) => 7,
                    IrType::Int(IntWidth::I16) => 15,
                    IrType::Int(IntWidth::I32) => 31,
                    IrType::Int(IntWidth::I64) => 63,
                    IrType::Float(FloatWidth::F32) => 24,  // significand bits
                    IrType::Float(FloatWidth::F64) => 53,
                    _ => 0,
                };
                Some(b.const_i32(digits))
            } else { None }
        }
        "radix" => {
            // Always 2 for binary machines.
            Some(b.const_i32(2))
        }
        "bit_size" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                let bits = match &ty {
                    IrType::Int(IntWidth::I8) => 8,
                    IrType::Int(IntWidth::I16) => 16,
                    IrType::Int(IntWidth::I32) => 32,
                    IrType::Int(IntWidth::I64) => 64,
                    _ => 0,
                };
                Some(b.const_i32(bits))
            } else { None }
        }
        "kind" => {
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                let kind = match &ty {
                    IrType::Int(IntWidth::I8) => 1,
                    IrType::Int(IntWidth::I16) => 2,
                    IrType::Int(IntWidth::I32) => 4,
                    IrType::Int(IntWidth::I64) => 8,
                    IrType::Float(FloatWidth::F32) => 4,
                    IrType::Float(FloatWidth::F64) => 8,
                    IrType::Bool => 4,
                    _ => 4,
                };
                Some(b.const_i32(kind))
            } else { None }
        }
        // ---- System inquiry functions ----
        "command_argument_count" => {
            Some(b.call(FuncRef::External("afs_command_argument_count".into()), vec![], IrType::Int(IntWidth::I32)))
        }

        // ---- iso_c_binding functions ----
        "c_loc" => {
            // c_loc(x) — return address of x. The arg is already passed by reference,
            // so the arg value IS the address.
            args.first().copied()
        }
        "c_sizeof" => {
            // c_sizeof(x) — return byte size of x's C representation.
            if let Some(arg) = args.first() {
                let ty = b.func().value_type(*arg).unwrap_or(IrType::Int(IntWidth::I32));
                let size: i64 = match &ty {
                    IrType::Int(IntWidth::I8) | IrType::Bool => 1,
                    IrType::Int(IntWidth::I16) => 2,
                    IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
                    IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
                    IrType::Ptr(_) => 8, // pointers are 8 bytes on ARM64
                    // Arrays use element size * count, but we don't have shape info here.
                    // For now, return element size. Proper impl needs descriptor access.
                    IrType::Array(elem, count) => {
                        let elem_size = match elem.as_ref() {
                            IrType::Int(IntWidth::I8) => 1,
                            IrType::Int(IntWidth::I16) => 2,
                            IrType::Int(IntWidth::I32) | IrType::Float(FloatWidth::F32) => 4,
                            IrType::Int(IntWidth::I64) | IrType::Float(FloatWidth::F64) => 8,
                            _ => 4,
                        };
                        elem_size * (*count as i64)
                    }
                    _ => 8, // default to pointer size for unknown types
                };
                Some(b.const_i64(size))
            } else { None }
        }
        "c_associated" => {
            // c_associated(p) → p /= null
            // c_associated(p, q) → p == q
            if args.len() >= 2 {
                Some(b.icmp(CmpOp::Eq, args[0], args[1]))
            } else if let Some(p) = args.first() {
                // Use type-matched zero to avoid register width mismatch.
                let ty = b.func().value_type(*p).unwrap_or(IrType::Int(IntWidth::I64));
                let null = match &ty {
                    IrType::Int(IntWidth::I32) => b.const_i32(0),
                    _ => b.const_i64(0),
                };
                Some(b.icmp(CmpOp::Ne, *p, null))
            } else { None }
        }

        _ => None,
    }
}

/// Lower an intrinsic subroutine call (CALL system_clock, CALL date_and_time, etc.).
/// Returns true if the name was recognized and lowered, false otherwise.
fn lower_intrinsic_subroutine(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    name: &str,
    args: &[crate::ast::expr::Argument],
) -> bool {
    /// Helper: get the nth positional arg as a by-ref pointer, or null if absent.
    fn nth_arg_ref(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[crate::ast::expr::Argument],
        n: usize,
    ) -> ValueId {
        if n < args.len() {
            if let crate::ast::expr::SectionSubscript::Element(e) = &args[n].value {
                return lower_arg_by_ref(b, &ctx.locals, e, ctx.st);
            }
        }
        b.const_i64(0) // null pointer for missing optional arg
    }

    /// Helper: get the nth positional arg as a by-value expression, or default.
    fn nth_arg_val(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[crate::ast::expr::Argument],
        n: usize,
        default: i32,
    ) -> ValueId {
        if n < args.len() {
            if let crate::ast::expr::SectionSubscript::Element(e) = &args[n].value {
                return lower_expr(b, &ctx.locals, e, ctx.st);
            }
        }
        b.const_i32(default)
    }

    /// Helper: get the nth positional arg as a (ptr, len) string pair, or (null, 0).
    fn nth_arg_str(
        b: &mut FuncBuilder,
        ctx: &LowerCtx,
        args: &[crate::ast::expr::Argument],
        n: usize,
    ) -> (ValueId, ValueId) {
        if n < args.len() {
            if let crate::ast::expr::SectionSubscript::Element(e) = &args[n].value {
                // Check if it's a character variable — pass ptr+len.
                if let Expr::Name { name } = &e.node {
                    if let Some(info) = ctx.locals.get(&name.to_lowercase()) {
                        if info.char_kind != CharKind::None {
                            return lower_string_expr(b, &ctx.locals, e, ctx.st);
                        }
                    }
                }
                // Otherwise pass as ref + zero length.
                let ptr = lower_arg_by_ref(b, &ctx.locals, e, ctx.st);
                let zero = b.const_i64(0);
                return (ptr, zero);
            }
        }
        let z = b.const_i64(0);
        (z, z)
    }

    match name {
        "system_clock" => {
            // call system_clock(count, count_rate, count_max) — all optional
            let count = nth_arg_ref(b, ctx, args, 0);
            let rate = nth_arg_ref(b, ctx, args, 1);
            let max = nth_arg_ref(b, ctx, args, 2);
            b.call(FuncRef::External("afs_system_clock".into()), vec![count, rate, max], IrType::Void);
            true
        }
        "cpu_time" => {
            let time = nth_arg_ref(b, ctx, args, 0);
            b.call(FuncRef::External("afs_cpu_time".into()), vec![time], IrType::Void);
            true
        }
        "date_and_time" => {
            // call date_and_time(date, time, zone, values) — all optional strings/array
            // Runtime: afs_date_and_time(date_buf, date_len, time_buf, time_len, zone_buf, zone_len, values)
            let (date_ptr, date_len) = nth_arg_str(b, ctx, args, 0);
            let (time_ptr, time_len) = nth_arg_str(b, ctx, args, 1);
            let (zone_ptr, zone_len) = nth_arg_str(b, ctx, args, 2);
            let values = nth_arg_ref(b, ctx, args, 3);
            b.call(FuncRef::External("afs_date_and_time".into()),
                vec![date_ptr, date_len, time_ptr, time_len, zone_ptr, zone_len, values],
                IrType::Void);
            true
        }
        "get_command_argument" => {
            // call get_command_argument(number, value, length, status)
            // Runtime: afs_get_command_argument(number, value, value_len, length, status)
            let number = nth_arg_val(b, ctx, args, 0, 0);
            let (val_ptr, val_len) = nth_arg_str(b, ctx, args, 1);
            let length = nth_arg_ref(b, ctx, args, 2);
            let status = nth_arg_ref(b, ctx, args, 3);
            b.call(FuncRef::External("afs_get_command_argument".into()),
                vec![number, val_ptr, val_len, length, status],
                IrType::Void);
            true
        }
        "command_argument_count" => {
            // This is a function, not a subroutine — handled in lower_intrinsic.
            false
        }
        "get_command" => {
            // call get_command(command, length, status)
            let (cmd_ptr, cmd_len) = nth_arg_str(b, ctx, args, 0);
            let length = nth_arg_ref(b, ctx, args, 1);
            let status = nth_arg_ref(b, ctx, args, 2);
            b.call(FuncRef::External("afs_get_command".into()),
                vec![cmd_ptr, cmd_len, length, status],
                IrType::Void);
            true
        }
        "get_environment_variable" => {
            // call get_environment_variable(name, value, length, status)
            // Runtime: afs_get_environment_variable(name, name_len, value, value_len, length, status)
            let (name_ptr, name_len) = nth_arg_str(b, ctx, args, 0);
            let (val_ptr, val_len) = nth_arg_str(b, ctx, args, 1);
            let length = nth_arg_ref(b, ctx, args, 2);
            let status = nth_arg_ref(b, ctx, args, 3);
            b.call(FuncRef::External("afs_get_environment_variable".into()),
                vec![name_ptr, name_len, val_ptr, val_len, length, status],
                IrType::Void);
            true
        }
        "random_number" => {
            let harvest = nth_arg_ref(b, ctx, args, 0);
            b.call(FuncRef::External("afs_random_number_f64".into()), vec![harvest], IrType::Void);
            true
        }
        "random_seed" => {
            let seed = nth_arg_val(b, ctx, args, 0, 0);
            let widened = b.int_extend(seed, IntWidth::I64, true);
            b.call(FuncRef::External("afs_random_seed".into()), vec![widened], IrType::Void);
            true
        }
        "execute_command_line" => {
            let (cmd_ptr, cmd_len) = nth_arg_str(b, ctx, args, 0);
            let wait = nth_arg_val(b, ctx, args, 1, 1);
            let exitstat = nth_arg_ref(b, ctx, args, 2);
            let cmdstat = nth_arg_ref(b, ctx, args, 3);
            b.call(FuncRef::External("afs_execute_command_line".into()),
                vec![cmd_ptr, cmd_len, wait, exitstat, cmdstat],
                IrType::Void);
            true
        }

        // ---- iso_c_binding subroutines ----
        "c_f_pointer" => {
            // call c_f_pointer(cptr, fptr [, shape])
            // Store the C pointer value into the Fortran pointer variable.
            // cptr is passed by value (it's a c_ptr), fptr is passed by reference.
            let cptr = nth_arg_val(b, ctx, args, 0, 0);
            let fptr = nth_arg_ref(b, ctx, args, 1);
            b.store(cptr, fptr);
            true
        }

        _ => false,
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

/// Check if a dummy argument is a derived type, returning the type name if so.
fn arg_derived_type_name(arg_name: &str, decls: &[crate::ast::decl::SpannedDecl]) -> Option<String> {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { type_spec, entities, .. } = &decl.node {
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    if let TypeSpec::Type(ref name) = type_spec {
                        return Some(name.clone());
                    }
                }
            }
        }
    }
    None
}

/// Check if a callee has VALUE-attributed arguments via its scope in the symbol table.
/// Returns a Vec<bool> per argument position — true if that arg is VALUE.
/// Returns None if callee scope not found or no VALUE args.
fn callee_value_arg_mask(st: &SymbolTable, callee_name: &str) -> Option<Vec<bool>> {
    use crate::sema::symtab::ScopeKind;
    let callee_scope = st.scopes.iter().find(|s| {
        match &s.kind {
            ScopeKind::Function(n) | ScopeKind::Subroutine(n) => n.to_lowercase() == callee_name,
            _ => false,
        }
    })?;
    if !callee_scope.symbols.values().any(|sym| sym.attrs.value) {
        return None;
    }
    // Use arg_order to build a positional mask.
    let mask: Vec<bool> = callee_scope.arg_order.iter().map(|arg_name| {
        callee_scope.symbols.get(arg_name)
            .map(|sym| sym.attrs.value)
            .unwrap_or(false)
    }).collect();
    Some(mask)
}

/// Check if a dummy argument has the VALUE attribute in its declaration.
fn arg_has_value_attr(arg_name: &str, decls: &[crate::ast::decl::SpannedDecl]) -> bool {
    let key = arg_name.to_lowercase();
    for decl in decls {
        if let Decl::TypeDecl { attrs, entities, .. } = &decl.node {
            for entity in entities {
                if entity.name.to_lowercase() == key {
                    return attrs.iter().any(|a| matches!(a, crate::ast::decl::Attribute::Value));
                }
            }
        }
    }
    false
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
                        // StringDescriptor layout: [data(8), len(8), capacity(8), flags(4)]
                        let ptr = b.load_typed(info.addr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                        // GEP with byte offset: use Ptr<i8> result so elem_size=1.
                        let eight = b.const_i64(8);
                        let len_ptr = b.gep(info.addr, vec![eight], IrType::Int(IntWidth::I8));
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
        Expr::BinaryOp { op: BinaryOp::Concat, left, right } => {
            // Concatenation: get both sides as (ptr, len), allocate temp, call afs_concat.
            let (a_ptr, a_len) = lower_string_expr(b, locals, left, st);
            let (b_ptr, b_len) = lower_string_expr(b, locals, right, st);
            let total_len = b.iadd(a_len, b_len);
            // Allocate temp buffer for the result.
            let result_buf = b.runtime_call(RuntimeFunc::Allocate, vec![total_len], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
            // Call afs_concat(result, a, a_len, b, b_len).
            b.call(
                FuncRef::External("afs_concat".into()),
                vec![result_buf, a_ptr, a_len, b_ptr, b_len],
                IrType::Void,
            );
            (result_buf, total_len)
        }
        _ => {
            // For other expressions, evaluate as value and use literal length if available.
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
fn insert_implicit_dealloc(b: &mut FuncBuilder, locals: &HashMap<String, LocalInfo>, type_layouts: &crate::sema::type_layout::TypeLayoutRegistry) {
    let stat_addr = b.alloca(IrType::Int(IntWidth::I32));
    for info in locals.values() {
        if info.char_kind == CharKind::Deferred {
            b.call(
                FuncRef::External("afs_dealloc_string".into()),
                vec![info.addr],
                IrType::Void,
            );
        } else if info.allocatable {
            b.call(
                FuncRef::External("afs_deallocate_array".into()),
                vec![info.addr, stat_addr],
                IrType::Void,
            );
        }
        // Finalization: call FINAL procedures for locally-owned derived type variables.
        // Skip by-ref params (they're owned by the caller, not the callee).
        if !info.by_ref {
            if let Some(ref type_name) = info.derived_type {
                if let Some(layout) = type_layouts.get(type_name) {
                    for final_proc in &layout.final_procs {
                        b.call(FuncRef::External(final_proc.clone()), vec![info.addr], IrType::Void);
                    }
                }
            }
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
        TypeSpec::Type(_) | TypeSpec::Class(_) => {
            // Derived types are passed as byte pointers (struct layout resolved elsewhere).
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8)))
        }
        _ => IrType::Int(IntWidth::I32), // fallback
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
                                let val = lower_expr_tl(b, &ctx.locals, value, ctx.st, ctx.type_layouts);
                                if info.derived_type.is_some() {
                                    // Derived type assignment: memcpy from source to destination.
                                    let size = if let Some(ref tn) = info.derived_type {
                                        ctx.type_layouts.get(tn).map(|l| l.size).unwrap_or(8)
                                    } else { 8 };
                                    let size_val = b.const_i64(size as i64);
                                    b.call(FuncRef::External("memcpy".into()),
                                        vec![info.addr, val, size_val],
                                        IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                                } else if info.by_ref {
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
                Expr::ComponentAccess { base, component } => {
                    // x%field = val (supports chained: x%a%b = val).
                    if let Some((base_addr, type_name)) = resolve_component_base(b, &ctx.locals, base, ctx.type_layouts) {
                        if let Some(layout) = ctx.type_layouts.get(&type_name) {
                            if let Some(field) = layout.field(component) {
                                let val = lower_expr_tl(b, &ctx.locals, value, ctx.st, ctx.type_layouts);
                                let offset = b.const_i64(field.offset as i64);
                                let field_ptr = b.gep(base_addr, vec![offset],
                                    IrType::Int(IntWidth::I8));
                                b.store(val, field_ptr);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Stmt::Print { items, .. } => {
            // PRINT * → unit 6 (stdout).
            let unit = b.const_i32(6);
            lower_write_items(b, ctx, items, unit);
        }

        Stmt::Write { controls, items } => {
            // Extract unit (first control). * means stdout (unit 6).
            let unit = if let Some(ctrl) = controls.first() {
                if matches!(&ctrl.value.node, Expr::Name { name } if name == "*") {
                    b.const_i32(6)
                } else {
                    lower_expr(b, &ctx.locals, &ctrl.value, ctx.st)
                }
            } else {
                b.const_i32(6)
            };

            // Check for format specifier (second positional control).
            // * means list-directed; a string literal means formatted.
            let fmt_control = controls.iter().skip(1)
                .find(|c| c.keyword.is_none())  // positional, not keyword=
                .or_else(|| controls.iter().find(|c| c.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("fmt")).unwrap_or(false)));

            let is_list_directed = match fmt_control {
                None => true,
                Some(ctrl) => matches!(&ctrl.value.node, Expr::Name { name } if name == "*"),
            };

            // Check for ADVANCE='NO'.
            let advance = controls.iter()
                .find(|c| c.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("advance")).unwrap_or(false))
                .map(|c| {
                    if let Expr::StringLiteral { value, .. } = &c.value.node {
                        !value.eq_ignore_ascii_case("no")
                    } else { true }
                })
                .unwrap_or(true);

            if is_list_directed {
                lower_write_items_adv(b, ctx, items, unit, advance);
            } else {
                // Formatted I/O: use push-based API.
                let (fmt_ptr, fmt_len) = lower_string_expr(b, &ctx.locals, &fmt_control.unwrap().value, ctx.st);
                b.call(FuncRef::External("afs_fmt_begin".into()), vec![unit, fmt_ptr, fmt_len], IrType::Void);

                for item in items {
                    lower_fmt_push(b, ctx, item);
                }

                let adv = b.const_i32(if advance { 1 } else { 0 });
                b.call(FuncRef::External("afs_fmt_end".into()), vec![adv], IrType::Void);
            }
        }

        Stmt::Call { callee, args } => {
            // Handle type-bound procedure calls: call obj%method(args)
            if let Expr::ComponentAccess { base, component } = &callee.node {
                if let Some((obj_addr, type_name)) = resolve_component_base_for_method(b, &ctx.locals, base, ctx.type_layouts) {
                    if let Some(layout) = ctx.type_layouts.get(&type_name) {
                        if let Some(bp) = layout.bound_proc(component) {
                            let target = bp.target_name.clone();
                            let nopass = bp.nopass;

                            // Build argument list: obj as first arg (PASS), then explicit args.
                            let mut call_args = Vec::new();
                            if !nopass {
                                call_args.push(obj_addr); // PASS: object address
                            }
                            for a in args {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &a.value {
                                    call_args.push(lower_arg_by_ref(b, &ctx.locals, e, ctx.st));
                                }
                            }
                            b.call(FuncRef::External(target), call_args, IrType::Void);
                        }
                    }
                }
            } else if let Expr::Name { name } = &callee.node {
                let key = name.to_lowercase();

                // Try intrinsic subroutine lowering first.
                if !lower_intrinsic_subroutine(b, ctx, &key, args) {
                    // Not an intrinsic — general subroutine call.
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
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts);
            if let Some(addr) = ctx.result_addr {
                let rv = b.load(addr);
                b.ret(Some(rv));
            } else {
                b.ret_void();
            }
        }

        Stmt::Stop { .. } => {
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts);
            b.runtime_call(RuntimeFunc::Stop, vec![], IrType::Void);
            b.unreachable();
        }
        Stmt::ErrorStop { .. } => {
            insert_implicit_dealloc(b, &ctx.locals, ctx.type_layouts);
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
                ctx.locals.insert(name.to_lowercase(), LocalInfo { addr, ty, dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
            }
            lower_stmts(b, ctx, body);

            // Remove associate names from scope.
            for key in &added_keys {
                ctx.locals.remove(key);
            }
        }

        Stmt::Continue { .. } => {} // no-op

        Stmt::Open { specs } => {
            // Extract UNIT and FILE from specs. Simplified: first spec is unit, second is file.
            let unit = if let Some(s) = specs.first() {
                lower_expr(b, &ctx.locals, &s.value, ctx.st)
            } else { b.const_i32(6) };

            // Find FILE= spec.
            let (file_ptr, file_len) = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("file")).unwrap_or(false))
                .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

            // Find STATUS= spec.
            let (status_ptr, status_len) = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("status")).unwrap_or(false))
                .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

            // Find ACTION= spec.
            let (action_ptr, action_len) = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("action")).unwrap_or(false))
                .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

            // Find ACCESS= spec.
            let (access_ptr, access_len) = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("access")).unwrap_or(false))
                .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

            // Find FORM= spec.
            let (form_ptr, form_len) = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("form")).unwrap_or(false))
                .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

            // Find RECL= spec.
            let recl_val = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("recl")).unwrap_or(false))
                .map(|s| lower_expr(b, &ctx.locals, &s.value, ctx.st))
                .unwrap_or_else(|| b.const_i64(0));

            let null = b.const_i64(0);

            // Check if we have any extended specifiers beyond the basic 7-arg set.
            let has_access = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("access")).unwrap_or(false));
            let has_form = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("form")).unwrap_or(false));
            let has_recl = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("recl")).unwrap_or(false));
            let has_position = specs.iter().any(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("position")).unwrap_or(false));

            if !has_access && !has_form && !has_recl && !has_position {
                // Simple case: use 7-arg afs_open_simple (unit + 3 string pairs).
                b.call(
                    FuncRef::External("afs_open_simple".into()),
                    vec![unit, file_ptr, file_len, status_ptr, status_len, action_ptr, action_len],
                    IrType::Void,
                );
            } else {
                // Extended case: build OpenControlBlock on the stack.
                // Find POSITION= spec.
                let (position_ptr, position_len) = specs.iter()
                    .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("position")).unwrap_or(false))
                    .map(|s| lower_string_expr(b, &ctx.locals, &s.value, ctx.st))
                    .unwrap_or_else(|| { let z = b.const_i64(0); (z, z) });

                // Layout matches repr(C) OpenControlBlock (128 bytes):
                //   0: unit(i32) + 4 pad, 8: filename(ptr), 16: filename_len(i64),
                //  24: status(ptr), 32: status_len(i64), 40: action(ptr), 48: action_len(i64),
                //  56: access(ptr), 64: access_len(i64), 72: form(ptr), 80: form_len(i64),
                //  88: recl(i64), 96: iostat(ptr), 104: newunit(ptr),
                // 112: position(ptr), 120: position_len(i64)
                let cb_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), 128);
                let cb = b.alloca(cb_ty);

                let store_at = |b: &mut crate::ir::builder::FuncBuilder, base, offset: i64, val| {
                    let off = b.const_i64(offset);
                    let ptr = b.gep(base, vec![off], IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));
                    b.store(val, ptr);
                };

                store_at(b, cb, 0, unit);
                store_at(b, cb, 8, file_ptr);
                store_at(b, cb, 16, file_len);
                store_at(b, cb, 24, status_ptr);
                store_at(b, cb, 32, status_len);
                store_at(b, cb, 40, action_ptr);
                store_at(b, cb, 48, action_len);
                store_at(b, cb, 56, access_ptr);
                store_at(b, cb, 64, access_len);
                store_at(b, cb, 72, form_ptr);
                store_at(b, cb, 80, form_len);
                store_at(b, cb, 88, recl_val);
                store_at(b, cb, 96, null);       // iostat = null
                store_at(b, cb, 104, null);      // newunit = null
                store_at(b, cb, 112, position_ptr);
                store_at(b, cb, 120, position_len);

                b.call(
                    FuncRef::External("afs_open".into()),
                    vec![cb],
                    IrType::Void,
                );
            }
        }

        Stmt::Close { specs } => {
            let unit = if let Some(s) = specs.first() {
                lower_expr(b, &ctx.locals, &s.value, ctx.st)
            } else { b.const_i32(6) };
            let null = b.const_i64(0);
            b.call(FuncRef::External("afs_close".into()), vec![unit, null], IrType::Void);
        }

        Stmt::Read { controls, items } => {
            // READ(unit, *) items — simplified: first control is unit.
            let unit = if let Some(ctrl) = controls.first() {
                lower_expr(b, &ctx.locals, &ctrl.value, ctx.st)
            } else {
                b.const_i32(5) // default stdin
            };
            let null = b.const_i64(0); // null IOSTAT
            for item in items {
                if let Expr::Name { name } = &item.node {
                    let key = name.to_lowercase();
                    if let Some(info) = ctx.locals.get(&key) {
                        let addr = if info.by_ref {
                            b.load(info.addr)
                        } else {
                            info.addr
                        };
                        let ty = &info.ty;
                        let func_name = match ty {
                            IrType::Int(IntWidth::I64) => "afs_read_int64",
                            IrType::Int(_) => "afs_read_int",
                            IrType::Float(FloatWidth::F64) => "afs_read_real64",
                            IrType::Float(_) => "afs_read_real",
                            _ => "afs_read_int",
                        };
                        b.call(FuncRef::External(func_name.into()), vec![unit, addr, null], IrType::Void);
                    }
                }
            }
        }

        Stmt::Inquire { specs, .. } => {
            // Simplified INQUIRE: extract UNIT or FILE, and EXIST.
            let null = b.const_i64(0);
            let file_spec = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("file")).unwrap_or(false));
            let exist_spec = specs.iter()
                .find(|s| s.keyword.as_deref().map(|k| k.eq_ignore_ascii_case("exist")).unwrap_or(false));

            if let Some(fs) = file_spec {
                let (fptr, flen) = lower_string_expr(b, &ctx.locals, &fs.value, ctx.st);
                let exist_addr = if let Some(es) = exist_spec {
                    if let Expr::Name { name } = &es.value.node {
                        ctx.locals.get(&name.to_lowercase()).map(|i| i.addr).unwrap_or(null)
                    } else { null }
                } else { null };
                b.call(FuncRef::External("afs_inquire_file".into()),
                    vec![fptr, flen, exist_addr, null, null], IrType::Void);
            }
        }

        Stmt::Flush { specs } => {
            let unit = if let Some(s) = specs.first() {
                lower_expr(b, &ctx.locals, &s.value, ctx.st)
            } else { b.const_i32(6) };
            let null = b.const_i64(0);
            b.call(FuncRef::External("afs_flush".into()), vec![unit, null], IrType::Void);
        }

        Stmt::Rewind { specs } => {
            let unit = if let Some(s) = specs.first() {
                lower_expr(b, &ctx.locals, &s.value, ctx.st)
            } else { b.const_i32(6) };
            let null = b.const_i64(0);
            b.call(FuncRef::External("afs_rewind".into()), vec![unit, null], IrType::Void);
        }

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
            ctx.locals.insert(key.clone(), LocalInfo { addr, ty: IrType::Int(IntWidth::I32), dims: vec![], allocatable: false, by_ref: false, char_kind: CharKind::None, derived_type: None });
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

/// Lower the items of a PRINT/WRITE statement to unit-based I/O calls.
fn lower_write_items(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    unit: ValueId,
) {
    lower_write_items_adv(b, ctx, items, unit, true);
}

fn lower_write_items_adv(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    items: &[crate::ast::expr::SpannedExpr],
    unit: ValueId,
    advance: bool,
) {
    for item in items {
        let is_char = if let Expr::Name { name } = &item.node {
            ctx.locals.get(&name.to_lowercase())
                .map(|i| i.char_kind != CharKind::None)
                .unwrap_or(false)
        } else {
            false
        };

        if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
            let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
            b.call(FuncRef::External("afs_write_string".into()), vec![unit, ptr, len], IrType::Void);
        } else {
            let val = lower_expr_tl(b, &ctx.locals, item, ctx.st, ctx.type_layouts);
            let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
            let func_name = match &ty {
                IrType::Int(IntWidth::I64) => "afs_write_int64",
                IrType::Int(_) => "afs_write_int",
                IrType::Float(FloatWidth::F64) => "afs_write_real64",
                IrType::Float(_) => "afs_write_real",
                IrType::Bool => "afs_write_logical",
                IrType::Ptr(_) => {
                    // Pointer type — likely a string. Use write_string with literal length.
                    let len = string_literal_len(item);
                    let len_val = b.const_i64(len);
                    b.call(FuncRef::External("afs_write_string".into()), vec![unit, val, len_val], IrType::Void);
                    continue;
                }
                _ => "afs_write_int",
            };
            b.call(FuncRef::External(func_name.into()), vec![unit, val], IrType::Void);
        }
    }
    if advance {
        b.call(FuncRef::External("afs_write_newline".into()), vec![unit], IrType::Void);
    }
}

/// Push a single I/O item value for formatted output via afs_fmt_push_*.
fn lower_fmt_push(
    b: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    item: &crate::ast::expr::SpannedExpr,
) {
    let is_char = if let Expr::Name { name } = &item.node {
        ctx.locals.get(&name.to_lowercase())
            .map(|i| i.char_kind != CharKind::None)
            .unwrap_or(false)
    } else {
        false
    };

    if is_char || matches!(item.node, Expr::StringLiteral { .. }) {
        let (ptr, len) = lower_string_expr(b, &ctx.locals, item, ctx.st);
        b.call(FuncRef::External("afs_fmt_push_string".into()), vec![ptr, len], IrType::Void);
    } else {
        let val = lower_expr(b, &ctx.locals, item, ctx.st);
        let ty = b.func().value_type(val).unwrap_or(IrType::Int(IntWidth::I32));
        match &ty {
            IrType::Int(IntWidth::I64) => {
                b.call(FuncRef::External("afs_fmt_push_int".into()), vec![val], IrType::Void);
            }
            IrType::Int(_) => {
                // Widen i32 to i64 for the push API.
                let widened = b.int_extend(val, IntWidth::I64, true);
                b.call(FuncRef::External("afs_fmt_push_int".into()), vec![widened], IrType::Void);
            }
            IrType::Float(_) => {
                // afs_fmt_push_real takes f64; f32 is promoted by calling convention.
                b.call(FuncRef::External("afs_fmt_push_real".into()), vec![val], IrType::Void);
            }
            IrType::Bool => {
                let int_val = b.int_extend(val, IntWidth::I32, false);
                b.call(FuncRef::External("afs_fmt_push_logical".into()), vec![int_val], IrType::Void);
            }
            IrType::Ptr(_) => {
                // Pointer type — likely a string.
                let len = string_literal_len(item);
                let len_val = b.const_i64(len);
                b.call(FuncRef::External("afs_fmt_push_string".into()), vec![val, len_val], IrType::Void);
            }
            _ => {
                let widened = b.int_extend(val, IntWidth::I64, true);
                b.call(FuncRef::External("afs_fmt_push_int".into()), vec![widened], IrType::Void);
            }
        }
    }
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
/// Convert TypeInfo to IR type for field loads.
fn type_info_to_ir_type(ti: &crate::sema::symtab::TypeInfo) -> IrType {
    use crate::sema::symtab::TypeInfo;
    let (size, _) = crate::sema::type_layout::size_of_type(ti);
    match size {
        1 => IrType::Int(IntWidth::I8),
        2 => IrType::Int(IntWidth::I16),
        4 => match ti {
            TypeInfo::Real { .. } => IrType::Float(FloatWidth::F32),
            TypeInfo::Logical { .. } => IrType::Bool,
            _ => IrType::Int(IntWidth::I32),
        },
        8 => match ti {
            TypeInfo::Real { .. } | TypeInfo::DoublePrecision => IrType::Float(FloatWidth::F64),
            _ => IrType::Int(IntWidth::I64),
        },
        _ => IrType::Int(IntWidth::I32),
    }
}

/// Resolve a component access base expression to (struct_address, type_name).
/// Handles both direct names (x%field) and chained access (x%inner%field).
fn resolve_component_base(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    base: &crate::ast::expr::SpannedExpr,
    tl: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<(ValueId, String)> {
    match &base.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            let info = locals.get(&key)?;
            let type_name = info.derived_type.as_ref()?.clone();
            let addr = if info.by_ref { b.load(info.addr) } else { info.addr };
            Some((addr, type_name))
        }
        Expr::ComponentAccess { base: inner_base, component } => {
            // Recursive: resolve the inner base first.
            let (inner_addr, inner_type) = resolve_component_base(b, locals, inner_base, tl)?;
            let layout = tl.get(&inner_type)?;
            let field = layout.field(component)?;
            let offset = b.const_i64(field.offset as i64);
            let field_ptr = b.gep(inner_addr, vec![offset], IrType::Int(IntWidth::I8));
            // The field must be a derived type for chaining to continue.
            if let crate::sema::symtab::TypeInfo::Derived(ref nested_type) = field.type_info {
                Some((field_ptr, nested_type.clone()))
            } else {
                None // Terminal field — caller should load, not chain further.
            }
        }
        _ => None,
    }
}

/// Resolve a base expression for a type-bound procedure call.
/// Returns (object_address, type_name) — the address of the base object.
/// For simple `obj%method()`, base is `obj` → returns (obj.addr, obj.type).
/// For `obj%inner%method()`, base is `obj%inner` → returns (inner.addr, inner.type).
fn resolve_component_base_for_method(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    base: &crate::ast::expr::SpannedExpr,
    tl: &crate::sema::type_layout::TypeLayoutRegistry,
) -> Option<(ValueId, String)> {
    match &base.node {
        Expr::Name { name } => {
            let key = name.to_lowercase();
            let info = locals.get(&key)?;
            let type_name = info.derived_type.as_ref()?.clone();
            let addr = if info.by_ref { b.load(info.addr) } else { info.addr };
            Some((addr, type_name))
        }
        Expr::ComponentAccess { base: inner_base, component } => {
            // Resolve the inner base, then GEP to the component field.
            let (inner_addr, inner_type) = resolve_component_base_for_method(b, locals, inner_base, tl)?;
            let layout = tl.get(&inner_type)?;
            let field = layout.field(component)?;
            let offset = b.const_i64(field.offset as i64);
            let field_ptr = b.gep(inner_addr, vec![offset], IrType::Int(IntWidth::I8));
            if let crate::sema::symtab::TypeInfo::Derived(ref nested_type) = field.type_info {
                Some((field_ptr, nested_type.clone()))
            } else {
                None
            }
        }
        _ => None,
    }
}

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
    lower_expr_full(b, locals, expr, st, None)
}

fn lower_expr_tl(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    tl: &crate::sema::type_layout::TypeLayoutRegistry,
) -> ValueId {
    lower_expr_full(b, locals, expr, st, Some(tl))
}

fn lower_expr_full(
    b: &mut FuncBuilder,
    locals: &HashMap<String, LocalInfo>,
    expr: &crate::ast::expr::SpannedExpr,
    st: &SymbolTable,
    type_layouts: Option<&crate::sema::type_layout::TypeLayoutRegistry>,
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

                // Check if this is a structure constructor: type_name(val1, val2, ...).
                if let Some(tl) = type_layouts {
                    if let Some(layout) = tl.get(&key) {
                        // Allocate a temporary struct on the stack and zero-initialize.
                        let struct_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I8)), layout.size as u64);
                        let tmp = b.alloca(struct_ty);
                        let zero = b.const_i32(0);
                        let sz = b.const_i64(layout.size as i64);
                        b.call(FuncRef::External("memset".into()), vec![tmp, zero, sz],
                            IrType::Ptr(Box::new(IrType::Int(IntWidth::I8))));

                        if args.len() != layout.fields.len() {
                            eprintln!("warning: structure constructor for '{}' has {} args but type has {} fields",
                                key, args.len(), layout.fields.len());
                        }

                        // Store each argument into the corresponding field.
                        for (i, arg) in args.iter().enumerate() {
                            if i < layout.fields.len() {
                                if let crate::ast::expr::SectionSubscript::Element(e) = &arg.value {
                                    let val = lower_expr_full(b, locals, e, st, type_layouts);
                                    let offset = b.const_i64(layout.fields[i].offset as i64);
                                    let field_ptr = b.gep(tmp, vec![offset], IrType::Int(IntWidth::I8));
                                    b.store(val, field_ptr);
                                }
                            }
                        }
                        return tmp;
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

                // Check if the callee has VALUE args (BIND(C) interface).
                let callee_value_args = callee_value_arg_mask(st, &key);

                // Pass args: by value for VALUE, by reference otherwise.
                let ref_arg_vals: Vec<ValueId> = args.iter().enumerate().map(|(i, a)| {
                    let is_value = callee_value_args.as_ref().map(|mask| i < mask.len() && mask[i]).unwrap_or(false);
                    match &a.value {
                        crate::ast::expr::SectionSubscript::Element(e) => {
                            if is_value {
                                lower_expr(b, locals, e, st)
                            } else {
                                lower_arg_by_ref(b, locals, e, st)
                            }
                        }
                        _ => b.const_i32(0),
                    }
                }).collect();

                // Look up callee return type from symbol table.
                // Search all scopes since the current scope may be global after resolve.
                let callee_sym = st.scopes.iter()
                    .find_map(|scope| scope.symbols.get(&key));
                let ret_ty = callee_sym
                    .and_then(|sym| sym.type_info.as_ref())
                    .map(crate::sema::types::type_info_to_fortran_type)
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

        Expr::ComponentAccess { base, component } => {
            if let Some(tl) = type_layouts {
                if let Some((base_addr, type_name)) = resolve_component_base(b, locals, base, tl) {
                    if let Some(layout) = tl.get(&type_name) {
                        if let Some(field) = layout.field(component) {
                            let offset = b.const_i64(field.offset as i64);
                            let field_ptr = b.gep(base_addr, vec![offset], IrType::Int(IntWidth::I8));

                            // If the field is itself a derived type, DON'T load — return the pointer
                            // (for chained access like x%inner%field).
                            if let crate::sema::symtab::TypeInfo::Derived(_) = &field.type_info {
                                return field_ptr;
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
        let (st, layouts) = resolve::resolve_file(&units).unwrap();
        lower_file(&units, &st, &layouts)
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
        assert!(ir.contains("afs_write_int"));
        assert!(ir.contains("afs_write_newline"));
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
        assert!(ir.contains("afs_write_int"));
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
        assert!(ir.contains("afs_write_string"));
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
